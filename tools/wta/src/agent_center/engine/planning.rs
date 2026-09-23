// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

use super::*;

impl Engine {
    pub(super) fn planning_command(
        &mut self,
        principal: &Principal,
        request: &Request,
    ) -> DomainResult<Response> {
        match request.method.as_str() {
            "project.configure" => {
                Self::human(principal)?;
                self.matched(request, &[])?;
                let input: ProjectConfigure = parse(&request.params)?;
                nonempty(&input.name, "name")?;
                let create_directory = input.create_directory.unwrap_or(false);
                let canonical =
                    super::super::project_directory::resolve(&input.root, create_directory)
                        .map_err(|error| bad("INVALID_ARGUMENT", format!("{error:#}")))?;
                self.project_root_preference(&input, None)?;
                Self::validate_limits(&input.limits)?;
                for capability in [
                    &input.coordinator_capability_id,
                    &input.worker_capability_id,
                    &input.check_capability_id,
                ] {
                    nonempty(capability, "capabilityId")?;
                }
                for (capability, kind) in [
                    (&input.coordinator_capability_id, "Coordinate"),
                    (&input.worker_capability_id, "ProduceResult"),
                    (&input.check_capability_id, "EvaluateGate"),
                ] {
                    if self.runtime_for(capability, kind).is_none() {
                        return Err(bad("CAPABILITY_UNAVAILABLE",format!(
                            "Configure and register runtime capability {capability} for {kind} before approving this project"
                        )));
                    }
                }
                if input.project_id.is_some() {
                    return Err(bad(
                        "METHOD_UNSUPPORTED",
                        "In-place project policy replacement requires grant migration and is not supported",
                    ));
                }
                let mut capabilities = input.capability_ids.clone().unwrap_or_default();
                capabilities.extend([
                    input.coordinator_capability_id.clone(),
                    input.worker_capability_id.clone(),
                    input.check_capability_id.clone(),
                ]);
                capabilities.sort();
                capabilities.dedup();
                let mut body = encode(&input)?;
                body["root"] = json!(canonical.to_string_lossy());
                body["policyRevision"] = json!(1);
                body["capabilityIds"] = json!(capabilities);
                body["planningUsed"] = json!(0);
                body["environmentRefs"] = json!(input
                    .environment_refs
                    .unwrap_or_else(|| vec!["local-default".into()]));
                body["approvedContext"] = json!([]);
                body["experimental"] = json!(true);
                if create_directory {
                    body["id"] = json!(id());
                    body["kind"] = json!("Project");
                    body["createdAt"] = json!(utc_after(0));
                    let mut operation =
                        self.effect("project.create", json!({"project":body}), None);
                    if let Some(proposal) = self.human_action_for_request(request) {
                        operation["conversationId"] = proposal["conversationId"].clone();
                        operation = self.put(operation);
                        self.emit("OperationChanged", &operation);
                    }
                    return Ok(Response::pending(
                        "",
                        text(&operation, "id").into(),
                        json!({"projectId":body["id"],"state":"Creating"}),
                    ));
                }
                let project = self.create("Project", body);
                Ok(Response::ok(
                    "",
                    json!({"projectId":project["id"],"version":project["version"],"policyRevision":1,"project":project}),
                ))
            }
            "runtime.register" => {
                if !matches!(principal, Principal::Runtime { .. } | Principal::Service) {
                    return Err(bad(
                        "FORBIDDEN",
                        "Only an authenticated runtime can register",
                    ));
                }
                self.matched(request, &[])?;
                let input: RuntimeRegister = parse(&request.params)?;
                uuid(&input.runtime_instance_id)?;
                if !input.protocol_versions.contains(&1) {
                    return Err(bad("CAPABILITY_UNAVAILABLE", "Runtime does not support v1"));
                }
                let mut keys = BTreeSet::new();
                for capability in &input.capabilities {
                    if !keys.insert(&capability.id)
                        || capability.kinds.is_empty()
                        || !capability.supports_scoped_stop
                    {
                        return Err(bad(
                            "INVALID_ARGUMENT",
                            "Runtime capability must be unique and support scoped stop",
                        ));
                    }
                    for kind in &capability.kinds {
                        if ![
                            "ProduceResult",
                            "EvaluateGate",
                            "ReviewResult",
                            "Coordinate",
                            "ExecuteWork",
                        ]
                        .contains(&kind.as_str())
                        {
                            return Err(bad("INVALID_ARGUMENT", "Unknown capability kind"));
                        }
                    }
                }
                let runtime_id = match principal {
                    Principal::Runtime { runtime_id } => runtime_id.clone(),
                    _ => input.runtime_instance_id.clone(),
                };
                uuid(&runtime_id)?;
                let runtime = json!({"id":runtime_id,"kind":"Runtime","runtimeInstanceId":input.runtime_instance_id,
                    "capabilities":encode(&input.capabilities)?,"status":"Registered","createdAt":utc_after(0)});
                self.put(runtime);
                Ok(Response::ok(
                    "",
                    json!({"runtimeId":runtime_id,"runtimeBinding":runtime_id}),
                ))
            }
            "work.create_draft" => {
                if !matches!(principal, Principal::Human | Principal::Service) {
                    self.coordinator(principal, None)?;
                }
                self.matched(request, &[])?;
                let input: CreateDraft = parse(&request.params)?;
                let execution_mode = input.execution_mode.as_deref().unwrap_or("WorkExecutor");
                if !["WorkExecutor", "LegacyTasks"].contains(&execution_mode)
                    || execution_mode == "LegacyTasks"
                        && matches!(principal, Principal::Invocation { .. })
                {
                    return Err(bad(
                        "INVALID_ARGUMENT",
                        "New agent-created work must use WorkExecutor mode",
                    ));
                }
                let project = self.record(&input.project_id, "Project")?;
                if let Principal::Invocation { invocation_id } = principal {
                    let invocation = self.record(invocation_id, "Invocation")?;
                    if !text(&invocation, "workId").is_empty() {
                        return Err(bad(
                            "FORBIDDEN",
                            "A work coordinator cannot create a duplicate or unrelated work",
                        ));
                    }
                    if invocation["scope"] == "Global" {
                        self.authorized_read(principal, &project)?;
                        for source in &input.source_message_ids {
                            self.authorized_read(
                                principal,
                                &self.record(source, "ConversationItem")?,
                            )?;
                        }
                        for artifact in &input.context {
                            self.authorized_read(
                                principal,
                                &self.record(&artifact.artifact_id, "Artifact")?,
                            )?;
                        }
                    } else if text(&invocation, "projectId") != input.project_id {
                        return Err(bad(
                            "FORBIDDEN",
                            "Draft project is outside the intake grant",
                        ));
                    }
                }
                nonempty(&input.goal, "goal")?;
                if input.criteria.is_empty() || input.scope.is_empty() {
                    return Err(bad(
                        "INVALID_ARGUMENT",
                        "Draft requires concrete scope and criteria",
                    ));
                }
                let mut keys = BTreeSet::new();
                for criterion in &input.criteria {
                    nonempty(&criterion.id, "criterion.id")?;
                    nonempty(&criterion.description, "criterion.description")?;
                    nonempty(&criterion.evidence_rule, "criterion.evidenceRule")?;
                    if !keys.insert(&criterion.id) {
                        return Err(bad("INVALID_ARGUMENT", "Duplicate work criterion"));
                    }
                }
                if !["LocalCode", "Report"].contains(&input.delivery.kind.as_str()) {
                    return Err(bad(
                        "METHOD_UNSUPPORTED",
                        "Only local code and readable reports are supported",
                    ));
                }
                if input.delivery.destination_workspace_id.is_some() {
                    return Err(bad(
                        "METHOD_UNSUPPORTED",
                        "Attached delivery workspaces require a writer handoff",
                    ));
                }
                self.artifacts(&input.context)?;
                for source in &input.source_message_ids {
                    self.record(source, "ConversationItem")?;
                }
                let mut spec = encode(&input)?;
                spec.as_object_mut()
                    .map(|fields| fields.remove("executionMode"));
                let work = self.create("Work",json!({"projectId":input.project_id,"title":input.goal,"executionMode":execution_mode,
                    "spec":spec,"currentSpecRevision":1,"currentPlanRevision":0,
                    "lifecycle":"Draft","desiredAdvancement":"Hold","priority":0,
                    "usage":{"executionAttempts":0,"evaluationAttempts":0,"coordinationTurns":0},"headGeneration":0}));
                let work_id = text(&work, "id").to_owned();
                // The root is a recorded intent, not a claimed existing workspace.
                let workspace = self.create("Workspace",json!({"workId":work_id,"projectId":input.project_id,
                    "sourceRoot":project["root"],"status":"Unprovisioned","headGeneration":0,"writer":"None",
                    "managedInputScopes":[],"managedOutputScopes":[]}));
                let mut work = work;
                work["workspaceId"] = workspace["id"].clone();
                let work = self.put(work);
                self.emit("WorkDrafted", &work);
                self.attention(
                    &work_id,
                    &work_id,
                    "DraftApproval",
                    "Review the brief and grant preview, then work.start",
                );
                Ok(Response::ok(
                    "",
                    json!({"workId":work_id,"specRevision":1,"version":work["version"],"workspaceId":workspace["id"]}),
                ))
            }
            "grant.preview" => {
                let input: GrantPreview = parse(&request.params)?;
                let work = self.record(&input.work_id, "Work")?;
                if !matches!(principal, Principal::Human | Principal::Service) {
                    self.proposal_coordinator(principal, &input.work_id)?;
                }
                self.matched(request, &[&work])?;
                let project = self.record(text(&work, "projectId"), "Project")?;
                if input.spec_revision != number(&work, "currentSpecRevision")
                    || input.policy_revision != number(&project, "policyRevision")
                {
                    return Err(bad(
                        "STALE_VERSION",
                        "Grant preview spec or project policy is stale",
                    ));
                }
                let mut body = json!({"workId":input.work_id,"specRevision":input.spec_revision,
                    "policyRevision":input.policy_revision,"allowedCapabilities":project["capabilityIds"],
                    "dataScopes":[project["root"]],"writableResourceScopes":[work["workspaceId"]],
                    "limits":project["limits"],"remaining":project["limits"],"status":"Proposed"});
                if let Some(grant_id) = work.get("currentGrantId").and_then(Value::as_str) {
                    let current = self.record(grant_id, "ExecutionGrant")?;
                    for field in [
                        "allowedCapabilities",
                        "dataScopes",
                        "writableResourceScopes",
                        "limits",
                    ] {
                        body[field] = current[field].clone();
                    }
                    body["basedOnGrantId"] = current["id"].clone();
                    body["remaining"] = current["limits"].clone();
                    for category in [
                        "executionAttempts",
                        "evaluationAttempts",
                        "coordinationTurns",
                    ] {
                        body["remaining"][category] = json!(number(&current["limits"], category)
                            .saturating_sub(number(&work["usage"], category)));
                    }
                }
                let grant = self.create("GrantProposal", body);
                Ok(Response::ok(
                    "",
                    json!({"grantProposalId":grant["id"],"version":grant["version"],"proposal":grant}),
                ))
            }
            "work.start" => {
                Self::human(principal)?;
                let input: WorkStart = parse(&request.params)?;
                let mut work = self.record(&input.work_id, "Work")?;
                self.matched(request, &[&work])?;
                if text(&work, "lifecycle") != "Draft" {
                    return Err(bad("BAD_STATE", "Only a draft can start"));
                }
                let project = self.record(text(&work, "projectId"), "Project")?;
                let mut proposal = self.record(&input.grant_proposal_id, "GrantProposal")?;
                if input.spec_revision != number(&work, "currentSpecRevision")
                    || input.project_policy_revision != number(&project, "policyRevision")
                    || text(&proposal, "workId") != input.work_id
                    || number(&proposal, "specRevision") != input.spec_revision
                    || number(&proposal, "policyRevision") != input.project_policy_revision
                    || text(&proposal, "status") != "Proposed"
                {
                    return Err(bad(
                        "STALE_VERSION",
                        "Start does not bind the current exact grant preview",
                    ));
                }
                let mut grant_body = proposal.clone();
                grant_body["issuedBy"] = json!(principal.key());
                grant_body["status"] = json!("Effective");
                let grant = self.create("ExecutionGrant", grant_body);
                proposal["status"] = json!("Applied");
                self.put(proposal);
                work["currentGrantId"] = grant["id"].clone();
                work["lifecycle"] = json!("Active");
                work["desiredAdvancement"] = json!("Advance");
                let work = self.put(work);
                self.resolve_attention(&input.work_id);
                self.emit("WorkStarted", &work);
                let workspace = self.record(text(&work, "workspaceId"), "Workspace")?;
                let operation = self.effect("workspace.provision",json!({"workspaceId":workspace["id"],"workId":work["id"],
                    "projectId":work["projectId"],"sourceRoot":project["root"],"kind":work["spec"]["delivery"]["kind"]}),Some(&input.work_id));
                if Self::executor_mode(&work) {
                    if !self
                        .related("WorkExecutionTurn", "workId", &input.work_id)
                        .iter()
                        .any(|turn| turn["state"] == "Queued")
                    {
                        self.enqueue_executor_input(
                            &work,
                            text(&work["spec"], "goal"),
                            None,
                            "WorkApproved",
                        )?;
                    }
                } else {
                    self.queue_coordination(Some(&input.work_id), None, "WorkStarted", &work)?;
                }
                Ok(Response::pending(
                    "",
                    text(&operation, "id").into(),
                    json!({"workId":work["id"],"version":work["version"],"lifecycle":"Active","desiredAdvancement":"Advance"}),
                ))
            }
            "plan.propose" => {
                let input: PlanPropose = parse(&request.params)?;
                let work = self.record(&input.work_id, "Work")?;
                self.coordinator(principal, Some(&input.work_id))?;
                self.matched(request, &[&work])?;
                self.validate_plan(&work, &input)?;
                let mut tasks = Vec::new();
                for contract in &input.tasks {
                    let mut task = encode(contract)?;
                    let task_id = contract.existing_task_id.clone().unwrap_or_else(id);
                    let prior = self.records.get(&task_id);
                    let mut contract_value = encode(contract)?;
                    contract_value
                        .as_object_mut()
                        .map(|o| o.remove("existingTaskId"));
                    let unchanged = prior.is_some_and(|prior| prior["contract"] == contract_value);
                    task["id"] = json!(task_id);
                    task["contract"] = contract_value;
                    task["revision"] =
                        json!(prior
                            .map_or(1, |prior| number(prior, "revision") + u64::from(!unchanged)));
                    tasks.push(task);
                }
                let proposal = self.create("PlanProposal",json!({"workId":input.work_id,"basedOnPlanRevision":number(&work,"currentPlanRevision"),
                    "specRevision":work["currentSpecRevision"],"tasks":tasks,"edges":encode(&input.edges)?,
                    "integrationTaskKey":input.integration_task_key,"reason":input.reason,"status":"Proposed"}));
                Ok(Response::ok(
                    "",
                    json!({"proposalId":proposal["id"],"version":proposal["version"],"impact":{"tasks":proposal["tasks"]},"validationFindings":[]}),
                ))
            }
            "plan.apply" => {
                let input: PlanApply = parse(&request.params)?;
                let mut proposal = self.record(&input.proposal_id, "PlanProposal")?;
                let work_id = text(&proposal, "workId").to_owned();
                self.coordinator(principal, Some(&work_id))?;
                let mut work = self.record(&work_id, "Work")?;
                self.matched(request, &[&work, &proposal])?;
                if text(&proposal, "status") != "Proposed"
                    || number(&proposal, "basedOnPlanRevision")
                        != number(&work, "currentPlanRevision")
                    || proposal["specRevision"] != work["currentSpecRevision"]
                {
                    return Err(bad("STALE_VERSION", "Plan proposal is not current"));
                }
                if text(&work, "lifecycle") != "Active"
                    || text(&work, "desiredAdvancement") != "Advance"
                {
                    return Err(bad(
                        "BAD_STATE",
                        "Plan application requires active approved advancement",
                    ));
                }
                let revision = number(&work, "currentPlanRevision") + 1;
                let replanning = work["requiresReplan"] == true;
                if replanning {
                    let operation = self.record(text(&work, "changeOperationId"), "Operation")?;
                    if text(&operation, "status") != "Succeeded" {
                        return Err(bad(
                            "BAD_STATE",
                            "Superseded execution must release before revised plan application",
                        ));
                    }
                }
                let prior_tasks = self.related("Task", "workId", &work_id);
                let proposed_ids: BTreeSet<_> = values(&proposal, "tasks")
                    .iter()
                    .map(|t| text(t, "id").to_owned())
                    .collect();
                for prior in &prior_tasks {
                    if !proposed_ids.contains(text(prior, "id")) {
                        if prior["requiredForDelivery"] == true
                            || !self
                                .related("Attempt", "taskId", text(prior, "id"))
                                .is_empty()
                        {
                            return Err(bad(
                                "METHOD_UNSUPPORTED",
                                "Removing required or executed tasks requires explicit spec change/invalidation",
                            ));
                        }
                    }
                }
                let mut carried = Vec::new();
                for proposed in values(&proposal, "tasks") {
                    let task_id = text(&proposed, "id").to_owned();
                    if let Some(prior) = self.records.get(&task_id).cloned() {
                        if !replanning
                            && number(&prior, "revision") == number(&proposed, "revision")
                        {
                            if text(&prior, "state") == "Blocked"
                                && prior.get("currentResultId").is_none()
                                && !self
                                    .related("Attempt", "taskId", &task_id)
                                    .iter()
                                    .any(|attempt| attempt["reservationHeld"] == true)
                            {
                                let mut replacement = prior;
                                replacement["state"] = json!("Ready");
                                replacement["planRevision"] = json!(revision);
                                self.put(replacement);
                                self.resolve_attention(&task_id);
                                continue;
                            }
                            if let Some(attempt) = prior.get("currentAttemptId") {
                                carried.push(attempt.clone());
                            }
                            continue;
                        }
                        if !self.related("Attempt", "taskId", &task_id).is_empty()
                            && !(replanning
                                && prior["specChangeId"] == work["specChangeId"]
                                && !self
                                    .related("Attempt", "taskId", &task_id)
                                    .iter()
                                    .any(|attempt| attempt["reservationHeld"] == true))
                        {
                            return Err(bad(
                                "METHOD_UNSUPPORTED",
                                "Changing executed task contracts requires explicit settlement/invalidation; propose new bounded investigation tasks instead",
                            ));
                        }
                    }
                    let mut task = proposed.clone();
                    task["kind"] = json!("Task");
                    task["workId"] = json!(work_id);
                    task["planRevision"] = json!(revision);
                    task["specRevision"] = work["currentSpecRevision"].clone();
                    task["state"] = json!("Ready");
                    task["createdAt"] = json!(utc_after(0));
                    self.put(task);
                }
                let plan = self.create("Plan",json!({"workId":work_id,"revision":revision,"specRevision":work["currentSpecRevision"],"tasks":proposal["tasks"],
                    "edges":proposal["edges"],"integrationTaskKey":proposal["integrationTaskKey"],"reason":proposal["reason"]}));
                proposal["status"] = json!("Applied");
                self.put(proposal);
                work["currentPlanRevision"] = json!(revision);
                work["planId"] = plan["id"].clone();
                work["requiresReplan"] = json!(false);
                let work = self.put(work);
                self.emit("PlanApplied", &plan);
                self.emit("WorkChanged", &work);
                self.resolve_attention(&work_id);
                Ok(Response::ok(
                    "",
                    json!({"planRevision":revision,"carriedAttemptIds":carried,"settlementOperationIds":[]}),
                ))
            }
            "workspace.takeover" => {
                Self::human(principal)?;
                let input: WorkspaceTakeover = parse(&request.params)?;
                let mut workspace = self.record(&input.workspace_id, "Workspace")?;
                self.matched(request, &[&workspace])?;
                let work = self.record(text(&workspace, "workId"), "Work")?;
                if text(&work, "lifecycle") != "Active"
                    || text(&workspace, "status") != "Ready"
                    || text(&workspace, "writer") != "None"
                {
                    return Err(bad(
                        "BAD_STATE",
                        "Takeover requires an active managed workspace without another transfer",
                    ));
                }
                workspace["status"] = json!("TakeoverPending");
                workspace["manualHold"] = json!(true);
                let workspace = self.put(workspace);
                let mut stops = Vec::new();
                for mut invocation in self.related("Invocation", "workId", text(&work, "id")) {
                    if !["Released", "Releasing"].contains(&text(&invocation, "state")) {
                        invocation["manualTakeoverWorkspaceId"] = workspace["id"].clone();
                        let invocation = self.put(invocation);
                        stops.push(self.stop_invocation(&invocation, "Hold")?);
                    }
                }
                let operation = self.create("Operation", json!({"workId":work["id"],"workspaceId":workspace["id"],
                    "operationType":"workspace.takeover","status":"Running","steps":[{"state":"Dispatched","intent":"Settle managed writers"}],
                    "stopOperationIds":stops}));
                self.emit("OperationChanged", &operation);
                Ok(Response::pending(
                    "",
                    text(&operation, "id").into(),
                    json!({"workspaceId":workspace["id"]}),
                ))
            }
            "workspace.handback" => {
                Self::human(principal)?;
                let input: WorkspaceHandback = parse(&request.params)?;
                let mut workspace = self.record(&input.workspace_id, "Workspace")?;
                self.matched(request, &[&workspace])?;
                if text(&workspace, "writer") != "Human" || text(&workspace, "status") != "Ready" {
                    return Err(bad(
                        "BAD_STATE",
                        "Handback requires the settled human writer grant",
                    ));
                }
                nonempty(&input.summary, "summary")?;
                workspace["writer"] = json!("HandingBack");
                workspace["status"] = json!("Capturing");
                let workspace = self.put(workspace);
                let mut operation = self.effect("artifact.capture", json!({"workspaceId":workspace["id"],"workId":workspace["workId"],
                    "root":workspace["localRoot"],"sources":[{"kind":"Tree","relativePath":"."}],"purpose":"ManualInput"}),
                    Some(text(&workspace,"workId")));
                operation["handback"] =
                    json!({"summary":input.summary,"resumeAffected":input.resume_affected});
                let operation = self.put(operation);
                Ok(Response::pending(
                    "",
                    text(&operation, "id").into(),
                    json!({"workspaceId":workspace["id"]}),
                ))
            }
            "work.control" => {
                Self::human(principal)?;
                let input: WorkControl = parse(&request.params)?;
                let mut work = self.record(&input.work_id, "Work")?;
                self.matched(request, &[&work])?;
                if !["Active", "Draft"].contains(&text(&work, "lifecycle")) {
                    return Err(bad("BAD_STATE", "Work is already terminal"));
                }
                if !["Hold", "Resume", "Cancel"].contains(&input.action.as_str()) {
                    return Err(bad("INVALID_ARGUMENT", "Invalid work control action"));
                }
                if input.action == "Resume" && text(&work, "lifecycle") != "Active" {
                    return Err(bad(
                        "BAD_STATE",
                        "A draft must be approved through work.start",
                    ));
                }
                work["desiredAdvancement"] = json!(if input.action == "Resume" {
                    "Advance"
                } else {
                    &input.action
                });
                if input.action == "Resume" {
                    for mut workspace in self.related("Workspace", "workId", &input.work_id) {
                        if text(&workspace, "writer") == "None"
                            && text(&workspace, "status") == "Ready"
                            && workspace["manualHold"] == true
                        {
                            workspace["manualHold"] = json!(false);
                            self.put(workspace);
                        }
                    }
                }
                let mut operations = Vec::new();
                if input.action != "Resume" {
                    if let Some(fields) = work.as_object_mut() {
                        fields.remove("continuationRecovery");
                        fields.remove("restartPrimarySession");
                    }
                    if work["executionMode"] == "ClaimingExecutor" {
                        work["executorClaim"]["status"] = json!(if input.action == "Cancel" {
                            "Cancelled"
                        } else {
                            "Held"
                        });
                        work["executorClaim"]["restartSession"] = json!(false);
                    }
                    for invocation in self.related("Invocation", "workId", &input.work_id) {
                        if !["Released", "Ended", "Releasing"].contains(&text(&invocation, "state"))
                        {
                            operations
                                .push(json!(self.stop_invocation(&invocation, &input.action)?));
                        }
                        if input.action == "Cancel" {
                            for mut turn in
                                self.related("WorkExecutionTurn", "workId", &input.work_id)
                            {
                                if turn["state"] == "Queued" {
                                    turn["state"] = json!("Cancelled");
                                    self.put(turn);
                                }
                            }
                        }
                    }
                }
                let work = self.put(work);
                self.emit("WorkChanged", &work);
                Ok(Response::ok(
                    "",
                    json!({"workId":work["id"],"version":work["version"],"desiredAdvancement":work["desiredAdvancement"],"settlementOperationIds":operations}),
                ))
            }
            _ => Err(bad("METHOD_UNSUPPORTED", &request.method)),
        }
    }

