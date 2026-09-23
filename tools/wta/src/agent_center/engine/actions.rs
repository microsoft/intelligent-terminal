// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

use super::*;

impl Engine {
    pub(super) fn resolve_global_input(
        &mut self,
        principal: &Principal,
        request: &Request,
    ) -> DomainResult<Response> {
        let input: ResolveGlobalInput = parse(&request.params)?;
        let invocation = self.global_coordinator(principal)?;
        let mut question = self.record(&input.request_id, "IntakeRequest")?;
        self.matched(request, &[&question])?;
        if text(&question, "status") != "Open" {
            return Err(bad("STALE_VERSION", "Question already answered/cancelled"));
        }
        let conversation_id = text(&invocation, "conversationId");
        if text(&question, "conversationId") != conversation_id {
            return Err(bad("FORBIDDEN", "Question belongs to another conversation"));
        }
        let captured_questions = invocation
            .pointer("/coordinationInput/snapshot/intakeRequests")
            .and_then(Value::as_array)
            .ok_or_else(|| bad("INVALID_REFERENCE", "Missing captured intake questions"))?;
        if !captured_questions
            .iter()
            .any(|captured| captured == &question)
        {
            return Err(bad(
                "FORBIDDEN",
                "Question was not captured by this global invocation",
            ));
        }
        self.human_action_source(&invocation, conversation_id, &input.message_id, true)?;
        let messages = invocation
            .pointer("/coordinationInput/snapshot/messages")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                bad(
                    "INVALID_REFERENCE",
                    "Missing captured conversation messages",
                )
            })?;
        let source = messages
            .iter()
            .position(|message| text(message, "id") == text(&question, "messageId"))
            .ok_or_else(|| {
                bad(
                    "INVALID_REFERENCE",
                    "Question source is not in captured history",
                )
            })?;
        let answer = messages
            .iter()
            .position(|message| text(message, "id") == input.message_id)
            .ok_or_else(|| {
                bad(
                    "INVALID_REFERENCE",
                    "Answer source is not in captured history",
                )
            })?;
        if answer <= source {
            return Err(bad(
                "FORBIDDEN",
                "An intake answer needs new human input after the question's source",
            ));
        }
        Self::validate_answer(&question["responseSchema"], &input.value)?;
        question["status"] = json!("Answered");
        question["answer"] = input.value;
        question["answerMessageId"] = json!(input.message_id);
        question["answerSource"] = json!("HumanMessageInterpretation");
        let question = self.put(question);
        let conversation = self.record(conversation_id, "Conversation")?;
        let conversation = self.put(conversation);
        self.emit("IntakeAnswered", &question);
        self.emit("IntakeAnswered", &conversation);
        // The current invocation already owns the new human input. Requeueing it
        // here would consume another turn and invalidate its own action previews.
        Ok(Response::ok(
            "",
            json!({"requestId":question["id"],"state":"Answered","question":question}),
        ))
    }

    pub(super) fn propose_human_action(
        &mut self,
        principal: &Principal,
        request: &Request,
    ) -> DomainResult<Response> {
        self.matched(request, &[])?;
        let input: ProposeHumanAction = parse(&request.params)?;
        let invocation = self.global_coordinator(principal)?;
        nonempty(&input.summary, "summary")?;
        self.human_action_source(&invocation, &input.conversation_id, &input.message_id, true)?;
        let mut frozen = Request::new(&input.method, input.params);
        frozen.command_id = Some(id());
        frozen.if_match = input.if_match;
        let (target, preview) = self.human_action_preview(principal, &frozen, &invocation)?;
        let envelope = Self::human_action_envelope(&frozen);
        for mut previous in self.related(
            "HumanActionProposal",
            "conversationId",
            &input.conversation_id,
        ) {
            if text(&previous, "status") == "Open" && previous["target"] == target {
                previous["status"] = json!("Superseded");
                let previous = self.put(previous);
                self.emit_human_action("HumanActionSuperseded", &previous)?;
            }
        }
        let mut body = json!({
            "status":"Open",
            "conversationId":input.conversation_id,
            "messageId":input.message_id,
            "invocationId":invocation["id"],
            "summary":input.summary,
            "target":target,
            "request":envelope,
            "preview":preview
        });
        if let Some(work_id) = body["preview"].pointer("/work/work/id").cloned() {
            body["workId"] = work_id;
        }
        let proposal = self.create("HumanActionProposal", body);
        self.emit_human_action("HumanActionProposed", &proposal)?;
        Ok(Response::ok(
            "",
            json!({"proposalId":proposal["id"],"proposal":proposal}),
        ))
    }

    pub(super) fn validate_human_action(
        &self,
        principal: &Principal,
        request: &Request,
    ) -> DomainResult<()> {
        let Some(proposal) = self.human_action_for_request(request) else {
            return Ok(());
        };
        Self::human(principal)?;
        if text(&proposal, "status") != "Open" {
            return Err(bad(
                "BAD_STATE",
                "Human action proposal is no longer open; refresh the conversation",
            ));
        }
        if proposal["request"] != Self::human_action_envelope(request) {
            return Err(bad(
                "INVALID_ARGUMENT",
                "Confirmation must submit the exact frozen human action request",
            ));
        }
        let invocation = self.record(text(&proposal, "invocationId"), "Invocation")?;
        // The proposing invocation may have finished. Its captured input remains the
        // provenance binding; a new global message must not retarget another work's card.
        self.human_action_source(
            &invocation,
            text(&proposal, "conversationId"),
            text(&proposal, "messageId"),
            false,
        )?;
        let (target, preview) = self.human_action_preview(principal, request, &invocation)?;
        if target != proposal["target"] || preview != proposal["preview"] {
            return Err(bad(
                "STALE_VERSION",
                "Human action authority or preview changed; request a fresh proposal",
            ));
        }
        Ok(())
    }

    pub(super) fn record_human_action(
        &mut self,
        request: &Request,
        response: &Response,
    ) -> DomainResult<()> {
        let Some(mut proposal) = self.human_action_for_request(request) else {
            return Ok(());
        };
        if !["ok", "pending", "needs_input"].contains(&response.status.as_str())
            || response.failure.is_some()
        {
            return Ok(());
        }
        if !matches!(
            (request.method.as_str(), response.status.as_str()),
            (
                "work.start" | "work.apply_change" | "project.configure",
                "pending"
            ) | (
                "work.control"
                    | "work.continue"
                    | "work.claim_executor"
                    | "project.configure"
                    | "delivery.accept"
                    | "delivery.request_changes",
                "ok"
            )
        ) {
            return Err(bad(
                "BAD_STATE",
                "Response status does not match the proposed operation contract",
            ));
        }
        if response.status == "pending" {
            let operation_id = response.operation_id.as_deref().ok_or_else(|| {
                bad(
                    "INVALID_REFERENCE",
                    "Pending human action requires a recorded operation",
                )
            })?;
            self.record(operation_id, "Operation")?;
        }
        if text(&proposal, "status") != "Open"
            || proposal["request"] != Self::human_action_envelope(request)
        {
            return Err(bad(
                "BAD_STATE",
                "Only the validated open human action can record a submission",
            ));
        }
        proposal["status"] = json!("Submitted");
        proposal["submission"] = encode(response)?;
        let proposal = self.put(proposal);
        self.emit_human_action("HumanActionSubmitted", &proposal)?;
        if request.method == "project.configure" && response.status == "pending" {
            // The project is not readable yet. Its terminal receipt wakes a fresh
            // conversation with the committed project in its captured access.
            return Ok(());
        }
        self.queue_coordination(
            None,
            Some(text(&proposal, "conversationId")),
            "HumanActionSubmitted",
            &proposal,
        )
    }

    fn human_action_envelope(request: &Request) -> Value {
        json!({
            "method":request.method,
            "params":request.params,
            "ifMatch":request.if_match,
            "commandId":request.command_id
        })
    }

    pub(super) fn human_action_for_request(&self, request: &Request) -> Option<Value> {
        let command_id = request.command_id.as_deref()?;
        self.all("HumanActionProposal")
            .into_iter()
            .find(|proposal| text(&proposal["request"], "commandId") == command_id)
    }

    fn human_action_source(
        &self,
        invocation: &Value,
        conversation_id: &str,
        message_id: &str,
        latest: bool,
    ) -> DomainResult<()> {
        uuid(conversation_id)?;
        uuid(message_id)?;
        let conversation = self.record(conversation_id, "Conversation")?;
        let message = self.record(message_id, "ConversationItem")?;
        if text(invocation, "scope") != "Global"
            || text(&invocation["subject"], "kind") != "Coordination"
            || text(invocation, "conversationId") != conversation_id
            || text(&conversation, "scope") != "Global"
            || text(&message, "conversationId") != conversation_id
            || text(&message, "role") != "human"
        {
            return Err(bad(
                "FORBIDDEN",
                "Proposal source must be a human message in the bound global conversation",
            ));
        }
        let snapshot_messages = invocation
            .pointer("/coordinationInput/snapshot/messages")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                bad(
                    "INVALID_REFERENCE",
                    "Global invocation has no captured conversation messages",
                )
            })?;
        if !snapshot_messages.iter().any(|captured| {
            // Intent resolution annotates the record in-place. The captured
            // human content and transport context, not its mutable version, bind it.
            [
                "id",
                "kind",
                "role",
                "conversationId",
                "clientMessageId",
                "text",
                "attachments",
                "context",
                "declaredIntent",
            ]
            .iter()
            .all(|field| captured.get(*field) == message.get(*field))
        }) {
            return Err(bad(
                "FORBIDDEN",
                "Human source message was not captured by this invocation",
            ));
        }
        let messages = conversation
            .get("messages")
            .and_then(Value::as_array)
            .ok_or_else(|| bad("INVALID_REFERENCE", "Conversation has no message history"))?;
        if !messages.iter().any(|current| current == &message) {
            return Err(bad(
                "INVALID_REFERENCE",
                "Human source message is not in its conversation history",
            ));
        }
        if latest
            && messages
                .iter()
                .rev()
                .find(|message| text(message, "role") == "human")
                .is_none_or(|message| text(message, "id") != message_id)
        {
            return Err(bad(
                "STALE_VERSION",
                "Newer human input arrived; coordinate from the current conversation",
            ));
        }
        Ok(())
    }

    fn emit_human_action(&mut self, kind: &str, proposal: &Value) -> DomainResult<()> {
        let conversation = self.record(text(proposal, "conversationId"), "Conversation")?;
        let conversation = self.put(conversation);
        self.emit(kind, proposal);
        self.emit(kind, &conversation);
        if let Some(work_id) = proposal.get("workId").and_then(Value::as_str) {
            for conversation in self.related("Conversation", "workId", work_id) {
                if conversation["id"] != proposal["conversationId"] {
                    let conversation = self.put(conversation);
                    self.emit(kind, &conversation);
                }
            }
        }
        Ok(())
    }

    fn human_action_preview(
        &self,
        principal: &Principal,
        request: &Request,
        invocation: &Value,
    ) -> DomainResult<(Value, Value)> {
        let (work, project, grant) = match request.method.as_str() {
            "project.configure" => {
                return self.human_action_project_preview(request, invocation);
            }
            "work.apply_change" => {
                return self.human_action_change_preview(principal, request);
            }
            "delivery.accept" | "delivery.request_changes" => {
                return self.human_action_delivery_preview(principal, request);
            }
            "work.start" => {
                let input: WorkStart = parse(&request.params)?;
                let work = self.record(&input.work_id, "Work")?;
                self.authorized_read(principal, &work)?;
                self.matched(request, &[&work])?;
                if text(&work, "lifecycle") != "Draft" {
                    return Err(bad("BAD_STATE", "Only a draft can start"));
                }
                let project = self.record(text(&work, "projectId"), "Project")?;
                self.authorized_read(principal, &project)?;
                let grant = self.record(&input.grant_proposal_id, "GrantProposal")?;
                if input.spec_revision != number(&work, "currentSpecRevision")
                    || input.project_policy_revision != number(&project, "policyRevision")
                    || text(&grant, "workId") != input.work_id
                    || number(&grant, "specRevision") != input.spec_revision
                    || number(&grant, "policyRevision") != input.project_policy_revision
                    || text(&grant, "status") != "Proposed"
                {
                    return Err(bad(
                        "STALE_VERSION",
                        "Start does not bind the current exact grant preview",
                    ));
                }
                (work, project, Some(grant))
            }
            "work.claim_executor" => {
                let input: ClaimExecutor = parse(&request.params)?;
                let work = self.record(&input.work_id, "Work")?;
                self.authorized_read(principal, &work)?;
                self.matched(request, &[&work])?;
                self.validate_executor_claim(&work, input.restart_session.unwrap_or(false))?;
                let project = self.record(text(&work, "projectId"), "Project")?;
                self.authorized_read(principal, &project)?;
                let grant = self.record(text(&work, "currentGrantId"), "ExecutionGrant")?;
                let (target, mut preview) =
                    self.human_action_work_preview(&work, &project, Some(&grant))?;
                preview["continuation"] = json!({"restartSession":input.restart_session.unwrap_or(false),
                    "sessionBehavior":if input.restart_session==Some(true) {"ReconstructAfterSettlement"}else{"AdoptVerifiedActualExecutorAfterSettlement"},
                    "requiresSettlement":true,"createsWork":false,"createsIndependentWriter":false,
                    "preservesProviderHistory":input.restart_session!=Some(true)});
                return Ok((target, preview));
            }
            "work.continue" => {
                let input: WorkContinue = parse(&request.params)?;
                let work = self.record(&input.work_id, "Work")?;
                self.authorized_read(principal, &work)?;
                self.matched(request, &[&work])?;
                if work.get("executionMode").is_none()
                    || (work["executionMode"] == "ClaimingExecutor"
                        && work["executorClaim"]["status"] != "Settling"
                        && work["executorClaim"]["status"] != "Held"
                        && input.restart_session != Some(true))
                {
                    return Err(bad(
                        "WORK_EXECUTOR_CLAIM_REQUIRED",
                        "Propose an explicit actual-executor claim for this historical work",
                    ));
                }
                self.validate_continue_work(&work)?;
                if work["executionMode"] == "ClaimingExecutor" {
                    let mut claim = request.clone();
                    claim.method = "work.claim_executor".into();
                    if work["executorClaim"]["status"] != "Settling" {
                        return self.human_action_preview(principal, &claim, invocation);
                    }
                }
                let project = self.record(text(&work, "projectId"), "Project")?;
                self.authorized_read(principal, &project)?;
                let grant = self.record(text(&work, "currentGrantId"), "ExecutionGrant")?;
                let unsettled: Vec<_> = self
                    .related("Invocation", "workId", &input.work_id)
                    .into_iter()
                    .filter(|invocation| invocation["state"] != "Released")
                    .collect();
                let attaching = !unsettled.is_empty()
                    && unsettled
                        .iter()
                        .all(|invocation| self.invocation_live(invocation));
                let claiming = work["executionMode"] == "ClaimingExecutor";
                let restart = if claiming {
                    work["executorClaim"]["restartSession"] == true
                } else {
                    input.restart_session.unwrap_or(false)
                };
                if unsettled.is_empty() && !restart && !claiming {
                    self.primary_session_ref(&work)?;
                }
                let (target, mut preview) =
                    self.human_action_work_preview(&work, &project, Some(&grant))?;
                preview["continuation"] = json!({
                    "restartSession":restart,
                    "sessionBehavior":if claiming {"ReconcileExistingClaim"} else if attaching {"AttachExistingExecution"} else if restart {"ReconstructAfterSettlement"} else {"LoadSavedPrimarySessionAfterSettlement"},
                    "requiresSettlement":!unsettled.is_empty() && !attaching,
                    "createsWork":false,"createsIndependentWriter":false,
                    "preservesProviderHistory":attaching || !restart
                });
                return Ok((target, preview));
            }
            "work.control" => {
                let input: WorkControl = parse(&request.params)?;
                let work = self.record(&input.work_id, "Work")?;
                self.authorized_read(principal, &work)?;
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
                let project = self.record(text(&work, "projectId"), "Project")?;
                self.authorized_read(principal, &project)?;
                let grant = work
                    .get("currentGrantId")
                    .and_then(Value::as_str)
                    .map(|grant_id| self.record(grant_id, "ExecutionGrant"))
                    .transpose()?;
                (work, project, grant)
            }
            _ => {
                return Err(bad(
                    "METHOD_UNSUPPORTED",
                    "This operation does not support global human action proposals",
                ));
            }
        };
        self.human_action_work_preview(&work, &project, grant.as_ref())
    }

    fn human_action_work_preview(
        &self,
        work: &Value,
        project: &Value,
        grant: Option<&Value>,
    ) -> DomainResult<(Value, Value)> {
        let workspace = self.record(text(work, "workspaceId"), "Workspace")?;
        let mut preview = json!({
            "work":self.view(work),
            "project":project,
            "workspace":workspace,
            "destination":work["spec"]["delivery"]
        });
        if let Some(grant) = grant {
            preview["grant"] = json!({"data":{"proposal":grant}});
        }
        Ok((json!({"kind":"Work","id":work["id"]}), preview))
    }

    fn human_action_change_preview(
        &self,
        principal: &Principal,
        request: &Request,
    ) -> DomainResult<(Value, Value)> {
        let input: WorkApplyChange = parse(&request.params)?;
        let proposal = self.record(&input.proposal_id, "ChangeProposal")?;
        let work = self.record(text(&proposal, "workId"), "Work")?;
        self.authorized_read(principal, &work)?;
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
        self.authorized_read(principal, &project)?;
        let previous_grant = self.record(text(&work, "currentGrantId"), "ExecutionGrant")?;
        let grant = self.record(&input.grant_proposal_id, "GrantProposal")?;
        if text(&grant, "status") != "Proposed"
            || grant["workId"] != work["id"]
            || grant["specRevision"] != work["currentSpecRevision"]
            || grant["policyRevision"] != project["policyRevision"]
            || grant["basedOnGrantId"] != previous_grant["id"]
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
            if grant[field] != previous_grant[field] {
                return Err(bad(
                    "FORBIDDEN",
                    "Specification revision cannot expand or reset approved execution authority",
                ));
            }
        }
        let (target, mut preview) =
            self.human_action_work_preview(&work, &project, Some(&grant))?;
        preview["changeProposal"] = proposal;
        preview["previousGrant"] = previous_grant;
        Ok((target, preview))
    }

    fn human_action_delivery_preview(
        &self,
        principal: &Principal,
        request: &Request,
    ) -> DomainResult<(Value, Value)> {
        let (candidate_id, changes) = if request.method == "delivery.accept" {
            let input: DeliveryAccept = parse(&request.params)?;
            (input.candidate_id, None)
        } else {
            let input: DeliveryChanges = parse(&request.params)?;
            (input.candidate_id.clone(), Some(input))
        };
        let candidate = self.record(&candidate_id, "DeliveryCandidate")?;
        let work = self.record(text(&candidate, "workId"), "Work")?;
        self.authorized_read(principal, &work)?;
        self.matched(request, &[&work, &candidate])?;
        if text(&candidate, "status") != "Proposed"
            || text(&work, "currentCandidateId") != candidate_id
        {
            return Err(bad("STALE_VERSION", "Candidate is not current"));
        }
        let result = self.record(text(&candidate, "integrationResultId"), "TaskResult")?;
        if let Some(changes) = &changes {
            if changes.findings.is_empty() {
                return Err(bad(
                    "INVALID_ARGUMENT",
                    "Revision requires concrete findings",
                ));
            }
            self.artifacts(&changes.preserve_artifacts)?;
            for artifact in &changes.preserve_artifacts {
                self.authorized_read(principal, &self.record(&artifact.artifact_id, "Artifact")?)?;
            }
            for finding in &changes.findings {
                if !values(&work["spec"], "criteria")
                    .iter()
                    .any(|criterion| text(criterion, "id") == finding.criterion_id)
                {
                    return Err(bad(
                        "INVALID_ARGUMENT",
                        "Goal expansion needs an approved specification change",
                    ));
                }
                nonempty(&finding.requested_change, "requestedChange")?;
            }
            let instruction = ReworkInstruction {
                result_id: text(&result, "id").into(),
                review_ids: vec![],
                gate_result_ids: vec![],
                reason: "UserRevision".into(),
                findings: changes
                    .findings
                    .iter()
                    .map(|finding| ReworkFinding {
                        criterion_id: finding.criterion_id.clone(),
                        evidence: vec![],
                        requested_change: finding.requested_change.clone(),
                    })
                    .collect(),
                preserve_artifacts: changes.preserve_artifacts.clone(),
                action: "ReviseOutput".into(),
            };
            self.validate_instruction(&result, &instruction)?;
            if !["Submitted", "Reviewing", "Accepted"].contains(&text(&result, "disposition")) {
                return Err(bad(
                    "BAD_STATE",
                    "Result already has a correction disposition",
                ));
            }
        } else {
            if candidate["specRevision"] != work["currentSpecRevision"]
                || text(&work, "desiredAdvancement") != "Advance"
            {
                return Err(bad(
                    "STALE_VERSION",
                    "Candidate/specification/advancement is not current",
                ));
            }
            let artifacts: Vec<ArtifactRef> = parse(&candidate["artifacts"])?;
            self.artifacts(&artifacts)?;
            if text(&result, "disposition") != "Accepted" || !self.acceptance_ready(&result)? {
                return Err(bad("BAD_STATE", "Candidate evidence is no longer accepted"));
            }
            if let Some(source_input) = candidate["destination"].get("sourceInput") {
                let (_, pinned) = self.inherited_delivery_code(&result)?;
                if &pinned != source_input
                    || candidate["destination"]["artifact"] != pinned["artifact"]
                    || candidate["destination"]["sourceResultId"] != pinned["sourceResultId"]
                {
                    return Err(bad(
                        "STALE_EVALUATION",
                        "Candidate code provenance no longer matches its accepted integration inputs",
                    ));
                }
            }
            let work_id = text(&work, "id");
            if self
                .related("Attempt", "workId", work_id)
                .iter()
                .any(|attempt| attempt["reservationHeld"] == true)
                || self
                    .related("Workspace", "workId", work_id)
                    .iter()
                    .any(|workspace| text(workspace, "writer") == "Human")
            {
                return Err(bad(
                    "BAD_STATE",
                    "Managed writers must settle before accepting delivery",
                ));
            }
            if self
                .related("AttentionItem", "workId", work_id)
                .iter()
                .any(|item| {
                    text(item, "status") == "Open" && text(item, "subjectId") != candidate_id
                })
            {
                return Err(bad(
                    "BAD_STATE",
                    "Completion-relevant obligations remain open",
                ));
            }
        }
        let project = self.record(text(&work, "projectId"), "Project")?;
        self.authorized_read(principal, &project)?;
        let grant = self.record(text(&work, "currentGrantId"), "ExecutionGrant")?;
        let (target, mut preview) =
            self.human_action_work_preview(&work, &project, Some(&grant))?;
        let artifacts: Vec<ArtifactRef> = parse(&candidate["artifacts"])?;
        let artifacts = artifacts
            .iter()
            .map(|artifact| self.record(&artifact.artifact_id, "Artifact"))
            .collect::<DomainResult<Vec<_>>>()?;
        preview["destination"] = candidate["destination"].clone();
        preview["candidate"] = candidate;
        preview["result"] = self.view(&result);
        preview["artifacts"] = json!(artifacts);
        if let Some(changes) = changes {
            preview["requestedChanges"] = encode(&changes)?;
        }
        Ok((target, preview))
    }

    fn human_action_project_preview(
        &self,
        request: &Request,
        invocation: &Value,
    ) -> DomainResult<(Value, Value)> {
        self.matched(request, &[])?;
        let input: ProjectConfigure = parse(&request.params)?;
        if input.project_id.is_some() {
            return Err(bad(
                "METHOD_UNSUPPORTED",
                "In-place project policy replacement requires grant migration and is not supported",
            ));
        }
        nonempty(&input.name, "name")?;
        let canonical = super::super::project_directory::resolve(
            &input.root,
            input.create_directory.unwrap_or(false),
        )
        .map_err(|error| bad("INVALID_ARGUMENT", format!("{error:#}")))?;
        let policy = self.global_policy()?;
        if policy["id"] != invocation["globalPolicyId"]
            || number(&policy, "version") != number(invocation, "globalPolicyVersion")
        {
            return Err(bad(
                "STALE_VERSION",
                "Global project proposal policy changed after invocation dispatch",
            ));
        }
        nonempty(
            text(&policy, "approvedModelDestination"),
            "approvedModelDestination",
        )?;
        let approved = [
            text(&policy, "capabilityId"),
            text(&policy, "workerCapabilityId"),
            text(&policy, "checkCapabilityId"),
        ];
        for ((capability, kind), expected) in [
            (&input.coordinator_capability_id, "Coordinate"),
            (&input.worker_capability_id, "ProduceResult"),
            (&input.check_capability_id, "EvaluateGate"),
        ]
        .into_iter()
        .zip(approved)
        {
            nonempty(capability, "capabilityId")?;
            if capability != expected {
                return Err(bad(
                    "FORBIDDEN",
                    "Project capabilities must match the explicitly approved global policy",
                ));
            }
            if self.runtime_for(capability, kind).is_none() {
                return Err(bad(
                    "CAPABILITY_UNAVAILABLE",
                    format!("Register approved runtime capability {capability} for {kind} first"),
                ));
            }
        }
        if input.capability_ids.as_ref().is_some_and(|capabilities| {
            capabilities
                .iter()
                .any(|capability| !approved.contains(&capability.as_str()))
        }) {
            return Err(bad(
                "FORBIDDEN",
                "Project proposal cannot add capabilities outside the global policy",
            ));
        }
        if input.environment_refs.as_ref().is_some_and(|environments| {
            environments.len() != 1 || environments[0] != "local-default"
        }) {
            return Err(bad(
                "FORBIDDEN",
                "Global policy has not approved additional project environments",
            ));
        }
        let approved_limits: Limits = parse(&policy["limits"])?;
        let approved_limits = encode(&approved_limits)?;
        Self::validate_limits(&input.limits)?;
        let limits = encode(&input.limits)?;
        let fields = limits
            .as_object()
            .ok_or_else(|| bad("INVALID_ARGUMENT", "Expected project limits"))?;
        for (field, value) in fields {
            let value = value.as_u64().ok_or_else(|| {
                bad(
                    "INVALID_ARGUMENT",
                    format!("Expected an integer limit: {field}"),
                )
            })?;
            let approved = approved_limits[field].as_u64().ok_or_else(|| {
                bad(
                    "INVALID_REFERENCE",
                    format!("Missing approved limit: {field}"),
                )
            })?;
            if value > approved {
                return Err(bad(
                    "FORBIDDEN",
                    format!("Project limit exceeds the approved global policy: {field}"),
                ));
            }
        }
        let messages = invocation
            .pointer("/coordinationInput/snapshot/messages")
            .and_then(Value::as_array)
            .ok_or_else(|| bad("INVALID_REFERENCE", "Missing captured human conversation"))?;
        let mut human_root = self.project_root_preference(&input, Some(invocation))?;
        for message in messages
            .iter()
            .filter(|message| text(message, "role") == "human")
        {
            if Self::human_action_explicit_root(text(message, "text"), &input.root) {
                self.human_action_source(
                    invocation,
                    text(invocation, "conversationId"),
                    text(message, "id"),
                    false,
                )?;
                human_root = true;
            }
        }
        if !human_root {
            let questions = invocation
                .pointer("/coordinationInput/snapshot/intakeRequests")
                .and_then(Value::as_array)
                .ok_or_else(|| bad("INVALID_REFERENCE", "Missing captured intake questions"))?;
            for captured in questions {
                if text(captured, "status") != "Answered"
                    || captured.get("answerSource").is_some()
                    || !Self::human_action_answer_contains_root(&captured["answer"], &input.root)
                {
                    continue;
                }
                let question = self.record(text(captured, "id"), "IntakeRequest")?;
                if &question != captured
                    || text(&question, "conversationId") != text(invocation, "conversationId")
                {
                    return Err(bad(
                        "FORBIDDEN",
                        "Directory answer is outside the captured global conversation",
                    ));
                }
                Self::validate_answer(&question["responseSchema"], &question["answer"])?;
                human_root = true;
            }
        }
        if !human_root {
            return Err(bad(
                "FORBIDDEN",
                "Project root requires a captured human directory or a captured code-root preference for a new child directory",
            ));
        }
        let root = canonical.to_string_lossy();
        let mut project = encode(&input)?;
        let mut capabilities: Vec<_> = approved.into_iter().collect();
        capabilities.sort_unstable();
        capabilities.dedup();
        project["root"] = json!(root);
        project["capabilityIds"] = json!(capabilities);
        project["environmentRefs"] = json!(["local-default"]);
        Ok((
            json!({"kind":"ProjectRoot","root":root}),
            json!({
                "project":project,
                "globalConversationPolicy":policy,
                "approvedModelDestination":policy["approvedModelDestination"]
            }),
        ))
    }

    pub(super) fn project_root_preference(
        &self,
        input: &ProjectConfigure,
        invocation: Option<&Value>,
    ) -> DomainResult<bool> {
        let Some(reference) = &input.root_preference else {
            return Ok(false);
        };
        if !input.create_directory.unwrap_or(false)
            || reference.kind != "Preference"
            || reference.version == 0
        {
            return Err(bad(
                "INVALID_ARGUMENT",
                "rootPreference requires a versioned Preference and createDirectory:true",
            ));
        }
        uuid(&reference.id)?;
        let preference = self.record(&reference.id, "Preference")?;
        if preference["status"] != "Active" || number(&preference, "version") != reference.version {
            return Err(bad(
                "STALE_VERSION",
                "Code-root preference changed or was forgotten",
            ));
        }
        if preference["scope"] != "User" || preference["key"] != "workspace.code_root" {
            return Err(bad(
                "FORBIDDEN",
                "Only the user's code-root preference supplies a new project parent",
            ));
        }
        if let Some(invocation) = invocation {
            let captured = invocation
                .pointer("/coordinationInput/snapshot/preferences/items")
                .and_then(Value::as_array);
            if captured.is_none_or(|items| !items.contains(&preference)) {
                return Err(bad(
                    "STALE_VERSION",
                    "Code-root preference was not captured by this invocation",
                ));
            }
        }
        let parent = Path::new(&input.root)
            .parent()
            .and_then(Path::to_str)
            .ok_or_else(|| {
                bad(
                    "INVALID_ARGUMENT",
                    "New project requires an absolute parent directory",
                )
            })?;
        let parent = parent.strip_prefix(r"\\?\").unwrap_or(parent);
        if !Self::human_action_explicit_root(text(&preference, "content"), parent) {
            return Err(bad(
                "FORBIDDEN",
                "Proposed project must be a direct child of the directory recorded in the code-root preference",
            ));
        }
        Ok(true)
    }

    pub(super) fn complete_project_action(&mut self, operation: &Value) -> DomainResult<()> {
        for mut proposal in self
            .all("HumanActionProposal")
            .into_iter()
            .filter(|proposal| {
                proposal["submission"]["operationId"] == operation["id"]
                    && proposal["request"]["method"] == "project.configure"
                    && proposal["status"] == "Submitted"
            })
        {
            proposal["completion"] = json!({
                "operationId":operation["id"],"status":operation["status"]
            });
            for field in ["result", "failure"] {
                if let Some(value) = operation.get(field) {
                    proposal["completion"][field] = value.clone();
                }
            }
            let proposal = self.put(proposal);
            self.emit_human_action("HumanActionCompleted", &proposal)?;
            self.queue_coordination(
                None,
                Some(text(&proposal, "conversationId")),
                "HumanActionCompleted",
                &proposal,
            )?;
        }
        Ok(())
    }

    fn human_action_explicit_root(message: &str, root: &str) -> bool {
        if message.trim() == root {
            return true;
        }
        // This checks literal path provenance, not natural-language intent. Quoted
        // paths must match in full; an existing parent or prefix is not inferred.
        message.match_indices(root).any(|(start, _)| {
            let before = message[..start].chars().next_back();
            let mut suffix = message[start + root.len()..].chars();
            let after = suffix.next();
            match before {
                Some(quote @ ('"' | '\'' | '`')) => after == Some(quote),
                None | Some(' ' | '\t' | '\r' | '\n') => {
                    !root.chars().any(char::is_whitespace)
                        && after.is_none_or(|ch| {
                            ch.is_whitespace()
                                || [',', ';', ')', ']', '。', '，', '；'].contains(&ch)
                                || (ch == '.' && suffix.next().is_none_or(char::is_whitespace))
                        })
                }
                _ => false,
            }
        })
    }

    fn human_action_answer_contains_root(answer: &Value, root: &str) -> bool {
        match answer {
            Value::String(value) => value == root,
            Value::Array(values) => values
                .iter()
                .any(|value| Self::human_action_answer_contains_root(value, root)),
            Value::Object(fields) => fields
                .values()
                .any(|value| Self::human_action_answer_contains_root(value, root)),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Engine;

    #[test]
    fn human_action_literal_root_distinguishes_sentence_period_from_path_suffix() {
        for (message, expected) in [
            (r"Please use C:\project. Ask for approval first.", true),
            (r"Please use C:\project.", true),
            ("Please use C:\\project.\nAsk for approval first.", true),
            (r"Please use C:\project.git.", false),
            (r"Please use C:\project.\nested", false),
            (r"Please use C:\project...", false),
            (r#"Please use "C:\project."."#, false),
            (r#"Please use "C:\project"."#, true),
            (r"Please use C:\project-other.", false),
        ] {
            assert_eq!(
                Engine::human_action_explicit_root(message, r"C:\project"),
                expected,
                "{message}"
            );
        }
    }
}
