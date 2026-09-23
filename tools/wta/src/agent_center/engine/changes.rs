// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

use super::*;

impl Engine {
    pub(super) fn validate_replacement(
        &self,
        work: &Value,
        replacement: &ReplacementSpec,
    ) -> DomainResult<()> {
        if replacement.revision != number(work, "currentSpecRevision") {
            return Err(bad(
                "STALE_VERSION",
                "Replacement must name the current specification revision",
            ));
        }
        if replacement.project_id != text(work, "projectId")
            || encode(&replacement.delivery)? != work["spec"]["delivery"]
        {
            return Err(bad(
                "FORBIDDEN",
                "This change cannot migrate project, destination or delivery authority",
            ));
        }
        nonempty(&replacement.goal, "goal")?;
        if replacement.scope.is_empty() || replacement.criteria.is_empty() {
            return Err(bad(
                "INVALID_ARGUMENT",
                "Replacement requires concrete scope and criteria",
            ));
        }
        for scope in replacement.scope.iter().chain(&replacement.exclusions) {
            nonempty(scope, "scope/exclusions")?;
        }
        let mut keys = BTreeSet::new();
        for criterion in &replacement.criteria {
            nonempty(&criterion.id, "criterion.id")?;
            nonempty(&criterion.description, "criterion.description")?;
            nonempty(&criterion.evidence_rule, "criterion.evidenceRule")?;
            if !keys.insert(&criterion.id) {
                return Err(bad("INVALID_ARGUMENT", "Duplicate work criterion"));
            }
        }
        self.artifacts(&replacement.context)?;
        for reference in &replacement.context {
            let artifact = self.record(&reference.artifact_id, "Artifact")?;
            let source = self.record(text(&artifact, "workId"), "Work")?;
            if source["projectId"] != work["projectId"] {
                return Err(bad(
                    "FORBIDDEN",
                    "Replacement context is outside the approved project",
                ));
            }
        }
        for source in &replacement.source_message_ids {
            let message = self.record(source, "ConversationItem")?;
            if text(&message, "workId") == text(work, "id") {
                continue;
            }
            let conversation = self.record(text(&message, "conversationId"), "Conversation")?;
            if conversation["scope"] != "Global" && conversation["projectId"] != work["projectId"] {
                return Err(bad(
                    "FORBIDDEN",
                    "Source message belongs to another project",
                ));
            }
        }
        Ok(())
    }