    fn validate_plan(&self, work: &Value, input: &PlanPropose) -> DomainResult<()> {
        if text(work, "lifecycle") != "Active" {
            return Err(bad(
                "BAD_STATE",
                "Execution planning requires approved work",
            ));
        }
        if input.based_on_plan_revision.unwrap_or(0) != number(work, "currentPlanRevision") {
            return Err(bad("STALE_VERSION", "Plan base revision is stale"));
        }
        if input.tasks.is_empty() || input.tasks.len() > 64 {
            return Err(bad("INVALID_ARGUMENT", "Plan must have 1..64 tasks"));
        }
        let grant = self.record(text(work, "currentGrantId"), "ExecutionGrant")?;
        let project = self.record(text(work, "projectId"), "Project")?;
        let mut keys = BTreeMap::new();
        let approved_scope: Vec<String> = parse(&work["spec"]["scope"])?;
        let excluded: Vec<String> = parse(&work["spec"]["exclusions"])?;
        for (task_index, task) in input.tasks.iter().enumerate() {
            if task.client_key.is_empty() || keys.insert(task.client_key.clone(), task).is_some() {
                return Err(bad(
                    "INVALID_ARGUMENT",
                    "Task client keys must be unique and nonempty",
                ));
            }
            if let Some(existing) = &task.existing_task_id {
                let prior = self.record(existing, "Task")?;
                if text(&prior, "workId") != text(work, "id")
                    || text(&prior, "clientKey") != task.client_key
                {
                    return Err(bad(
                        "INVALID_REFERENCE",
                        "Existing task belongs to another work/key",
                    ));
                }
                if work["requiresReplan"] == true {
                    for gate in values(&prior["contract"], "gateDefinitions")
                        .into_iter()
                        .filter(|gate| gate["required"] == true)
                    {
                        if !task.gate_definitions.iter().any(|candidate| {
                            encode(candidate).is_ok_and(|candidate| candidate == gate)
                        }) {
                            return Err(bad("FORBIDDEN","A spec replan must preserve existing required checks without weakening their recipes"));
                        }
                    }
                    if prior["contract"]["reviewPolicy"]["required"] == true
                        && encode(&task.review_policy)? != prior["contract"]["reviewPolicy"]
                    {
                        return Err(bad(
                            "FORBIDDEN",
                            "A spec replan must preserve the required reviewer policy",
                        ));
                    }
                    for manual in values(&prior["contract"], "inputSlots")
                        .into_iter()
                        .filter(|slot| text(slot, "slot") == "manual-contribution")
                    {
                        if !task
                            .input_slots
                            .iter()
                            .any(|slot| encode(slot).is_ok_and(|slot| slot == manual))
                        {
                            return Err(bad(
                                "FORBIDDEN",
                                "Replanning must retain the captured human contribution",
                            ));
                        }
                    }
                }
            }
            if !["Contribution", "Integration"].contains(&task.role.as_str()) {
                return Err(bad("INVALID_ARGUMENT", "Invalid task role"));
            }
            if task
                .scope
                .iter()
                .any(|scope| !approved_scope.contains(scope))
                || excluded
                    .iter()
                    .any(|scope| !task.exclusions.contains(scope))
            {
                return Err(bad(
                    "FORBIDDEN",
                    "Task scope/exclusions exceed the exact approved work brief: copy scope entries from work.spec.scope and retain every exact work.spec.exclusions entry; read work.get before correcting the plan",
                ));
            }
            if !values(&grant, "allowedCapabilities").contains(&json!(task.capability_id)) {
                return Err(bad("FORBIDDEN", "Capability is not in the effective grant"));
            }
            let workspace = self.record(&task.resource_requirements.workspace_id, "Workspace")?;
            if text(&workspace, "workId") != text(work, "id") {
                return Err(bad("FORBIDDEN", "Workspace belongs to another work"));
            }
            if !["ReadOnly", "ExclusiveWrite"].contains(&task.resource_requirements.mode.as_str()) {
                return Err(bad("INVALID_ARGUMENT", "Invalid resource mode"));
            }
            nonempty(&task.objective, "objective")?;
            let mut slots = BTreeSet::new();
            for (output_index, output) in task.outputs.iter().enumerate() {
                if output.slot.is_empty() || !slots.insert(&output.slot) {
                    return Err(bad("INVALID_ARGUMENT", "Duplicate/empty output slot"));
                }
                if !OutputContract::KINDS.contains(&output.kind.as_str()) {
                    let message = format!(
                        "Invalid output kind. Expected one of: {} (case-sensitive). Correct the field and submit a new commandId",
                        OutputContract::KINDS.join(", ")
                    );
                    let mut response = bad("INVALID_ARGUMENT", &message);
                    if let Some(failure) = &mut response.failure {
                        failure.field_errors.push(FieldError {
                            path: format!(
                                "params.tasks[{task_index}].outputs[{output_index}].kind"
                            ),
                            message,
                        });
                    }
                    return Err(response);
                }
            }
            if !task.outputs.iter().any(|output| output.required) {
                return Err(bad(
                    "INVALID_ARGUMENT",
                    "Task requires at least one required output",
                ));
            }
            let mut criterion_ids = BTreeSet::new();
            for criterion in &task.criteria {
                if criterion.id.is_empty() || !criterion_ids.insert(&criterion.id) {
                    return Err(bad("INVALID_ARGUMENT", "Duplicate/empty criterion"));
                }
                if criterion.required_evidence.is_empty() {
                    return Err(bad(
                        "INVALID_ARGUMENT",
                        "Every criterion requires concrete evidence rules",
                    ));
                }
            }
            let mut gates = BTreeSet::new();
            for gate in &task.gate_definitions {
                if gate.id.is_empty() || !gates.insert(&gate.id) || gate.revision != 1 {
                    return Err(bad(
                        "INVALID_ARGUMENT",
                        "New gate definitions need unique keys and revision 1",
                    ));
                }
                if gate.criterion_ids.is_empty()
                    || gate
                        .criterion_ids
                        .iter()
                        .any(|key| !criterion_ids.contains(key))
                {
                    return Err(bad("INVALID_ARGUMENT", "Gate references unknown criteria"));
                }
                match gate.kind.as_str() {
                    "Command" => {
                        if gate.decision_schema_ref.is_some() {
                            return Err(bad(
                                "INVALID_ARGUMENT",
                                "Command gate forbids decisionSchemaRef",
                            ));
                        }
                        let recipe = gate.recipe.as_ref().ok_or_else(|| {
                            bad("INVALID_ARGUMENT", "Command gate requires recipe")
                        })?;
                        if recipe.timeout_seconds == 0
                            || recipe.timeout_seconds > number(&grant["limits"], "executionSeconds")
                        {
                            return Err(bad(
                                "FORBIDDEN",
                                "Check timeout exceeds approved execution duration",
                            ));
                        }
                        if recipe.evidence_parser_id != "process-exit-v1" {
                            return Err(bad(
                                "METHOD_UNSUPPORTED",
                                "Only process-exit-v1 is registered",
                            ));
                        }
                        if !values(&project, "environmentRefs")
                            .contains(&json!(recipe.environment_ref))
                        {
                            return Err(bad("INVALID_REFERENCE", "Unknown environmentRef"));
                        }
                        nonempty(&recipe.executable, "recipe.executable")?;
                        Self::relative_path(&recipe.cwd_relative)?;
                    }
                    "HumanCheckpoint" => {
                        return Err(bad(
                            "METHOD_UNSUPPORTED",
                            "Human gate schema registration is not yet implemented; use a TaskInput decision for human judgment",
                        ));
                    }
                    _ => return Err(bad("INVALID_ARGUMENT", "Unknown gate kind")),
                }
            }
            for criterion in &task.criteria {
                for rule in &criterion.required_evidence {
                    if !task.gate_definitions.iter().any(|gate| {
                        gate.required
                            && gate.id == *rule
                            && gate.criterion_ids.contains(&criterion.id)
                    }) && !(rule.starts_with("artifact:")
                        && task.outputs.iter().any(|output| {
                            output.required && format!("artifact:{}", output.slot) == *rule
                        }))
                    {
                        return Err(bad(
                            "INVALID_ARGUMENT",
                            "requiredEvidence must name a required gate or artifact:<required-slot>",
                        ));
                    }
                }
            }
            if task
                .outputs
                .iter()
                .any(|output| ["Code", "Tree", "GitCommit"].contains(&output.kind.as_str()))
                && !task.gate_definitions.iter().any(|gate| gate.required)
            {
                return Err(bad(
                    "INVALID_ARGUMENT",
                    "Code output needs a concrete required check",
                ));
            }
            if task.review_policy.revision == 0
                || task.review_policy.rule != "AllRequiredGatesThenReview"
            {
                return Err(bad("INVALID_ARGUMENT", "Invalid review policy"));
            }
            if task.review_policy.required {
                let capability = task
                    .review_policy
                    .reviewer_capability_id
                    .as_ref()
                    .ok_or_else(|| {
                        bad(
                            "INVALID_ARGUMENT",
                            "Required review needs reviewerCapabilityId",
                        )
                    })?;
                if !values(&grant, "allowedCapabilities").contains(&json!(capability)) {
                    return Err(bad("FORBIDDEN", "Reviewer is not approved"));
                }
            } else if task.review_policy.reviewer_capability_id.is_some() {
                return Err(bad(
                    "INVALID_ARGUMENT",
                    "Optional reviewer capability is not used by this profile",
                ));
            }
            let mut inputs = BTreeSet::new();
            for slot in &task.input_slots {
                if slot.slot.is_empty() || !inputs.insert(&slot.slot) {
                    return Err(bad("INVALID_ARGUMENT", "Duplicate/empty input slot"));
                }
                if let InputSource::Artifact { artifact } = &slot.source {
                    self.artifacts(std::slice::from_ref(artifact))?;
                    let record = self.record(&artifact.artifact_id, "Artifact")?;
                    if text(&record, "workId") != text(work, "id")
                        && !values(&work["spec"], "context").contains(&encode(artifact)?)
                    {
                        return Err(bad(
                            "FORBIDDEN",
                            "External artifact input was not approved in the work brief",
                        ));
                    }
                }
            }
        }
        let integration = keys
            .get(&input.integration_task_key)
            .ok_or_else(|| bad("INVALID_REFERENCE", "Missing integration task"))?;
        if integration.role != "Integration" && text(&work["spec"]["delivery"], "kind") != "Report"
        {
            return Err(bad(
                "INVALID_ARGUMENT",
                "Local code requires an Integration role",
            ));
        }
        for criterion in values(&work["spec"], "criteria") {
            if !integration
                .criteria
                .iter()
                .any(|candidate| candidate.id == text(&criterion, "id"))
            {
                return Err(bad(
                    "INVALID_ARGUMENT",
                    "Integration must map every work criterion",
                ));
            }
            if !integration.criteria.iter().any(|candidate| {
                candidate.id == text(&criterion, "id")
                    && candidate.description == text(&criterion, "description")
                    && candidate
                        .evidence_rule
                        .as_deref()
                        .is_none_or(|rule| rule == text(&criterion, "evidenceRule"))
                    && {
                        let approved = text(&criterion, "evidenceRule");
                        let reference_rule = approved.starts_with("command:")
                            || approved.starts_with("artifact:")
                            || (approved.is_ascii() && !approved.chars().any(char::is_whitespace));
                        candidate.required_evidence.iter().any(|rule| {
                            if let Some(gate_id) = approved.strip_prefix("command:") {
                                rule == gate_id
                                    && integration.gate_definitions.iter().any(|gate| {
                                        gate.id == gate_id
                                            && gate.kind == "Command"
                                            && gate.required
                                            && gate.criterion_ids.contains(&candidate.id)
                                    })
                            } else {
                                rule == approved
                            }
                        }) || (!reference_rule
                            && candidate.evidence_rule.as_deref() == Some(approved))
                    }
            }) {
                return Err(bad(
                    "INVALID_ARGUMENT",
                    "Integration criteria must preserve the approved description and evidence rule: copy prose to criteria[].evidenceRule and bind requiredEvidence to required gate IDs or artifact:<required-slot>. command:<gate>, artifact:<slot> and legacy bare ASCII gate references must retain their exact required binding",
                ));
            }
        }
        let mut adjacency: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut edges = BTreeSet::new();
        for edge in &input.edges {
            let source = keys
                .get(&edge.source_task_key)
                .ok_or_else(|| bad("INVALID_REFERENCE", "Unknown edge source"))?;
            let consumer = keys
                .get(&edge.consumer_task_key)
                .ok_or_else(|| bad("INVALID_REFERENCE", "Unknown edge consumer"))?;
            if edge.source_task_key == edge.consumer_task_key
                || !edges.insert((
                    &edge.source_task_key,
                    &edge.output_slot,
                    &edge.consumer_task_key,
                ))
            {
                return Err(bad("INVALID_ARGUMENT", "Self/duplicate dependency"));
            }
            if !source
                .outputs
                .iter()
                .any(|output| output.slot == edge.output_slot)
            {
                return Err(bad(
                    "INVALID_REFERENCE",
                    "Dependency references unknown output slot",
                ));
            }
            if !["ArtifactAvailable", "GatePassed"].contains(&edge.condition.as_str()) {
                return Err(bad(
                    "METHOD_UNSUPPORTED",
                    "Unsupported dependency condition",
                ));
            }
            if edge.consumer_task_key == input.integration_task_key
                && edge.condition != "GatePassed"
            {
                return Err(bad("INVALID_ARGUMENT", "Integration consumes accepted contribution bindings through GatePassed dependencies"));
            }
            if edge
                .required_gate_ids
                .iter()
                .any(|key| !source.gate_definitions.iter().any(|gate| gate.id == *key))
            {
                return Err(bad("INVALID_REFERENCE", "Unknown dependency gate"));
            }
            if !consumer.input_slots.iter().any(|slot|matches!(&slot.source,InputSource::Dependency{source_task_key,output_slot} if *source_task_key==edge.source_task_key && *output_slot==edge.output_slot)) { return Err(bad("INVALID_ARGUMENT","Dependency has no corresponding consumer input slot")); }
            adjacency
                .entry(edge.consumer_task_key.clone())
                .or_default()
                .push(edge.source_task_key.clone());
        }
        for task in &input.tasks {
            for slot in &task.input_slots {
                if let InputSource::Dependency {
                    source_task_key,
                    output_slot,
                } = &slot.source
                {
                    if !edges.contains(&(source_task_key, output_slot, &task.client_key)) {
                        return Err(bad(
                            "INVALID_ARGUMENT",
                            "Input dependency has no explicit edge",
                        ));
                    }
                }
            }
            let mut stack = vec![task.client_key.clone()];
            let mut visited = BTreeSet::new();
            while let Some(key) = stack.pop() {
                if let Some(parents) = adjacency.get(&key) {
                    for parent in parents {
                        if parent == &task.client_key {
                            return Err(bad("INVALID_ARGUMENT", "Dependency cycle"));
                        }
                        if visited.insert(parent.clone()) {
                            stack.push(parent.clone());
                        }
                    }
                }
            }
        }
        let mut ancestors = BTreeSet::new();
        let mut stack = vec![input.integration_task_key.clone()];
        while let Some(key) = stack.pop() {
            for parent in adjacency.get(&key).into_iter().flatten() {
                if ancestors.insert(parent.clone()) {
                    stack.push(parent.clone());
                }
            }
        }
        for task in &input.tasks {
            if task.required_for_delivery
                && task.client_key != input.integration_task_key
                && !ancestors.contains(&task.client_key)
            {
                return Err(bad(
                    "INVALID_ARGUMENT",
                    "Integration omits a required contribution",
                ));
            }
        }
        Ok(())
    }

    pub(super) fn relative_path(path: &str) -> DomainResult<()> {
        if path.is_empty()
            || Path::new(path).is_absolute()
            || path.split(['\\', '/']).any(|part| part == "..")
            || path.contains(':')
        {
            Err(bad(
                "INVALID_ARGUMENT",
                "Path must remain relative to the registered workspace",
            ))
        } else {
            Ok(())
        }
    }

    pub(super) fn schedule(&mut self) -> DomainResult<()> {
        for work in self.all("Work") {
            if !Self::legacy_mode(&work)
                || text(&work, "lifecycle") != "Active"
                || text(&work, "desiredAdvancement") != "Advance"
                || work["requiresReplan"] == true
                || work.get("continuationRecovery").is_some()
            {
                continue;
            }
            let work_id = text(&work, "id");
            let workspace = self.record(text(&work, "workspaceId"), "Workspace")?;
            if text(&workspace, "status") != "Ready" || workspace["manualHold"] == true {
                continue;
            }
            let Some(plan) = self.records.get(text(&work, "planId")).cloned() else {
                continue;
            };
            for task in self.related("Task", "workId", work_id) {
                if text(&task, "state") != "Ready" {
                    continue;
                }
                let Some(inputs) = self.resolve_inputs(&task, &plan)? else {
                    continue;
                };
                self.admit_task(&work, &task, inputs, None)?;
            }
            for unit in self.related("EvaluationUnit", "workId", work_id) {
                if text(&unit, "state") != "Queued" {
                    continue;
                }
                let task = self.record(text(&unit, "taskId"), "Task")?;
                let inputs = values(&unit, "inputs");
                self.admit_task(&work, &task, inputs, Some(&unit))?;
            }
        }
        Ok(())
    }