    pub(super) fn change_command(
        &mut self,
        principal: &Principal,
        request: &Request,
    ) -> DomainResult<Response> {
        match request.method.as_str() {
            "work.propose_change" => {
                let input: WorkProposeChange = parse(&request.params)?;
                let work = self.record(&input.work_id, "Work")?;
                if !matches!(principal, Principal::Human | Principal::Service) {
                    self.proposal_coordinator(principal, &input.work_id)?;
                    for source in &input.replacement_spec.source_message_ids {
                        let message = self.record(source, "ConversationItem")?;
                        let global_source = message
                            .get("conversationId")
                            .and_then(Value::as_str)
                            .and_then(|id| self.records.get(id))
                            .is_some_and(|conversation| conversation["scope"] == "Global");
                        if global_source {
                            self.authorized_read(principal, &message)?;
                        }
                    }
                }
                self.matched(request, &[&work])?;
                if text(&work, "lifecycle") != "Active" {
                    return Err(bad(
                        "BAD_STATE",
                        "Specification changes require an active work",
                    ));
                }
                self.validate_replacement(&work, &input.replacement_spec)?;
                nonempty(&input.reason, "reason")?;
                let mut requested = BTreeSet::new();
                for task_id in &input.affected_task_ids {
                    let task = self.record(task_id, "Task")?;
                    if task["workId"] != work["id"] || !requested.insert(task_id) {
                        return Err(bad(
                            "INVALID_REFERENCE",
                            "Affected task must be unique and belong to this work",
                        ));
                    }
                }
                let tasks: Vec<_> = self
                    .related("Task", "workId", &input.work_id)
                    .iter()
                    .map(|task| task["id"].clone())
                    .collect();
                let impact = json!({"affectedTaskIds":tasks,"affectedCriteria":input.replacement_spec.criteria,
                    "invalidatedCandidateIds":work.get("currentCandidateId").into_iter().collect::<Vec<_>>(),
                    "carryForwardTaskIds":[],"authorityExpansion":false,"allowanceReset":false,
                    "requiresReplan":true});
                let proposal = self.create("ChangeProposal", json!({"workId":work["id"],
                    "basedOnSpecRevision":work["currentSpecRevision"],"basedOnPlanRevision":work["currentPlanRevision"],
                    "grantId":work["currentGrantId"],"replacementSpec":encode(&input.replacement_spec)?,
                    "affectedTaskIds":tasks,"reason":input.reason,"impact":impact,"status":"Proposed"}));
                self.emit("ChangeProposed", &proposal);
                self.attention(&input.work_id, text(&proposal,"id"), "ChangeApproval",
                    "Review the replacement brief and exact grant preview before applying this change");
                let mut response = Response::ok(
                    "",
                    json!({"proposalId":proposal["id"],"version":proposal["version"],
                    "impact":impact,"proposal":proposal}),
                );
                response.subjects = vec![Self::reference(&work), Self::reference(&proposal)];
                Ok(response)
            }
            "work.apply_change" => {
                Self::human(principal)?;
                let input: WorkApplyChange = parse(&request.params)?;
                let mut proposal = self.record(&input.proposal_id, "ChangeProposal")?;
                let work_id = text(&proposal, "workId").to_owned();
                let mut work = self.record(&work_id, "Work")?;
                self.matched(request, &[&work, &proposal])?;
                if text(&work, "lifecycle") != "Active" || work["requiresReplan"] == true {
                    return Err(bad(
                        "BAD_STATE",
                        "Apply the outstanding replan before another specification change",
                    ));
                }
                let replacement: ReplacementSpec = parse(&proposal["replacementSpec"])?;
                self.validate_replacement(&work, &replacement)?;
                if text(&proposal, "status") != "Proposed"
                    || proposal["basedOnPlanRevision"] != work["currentPlanRevision"]
                    || proposal["grantId"] != work["currentGrantId"]
                {
                    return Err(bad(
                        "STALE_VERSION",
                        "Change proposal no longer names the current plan and grant",
                    ));
                }
                let workspace = self.record(text(&work, "workspaceId"), "Workspace")?;
                if text(&workspace, "status") != "Ready" || text(&workspace, "writer") != "None" {
                    return Err(bad(
                        "BAD_STATE",
                        "Complete workspace handoff before applying a specification change",
                    ));
                }
                let project = self.record(text(&work, "projectId"), "Project")?;
                let mut previous_grant =
                    self.record(text(&work, "currentGrantId"), "ExecutionGrant")?;
                let mut preview = self.record(&input.grant_proposal_id, "GrantProposal")?;
                if text(&preview, "status") != "Proposed"
                    || preview["workId"] != work["id"]
                    || preview["specRevision"] != work["currentSpecRevision"]
                    || preview["policyRevision"] != project["policyRevision"]
                    || preview["basedOnGrantId"] != previous_grant["id"]
                {
                    return Err(bad(
                        "STALE_VERSION",
                        "Change approval requires a current exact grant preview",
                    ));
                }
                for field in [
                    "allowedCapabilities",
                    "dataScopes",
                    "writableResourceScopes",
                    "limits",
                ] {
                    if preview[field] != previous_grant[field] {
                        return Err(bad("FORBIDDEN","Specification revision cannot expand or reset approved execution authority"));
                    }
                }
                let revision = number(&work, "currentSpecRevision") + 1;
                let mut spec = encode(&replacement)?;
                spec["revision"] = json!(revision);
                if !self
                    .related("WorkSpec", "workId", &work_id)
                    .iter()
                    .any(|record| record["revision"] == work["currentSpecRevision"])
                {
                    self.create("WorkSpec",json!({"workId":work_id,"revision":work["currentSpecRevision"],"body":work["spec"]}));
                }
                let spec_record = self.create(
                    "WorkSpec",
                    json!({"workId":work_id,"revision":revision,"body":spec}),
                );
                previous_grant["status"] = json!("Superseded");
                self.put(previous_grant.clone());
                let mut grant_body = preview.clone();
                grant_body["specRevision"] = json!(revision);
                grant_body["issuedBy"] = json!(principal.key());
                grant_body["status"] = json!("Effective");
                grant_body["previousGrantId"] = previous_grant["id"].clone();
                let grant = self.create("ExecutionGrant", grant_body);
                preview["status"] = json!("Applied");
                self.put(preview);
                let mut stops = Vec::new();
                for mut invocation in self.related("Invocation", "workId", &work_id) {
                    if text(&invocation, "state") == "Released" {
                        continue;
                    }
                    invocation["specChangeId"] = proposal["id"].clone();
                    let invocation = self.put(invocation);
                    if text(&invocation, "state") != "Releasing" {
                        stops.push(self.stop_invocation(&invocation, "Hold")?);
                    }
                }
                for mut task in self.related("Task", "workId", &work_id) {
                    task["revision"] = json!(number(&task, "revision") + 1);
                    task["state"] = json!("NeedsReplan");
                    task["specChangeId"] = proposal["id"].clone();
                    for result in self.related("TaskResult", "taskId", text(&task, "id")) {
                        let mut result = result;
                        if text(&result, "disposition") != "Superseded" {
                            result["disposition"] = json!("Superseded");
                            result["invalidationReason"] = json!("SpecificationChanged");
                            self.put(result);
                        }
                    }
                    if let Some(object) = task.as_object_mut() {
                        for field in [
                            "currentResultId",
                            "acceptedResultId",
                            "currentAttemptId",
                            "reworkId",
                        ] {
                            object.remove(field);
                        }
                    }
                    let task = self.put(task);
                    self.resolve_attention(text(&task, "id"));
                }
                for kind in [
                    "EvaluationUnit",
                    "ContextRequest",
                    "DecisionRequest",
                    "PlanProposal",
                    "DeliveryCandidate",
                    "ChangeProposal",
                ] {
                    for mut record in self.related(kind, "workId", &work_id) {
                        if record["id"] == proposal["id"] {
                            continue;
                        }
                        let field = if kind == "EvaluationUnit" {
                            "state"
                        } else {
                            "status"
                        };
                        if [
                            "Queued",
                            "Running",
                            "Open",
                            "Answered",
                            "Proposed",
                            "Ready",
                            "PendingAcceptance",
                        ]
                        .contains(&text(&record, field))
                            || kind == "DeliveryCandidate"
                        {
                            record[field] = json!("Superseded");
                            let record = self.put(record);
                            self.resolve_attention(text(&record, "id"));
                        }
                    }
                }
                let operation = self.create("Operation",json!({"workId":work_id,"operationType":"work.apply_change",
                    "changeProposalId":proposal["id"],"status":"Running","stopOperationIds":stops,
                    "steps":[{"state":"Dispatched","intent":"Settle superseded execution before replanning"}]}));
                self.emit("OperationChanged", &operation);
                work["spec"] = spec_record["body"].clone();
                work["title"] = json!(replacement.goal);
                work["currentSpecRevision"] = json!(revision);
                work["currentSpecId"] = spec_record["id"].clone();
                work["currentGrantId"] = grant["id"].clone();
                work["requiresReplan"] = json!(true);
                work["specChangeId"] = proposal["id"].clone();
                work["changeOperationId"] = operation["id"].clone();
                work["headGeneration"] = json!(number(&work, "headGeneration") + 1);
                if let Some(object) = work.as_object_mut() {
                    for field in [
                        "currentCandidateId",
                        "currentAcceptanceId",
                        "integrationResultId",
                    ] {
                        object.remove(field);
                    }
                }
                let work = self.put(work);
                proposal["status"] = json!("Applied");
                proposal["appliedSpecRevision"] = json!(revision);
                proposal["operationId"] = operation["id"].clone();
                let proposal = self.put(proposal);
                self.resolve_attention(&input.proposal_id);
                self.emit("WorkSpecChanged", &work);
                self.emit("ChangeApplied", &proposal);
                let mut response = Response::pending(
                    "",
                    text(&operation, "id").into(),
                    json!({
                    "workId":work["id"],"version":work["version"],"specRevision":revision,
                    "settlementOperationIds":stops,"changeOperationId":operation["id"]}),
                );
                response.subjects = vec![Self::reference(&work), Self::reference(&proposal)];
                Ok(response)
            }
            _ => Err(bad("METHOD_UNSUPPORTED", &request.method)),
        }
    }

    pub(super) fn settle_spec_changes(&mut self) -> DomainResult<()> {
        for mut operation in self.all("Operation") {
            if text(&operation, "operationType") != "work.apply_change"
                || text(&operation, "status") != "Running"
            {
                continue;
            }
            let work_id = text(&operation, "workId").to_owned();
            if self
                .related("Invocation", "workId", &work_id)
                .iter()
                .any(|invocation| text(invocation, "state") != "Released")
            {
                continue;
            }
            operation["status"] = json!("Succeeded");
            operation["steps"] = json!([{"state":"Confirmed","intent":"Superseded execution released; replan queued"}]);
            let work = self.record(&work_id, "Work")?;
            operation["result"] = json!({"workId":work_id,"specRevision":work["currentSpecRevision"],
                "requiresReplan":true,"settled":true});
            let operation = self.put(operation);
            self.emit("OperationChanged", &operation);
            self.queue_coordination(Some(&work_id), None, "WorkSpecChanged", &operation)?;
        }
        Ok(())
    }
}