    fn resolve_inputs(&self, task: &Value, plan: &Value) -> DomainResult<Option<Vec<Value>>> {
        let contract: TaskContract = parse(&task["contract"])?;
        let mut inputs = Vec::new();
        for slot in contract.input_slots {
            match slot.source {
                InputSource::Artifact { artifact } => {
                    self.artifacts(std::slice::from_ref(&artifact))?;
                    inputs.push(encode(&InputRef {
                        slot: slot.slot,
                        artifact,
                        source_result_id: None,
                        source_gate_ids: Vec::new(),
                    })?);
                }
                InputSource::Dependency {
                    source_task_key,
                    output_slot,
                } => {
                    let source = self
                        .related("Task", "workId", text(task, "workId"))
                        .into_iter()
                        .find(|candidate| text(candidate, "clientKey") == source_task_key)
                        .ok_or_else(|| bad("INVALID_REFERENCE", "Dependency source disappeared"))?;
                    let Some(result) = self.records.get(text(&source, "currentResultId")) else {
                        return Ok(None);
                    };
                    let edge = values(plan, "edges")
                        .into_iter()
                        .find(|edge| {
                            text(edge, "sourceTaskKey") == source_task_key
                                && text(edge, "consumerTaskKey") == contract.client_key
                                && text(edge, "outputSlot") == output_slot
                        })
                        .ok_or_else(|| bad("INVALID_REFERENCE", "Dependency edge disappeared"))?;
                    if text(&edge, "condition") == "GatePassed"
                        && text(result, "disposition") != "Accepted"
                    {
                        return Ok(None);
                    }
                    if !["Submitted", "Reviewing", "Accepted"]
                        .contains(&text(result, "disposition"))
                    {
                        return Ok(None);
                    }
                    let gates: Vec<_> = self
                        .related("GateResult", "resultId", text(result, "id"))
                        .into_iter()
                        .filter(|gate| {
                            number(gate, "evaluationRound") == number(result, "evaluationRound")
                                && text(gate, "outcome") == "Passed"
                        })
                        .collect();
                    for required in values(&edge, "requiredGateIds") {
                        if !gates
                            .iter()
                            .any(|gate| gate["gateDefinitionId"] == required)
                        {
                            return Ok(None);
                        }
                    }
                    let Some(output) = values(&result["body"], "outputs")
                        .into_iter()
                        .find(|output| text(output, "slot") == output_slot)
                    else {
                        return Ok(None);
                    };
                    inputs.push(json!({"slot":slot.slot,"artifact":output["artifact"],"sourceResultId":result["id"],
                        "sourceGateIds":gates.iter().map(|gate|gate["id"].clone()).collect::<Vec<_>>()}));
                }
            }
        }
        inputs.sort_by(|a, b| {
            text(a, "slot")
                .cmp(text(b, "slot"))
                .then(text(&a["artifact"], "artifactId").cmp(text(&b["artifact"], "artifactId")))
        });
        Ok(Some(inputs))
    }

    pub(super) fn finalize_works(&mut self) -> DomainResult<()> {
        for mut work in self.all("Work") {
            if text(&work, "lifecycle") != "Active" && text(&work, "lifecycle") != "Draft" {
                continue;
            }
            let work_id = text(&work, "id").to_owned();
            let live = self
                .related("Invocation", "workId", &work_id)
                .iter()
                .any(|invocation| text(invocation, "state") != "Released");
            let pending = self
                .related("Operation", "workId", &work_id)
                .iter()
                .any(|operation| !["Succeeded", "Failed"].contains(&text(operation, "status")));
            if !live && !pending && text(&work, "desiredAdvancement") == "Cancel" {
                work["lifecycle"] = json!("Cancelled");
                let work = self.put(work);
                self.emit("WorkCancelled", &work);
                for item in self.related("AttentionItem", "workId", &work_id) {
                    self.resolve_attention(text(&item, "subjectId"));
                }
            } else if !live && !pending && work.get("currentAcceptanceId").is_some() {
                work["lifecycle"] = json!("Completed");
                let work = self.put(work);
                self.emit("WorkCompleted", &work);
            }
        }
        Ok(())
    }

    pub(super) fn settle_workspace_operations(&mut self) -> DomainResult<()> {
        for mut operation in self.all("Operation") {
            if text(&operation, "operationType") != "workspace.takeover"
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
            let mut workspace = self.record(text(&operation, "workspaceId"), "Workspace")?;
            workspace["writer"] = json!("Human");
            workspace["status"] = json!("Ready");
            let workspace = self.put(workspace);
            operation["status"] = json!("Succeeded");
            operation["steps"] =
                json!([{"state":"Confirmed","intent":"Transfer exclusive managed writer grant"}]);
            operation["result"] = json!({"workspaceId":workspace["id"],"localRoot":workspace["localRoot"],"writer":"Human"});
            let operation = self.put(operation);
            self.emit("OperationChanged", &operation);
            self.emit("TakeoverReady", &workspace);
            self.attention(
                &work_id,
                text(&workspace, "id"),
                "HumanWriter",
                "Hand back the edited workspace to capture changes and resume affected validation",
            );
        }
        Ok(())
    }

    pub(super) fn apply_handback(&mut self, operation: &Value) -> DomainResult<()> {
        let work_id = text(operation, "workId");
        let mut work = self.record(work_id, "Work")?;
        let mut workspace = self.record(
            text(&operation["effect"]["params"], "workspaceId"),
            "Workspace",
        )?;
        let artifacts = values(&operation["result"], "artifacts");
        if artifacts.len() != 1 {
            return Err(bad(
                "EXECUTION_FAILED",
                "Handback must produce exactly one fixed tree snapshot",
            ));
        }
        let snapshot = artifacts[0].clone();
        let revision = number(&work, "currentPlanRevision") + 1;
        if let Some(mut plan) = self.records.get(text(&work, "planId")).cloned() {
            let mut contracts = Vec::new();
            for mut task in self.related("Task", "workId", work_id) {
                // Without an executor-provided changed-file manifest, all workspace consumers
                // are conservatively invalidated; no old gate is relabeled for manual content.
                if text(&task["resourceRequirements"], "workspaceId") != text(&workspace, "id") {
                    contracts.push(task);
                    continue;
                }
                let mut inputs = values(&task["contract"], "inputSlots");
                inputs.retain(|input| text(input, "slot") != "manual-contribution");
                inputs.push(json!({"slot":"manual-contribution","source":{"kind":"Artifact","artifact":snapshot}}));
                task["contract"]["inputSlots"] = json!(inputs);
                task["inputSlots"] = task["contract"]["inputSlots"].clone();
                task["revision"] = json!(number(&task, "revision") + 1);
                task["planRevision"] = json!(revision);
                task["state"] = json!("Ready");
                task.as_object_mut().map(|object| {
                    object.remove("acceptedResultId");
                    object.remove("reworkId");
                });
                if let Some(mut result) = self.records.get(text(&task, "currentResultId")).cloned()
                {
                    result["disposition"] = json!("Superseded");
                    result["invalidationReason"] = json!("HumanContribution");
                    self.put(result);
                }
                for mut unit in self.related("EvaluationUnit", "taskId", text(&task, "id")) {
                    if ["Queued", "Running"].contains(&text(&unit, "state")) {
                        unit["state"] = json!("Superseded");
                        self.put(unit);
                    }
                }
                contracts.push(task.clone());
                let task = self.put(task);
                self.resolve_attention(text(&task, "id"));
            }
            plan["revision"] = json!(revision);
            plan["specRevision"] = work["currentSpecRevision"].clone();
            plan["tasks"] = json!(contracts);
            plan["reason"] = json!(
                "Revalidate all managed workspace consumers after the captured human contribution"
            );
            let plan = self.create("Plan", plan);
            work["planId"] = plan["id"].clone();
            work["currentPlanRevision"] = json!(revision);
            self.emit("PlanApplied", &plan);
        }
        if let Some(mut candidate) = self.records.get(text(&work, "currentCandidateId")).cloned() {
            candidate["status"] = json!("Superseded");
            let candidate = self.put(candidate);
            self.resolve_attention(text(&candidate, "id"));
        }
        work["headGeneration"] = json!(number(&work, "headGeneration") + 1);
        if let Some(object) = work.as_object_mut() {
            for field in [
                "currentCandidateId",
                "integrationResultId",
                "currentAcceptanceId",
            ] {
                object.remove(field);
            }
        }
        workspace["headGeneration"] = work["headGeneration"].clone();
        workspace["writer"] = json!("None");
        workspace["status"] = json!("Ready");
        workspace["manualHold"] = json!(operation["handback"]["resumeAffected"] != true);
        workspace["lastHumanContribution"] = snapshot.clone();
        let workspace = self.put(workspace);
        let contribution = self.create("HumanContribution",json!({"workId":work_id,"workspaceId":workspace["id"],
            "artifact":snapshot,"summary":operation["handback"]["summary"],"planRevision":revision,"operationId":operation["id"]}));
        self.put(work);
        self.resolve_attention(text(&workspace, "id"));
        self.emit("HumanContributionRecorded", &contribution);
        self.queue_coordination(
            Some(work_id),
            None,
            "HumanContributionRecorded",
            &contribution,
        )?;
        Ok(())
    }
}
