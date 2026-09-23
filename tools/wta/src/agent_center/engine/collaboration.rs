// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

use super::*;

impl Engine {
    fn open_console(&mut self, input: &ConsoleOpen) -> DomainResult<Value> {
        uuid(&input.console_session_id)?;
        uuid(&input.conversation_id)?;
        if input.console_session_id == input.conversation_id {
            return Err(bad(
                "INVALID_ARGUMENT",
                "Console and conversation identities must be distinct UUIDs",
            ));
        }
        let global = input.scope.as_deref() == Some("Global");
        if input.scope.is_some() && !global {
            return Err(bad("INVALID_ARGUMENT", "Unknown conversation scope"));
        }
        if global {
            if input.project_id.is_some() {
                return Err(bad(
                    "INVALID_ARGUMENT",
                    "Global registration has no project; capture project hints on messages",
                ));
            }
        } else {
            self.record(input.project_id.as_deref().unwrap_or(""), "Project")?;
        }
        if let Some(console) = self.records.get(&input.console_session_id) {
            if text(console, "kind") != "ConsoleSession"
                || (console["scope"] == "Global") != global
                || console.get("projectId").and_then(Value::as_str) != input.project_id.as_deref()
                || text(console, "conversationId") != input.conversation_id
            {
                return Err(bad(
                    "INVALID_REFERENCE",
                    "Console identity is already bound to another project/conversation",
                ));
            }
            return Ok(console.clone());
        }
        if let Some(conversation) = self.records.get(&input.conversation_id) {
            if text(conversation, "kind") != "Conversation"
                || (conversation["scope"] == "Global") != global
                || conversation.get("projectId").and_then(Value::as_str)
                    != input.project_id.as_deref()
                || text(conversation, "consoleSessionId") != input.console_session_id
            {
                return Err(bad(
                    "INVALID_REFERENCE",
                    "Conversation identity belongs to another console/project",
                ));
            }
        } else {
            let mut conversation = json!({"id":input.conversation_id,"kind":"Conversation",
                "consoleSessionId":input.console_session_id,"messages":[],"createdAt":utc_after(0)});
            if let Some(project) = &input.project_id {
                conversation["projectId"] = json!(project);
            } else {
                conversation["scope"] = json!("Global");
                conversation["coordinationUsed"] = json!(0);
            }
            self.put(conversation);
        }
        let mut console = json!({"id":input.console_session_id,"kind":"ConsoleSession",
            "conversationId":input.conversation_id,"contextVersion":1,"createdAt":utc_after(0)});
        if let Some(project) = &input.project_id {
            console["projectId"] = json!(project);
        } else {
            console["scope"] = json!("Global");
        }
        let console = self.put(console);
        Ok(console)
    }

    pub(super) fn emit_message(&mut self, kind: &str, message: &Value) {
        if let Some(conversation) = self.records.get(text(message, "conversationId")).cloned() {
            let mut conversation = conversation;
            let mut messages = values(&conversation, "messages");
            if let Some(index) = messages.iter().position(|item| item["id"] == message["id"]) {
                messages[index] = message.clone();
            } else {
                messages.push(message.clone());
            }
            conversation["messages"] = json!(messages);
            let conversation = self.put(conversation);
            self.emit(kind, &conversation);
        } else {
            self.emit(kind, message);
        }
    }

    pub(super) fn queue_coordination(
        &mut self,
        work_id: Option<&str>,
        conversation_id: Option<&str>,
        reason: &str,
        subject: &Value,
    ) -> DomainResult<()> {
        let work_conversation = if let Some(work_id) = work_id.filter(|id| !id.is_empty()) {
            Some(self.ensure_work_conversation(work_id)?)
        } else {
            None
        };
        let conversation_id = work_conversation
            .as_ref()
            .map(|conversation| text(conversation, "id"))
            .or(conversation_id);
        let scope = work_id
            .filter(|id| !id.is_empty())
            .or(conversation_id)
            .ok_or_else(|| {
                bad(
                    "INVALID_REFERENCE",
                    "Coordination needs a work or conversation scope",
                )
            })?;
        if self.all("CoordinationTrigger").iter().any(|trigger| {
            text(trigger, "scopeId") == scope
                && text(trigger, "subjectId") == text(subject, "id")
                && number(trigger, "subjectVersion") == number(subject, "version")
                && text(trigger, "reason") == reason
                && text(trigger, "status") == "Queued"
        }) {
            return Ok(());
        }
        let mut body = json!({"scopeId":scope,"subjectId":subject["id"],"subjectKind":subject["kind"],"subjectVersion":subject["version"],
            "reason":reason,"status":"Queued","eventId":id()});
        if let Some(work_id) = work_id.filter(|id| !id.is_empty()) {
            body["workId"] = json!(work_id);
        }
        if let Some(conversation_id) = conversation_id {
            body["conversationId"] = json!(conversation_id);
        }
        let trigger = self.create("CoordinationTrigger", body);
        self.emit("CoordinationRequested", &trigger);
        Ok(())
    }

    pub(super) fn coordinate(&mut self) -> DomainResult<()> {
        let mut scopes = BTreeSet::new();
        for trigger in self.all("CoordinationTrigger") {
            if text(&trigger, "status") != "Queued"
                || !scopes.insert(text(&trigger, "scopeId").to_owned())
            {
                continue;
            }
            let scope = text(&trigger, "scopeId");
            let active = self.all("CoordinationTurn").iter().any(|turn| {
                text(turn, "scopeId") == scope
                    && self
                        .records
                        .get(text(turn, "invocationId"))
                        .is_some_and(|invocation| text(invocation, "state") != "Released")
            });
            if active {
                continue;
            }
            let work_id = text(&trigger, "workId");
            let work_conversation = if !work_id.is_empty() {
                Some(self.ensure_work_conversation(work_id)?)
            } else {
                None
            };
            let conversation_id = work_conversation
                .as_ref()
                .map(|conversation| text(conversation, "id"))
                .unwrap_or_else(|| text(&trigger, "conversationId"));
            if work_id.is_empty()
                && self.record(conversation_id, "Conversation")?["scope"] == "Global"
            {
                self.coordinate_global(&trigger)?;
                continue;
            }
            let (project, work) = if !work_id.is_empty() {
                let work = self.record(work_id, "Work")?;
                if !Self::legacy_mode(&work)
                    || text(&work, "lifecycle") != "Active"
                    || text(&work, "desiredAdvancement") != "Advance"
                    || work.get("continuationRecovery").is_some()
                    || self
                        .records
                        .get(text(&work, "workspaceId"))
                        .is_none_or(|workspace| {
                            text(workspace, "status") != "Ready"
                                || workspace["manualHold"] == true
                                || workspace["writer"] == "Human"
                        })
                    || work
                        .get("changeOperationId")
                        .and_then(Value::as_str)
                        .and_then(|id| self.records.get(id))
                        .is_some_and(|operation| text(operation, "status") != "Succeeded")
                {
                    continue;
                }
                (
                    self.record(text(&work, "projectId"), "Project")?,
                    Some(work),
                )
            } else {
                let conversation = self.record(conversation_id, "Conversation")?;
                (
                    self.record(text(&conversation, "projectId"), "Project")?,
                    None,
                )
            };
            let remaining = if let Some(work) = &work {
                number(&project["limits"], "coordinationTurns")
                    .saturating_sub(number(&work["usage"], "coordinationTurns"))
            } else {
                number(&project["limits"], "coordinationTurns")
                    .saturating_sub(number(&project, "planningUsed"))
            };
            if remaining == 0 {
                self.attention(
                    work_id,
                    scope,
                    "CoordinationAllowanceExhausted",
                    "Approve an explicit policy allowance or retain/hold work",
                );
                continue;
            }
            let Some(runtime) =
                self.runtime_for(text(&project, "coordinatorCapabilityId"), "Coordinate")
            else {
                self.attention(
                    work_id,
                    scope,
                    "CoordinatorUnavailable",
                    "Register the approved coordinator capability",
                );
                continue;
            };
            let mut triggers = self
                .related("CoordinationTrigger", "scopeId", scope)
                .into_iter()
                .filter(|trigger| text(trigger, "status") == "Queued")
                .collect::<Vec<_>>();
            // Reevaluate stale queued triggers rather than reflexively waking the coordinator.
            triggers.retain(|trigger| match text(trigger, "reason") {
                "WorkStarted" => work
                    .as_ref()
                    .is_some_and(|work| work.get("planId").is_none()),
                "ContextRequested" => self
                    .records
                    .get(text(trigger, "subjectId"))
                    .is_some_and(|request| text(request, "status") == "Open"),
                "ChangesRequested" => {
                    self.records
                        .get(text(trigger, "subjectId"))
                        .is_some_and(|result| {
                            ["ChangesRequested", "Rejected"].contains(&text(result, "disposition"))
                        })
                }
                _ => true,
            });
            if triggers.is_empty() {
                continue;
            }
            let session_reuse = if let Some(work) = &work {
                match self.primary_session_ref(work) {
                    Ok(reference) => reference,
                    Err(failure) => {
                        let failure = encode(&failure.failure)?;
                        if work.get("continuationFailure") != Some(&failure) {
                            let mut failed_work = work.clone();
                            failed_work["continuationFailure"] = failure;
                            let failed_work = self.put(failed_work);
                            self.emit("WorkChanged", &failed_work);
                        }
                        self.attention(
                            work_id,
                            work_id,
                            "SESSION_RESUME_UNAVAILABLE",
                            "Explicitly approve context reconstruction with restartSession",
                        );
                        continue;
                    }
                }
            } else {
                None
            };
            let turn_id = self
                .related("CoordinationTurn", "scopeId", scope)
                .into_iter()
                .find(|turn| text(turn, "state") == "Queued" && turn.get("invocationId").is_none())
                .map(|turn| text(&turn, "id").to_owned())
                .unwrap_or_else(id);
            let invocation_id = id();
            let mut message_body = json!({"role":"assistant","parts":[],"status":"Streaming","invocationId":invocation_id});
            if !work_id.is_empty() {
                message_body["workId"] = json!(work_id);
            }
            if !conversation_id.is_empty() {
                message_body["conversationId"] = json!(conversation_id);
            }
            let message = self.create("ConversationItem", message_body);
            let mut snapshot = if let Some(work) = &work {
                self.view(work)
            } else {
                self.view(&self.record(conversation_id, "Conversation")?)
            };
            if !conversation_id.is_empty() && !work_id.is_empty() {
                snapshot["conversation"] =
                    self.view(&self.record(conversation_id, "Conversation")?);
            }
            snapshot["preferences"] = self.memory_snapshot(Some(text(&project, "id")));
            snapshot["declinedAttempts"] = json!(triggers
                .iter()
                .filter(|trigger| text(trigger, "reason") == "ContractDeclined")
                .filter_map(|trigger| self.records.get(text(trigger, "subjectId")))
                .filter(|attempt| text(attempt, "kind") == "Attempt"
                    && text(attempt, "workId") == work_id)
                .collect::<Vec<_>>());
            snapshot["evaluationFailures"] = json!(triggers
                .iter()
                .filter(|trigger| text(trigger, "reason") == "ChangesRequested")
                .filter_map(|trigger| self.records.get(text(trigger, "subjectId")))
                .filter(|result| text(result, "kind") == "TaskResult"
                    && text(result, "workId") == work_id)
                .flat_map(|result| values(result, "evaluationFailures"))
                .collect::<Vec<_>>());
            let input = json!({"turnId":turn_id,"scope":if !work_id.is_empty(){json!({"workId":work_id,"conversationId":conversation_id})}else{json!({"conversationId":conversation_id})},
                "replyMessageId":message["id"],"snapshotVersion":work.as_ref().map_or(number(&snapshot,"version"),|work|number(work,"version")),
                "triggerEvents":triggers.iter().map(|trigger|json!({"eventId":trigger["eventId"],"kind":trigger["reason"],
                    "subject":{"kind":trigger["subjectKind"],"id":trigger["subjectId"],"version":trigger["subjectVersion"]}})).collect::<Vec<_>>(),
                "snapshot":snapshot,"remainingAllowance":remaining});
            let mut turn = json!({"id":turn_id,"kind":"CoordinationTurn","scopeId":scope,"invocationId":invocation_id,
                "replyMessageId":message["id"],"input":input,"state":"Queued","createdAt":utc_after(0)});
            let mut invocation = json!({"id":invocation_id,"kind":"Invocation","projectId":project["id"],
                "subject":{"kind":"Coordination","id":turn_id},"runtimeId":runtime["id"],"capabilityId":project["coordinatorCapabilityId"],
                "coordinationInput":input,"replyMessageId":message["id"],"bindingGeneration":1,
                "limits":{"deadlineUtc":utc_after(number(&project["limits"],"coordinationSeconds")),"remainingExecutionAllowance":remaining,"remainingContextRounds":project["limits"]["contextRounds"]},
                "availableToolNames":["memory_list","project_get","work_get","work_create_draft","work_propose_change","grant_preview","plan_propose","plan_apply",
                    "task_get","task_list","result_get","progress_get","artifact_get","artifact_read","task_answer_context","task_rework","decision_request",
                    "conversation_resolve_intents","conversation_request_input","coordination_finish"],
                "state":"Dispatching","lastSequence":0,"createdAt":utc_after(0)});
            if let Some(reference) = session_reuse {
                invocation["sessionReuseRef"] = json!(reference);
            }
            if work_id.is_empty() {
                invocation["availableToolNames"]
                    .as_array_mut()
                    .ok_or_else(|| bad("INVALID_REFERENCE", "Coordinator tool bindings missing"))?
                    .extend([json!("memory_store"), json!("memory_forget")]);
            }
            if !work_id.is_empty() {
                invocation["availableToolNames"]
                    .as_array_mut()
                    .ok_or_else(|| bad("INVALID_REFERENCE", "Coordinator tool bindings missing"))?
                    .retain(|tool| {
                        tool != "work_create_draft" && tool != "conversation_resolve_intents"
                    });
                turn["workId"] = json!(work_id);
                invocation["workId"] = json!(work_id);
            }
            if !conversation_id.is_empty() {
                turn["conversationId"] = json!(conversation_id);
            }
            self.put(turn);
            let invocation = self.put(invocation);
            for mut trigger in triggers {
                trigger["status"] = json!("Captured");
                trigger["turnId"] = json!(turn_id);
                self.put(trigger);
            }
            if let Some(mut work) = work {
                work.as_object_mut()
                    .map(|object| object.remove("restartPrimarySession"));
                work["usage"]["coordinationTurns"] =
                    json!(number(&work["usage"], "coordinationTurns") + 1);
                self.put(work);
            } else {
                let mut project = project;
                project["planningUsed"] = json!(number(&project, "planningUsed") + 1);
                self.put(project);
            }
            self.emit_message("MessageRecorded", &message);
            self.resolve_attention(scope);
            self.effect(
                "runtime.invoke",
                json!({"invocation":Self::wire_invocation(&invocation)?}),
                if work_id.is_empty() {
                    None
                } else {
                    Some(work_id)
                },
            );
        }
        Ok(())
    }

    pub(super) fn collaboration_command(
        &mut self,
        principal: &Principal,
        request: &Request,
    ) -> DomainResult<Response> {
        match request.method.as_str() {
            "console.open" => {
                Self::human(principal)?;
                self.matched(request, &[])?;
                let input: ConsoleOpen = parse(&request.params)?;
                let console = self.open_console(&input)?;
                let mut data = json!({"consoleSessionId":console["id"],
                    "conversationId":console["conversationId"],"contextVersion":console["contextVersion"],"version":console["version"]});
                if let Some(project) = console.get("projectId") {
                    data["projectId"] = project.clone();
                } else {
                    data["scope"] = json!("Global");
                }
                Ok(Response::ok("", data))
            }
            "conversation.submit" => {
                Self::human(principal)?;
                self.matched(request, &[])?;
                let input: ConversationSubmit = parse(&request.params)?;
                uuid(&input.conversation_id)?;
                uuid(&input.client_message_id)?;
                uuid(&input.context.console_session_id)?;
                if input.context.context_version == 0 {
                    return Err(bad(
                        "INVALID_ARGUMENT",
                        "Captured contextVersion must be positive",
                    ));
                }
                let global = input.context.scope.as_deref() == Some("Global");
                if input.context.scope.is_some() && !global {
                    return Err(bad("INVALID_ARGUMENT", "Unknown conversation scope"));
                }
                if let Some(project) = &input.context.project_id {
                    self.record(project, "Project")?;
                } else if !global {
                    return Err(bad(
                        "INVALID_ARGUMENT",
                        "Project-scoped intake requires a project",
                    ));
                }
                if global {
                    self.global_policy()?;
                }
                self.open_console(&ConsoleOpen {
                    console_session_id: input.context.console_session_id.clone(),
                    project_id: if global {
                        None
                    } else {
                        input.context.project_id.clone()
                    },
                    conversation_id: input.conversation_id.clone(),
                    scope: input.context.scope.clone(),
                })?;
                let conversation = self.record(&input.conversation_id, "Conversation")?;
                let scoped_work = conversation
                    .get("workId")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                if !global && input.context.selected_work_id.is_some() && scoped_work.is_none() {
                    return Err(bad(
                        "INVALID_REFERENCE",
                        "Open the selected work with work.open before submitting work-scoped chat",
                    ));
                }
                if let Some(work_id) = &scoped_work {
                    let console =
                        self.record(&input.context.console_session_id, "ConsoleSession")?;
                    if global
                        || input.context.selected_work_id.as_deref() != Some(work_id.as_str())
                        || console["selectedWorkId"] != *work_id
                        || number(&console, "contextVersion") != input.context.context_version
                    {
                        return Err(bad(
                            "INVALID_REFERENCE",
                            "Work chat requires the exact opened work context",
                        ));
                    }
                    let work = self.record(work_id, "Work")?;
                    if !["Draft", "Active"].contains(&text(&work, "lifecycle"))
                        || work["desiredAdvancement"] == "Cancel"
                    {
                        return Err(bad(
                            "BAD_STATE",
                            "Terminal or cancelling work cannot receive new execution input",
                        ));
                    }
                }
                nonempty(&input.text, "text")?;
                self.artifacts(&input.attachments)?;
                if let Some(intent) = &input.declared_intent {
                    if ![
                        "ImmediateQuestion",
                        "WorkDiscussion",
                        "NewWork",
                        "ContextAddition",
                        "ChangeProposal",
                    ]
                    .contains(&intent.as_str())
                    {
                        return Err(bad("INVALID_ARGUMENT", "Unknown declared intent"));
                    }
                    if scoped_work.is_some() && intent == "NewWork" {
                        return Err(bad(
                            "INVALID_ARGUMENT",
                            "New goals belong in global intake, not an existing work conversation",
                        ));
                    }
                }
                if let Some(work) = &input.context.selected_work_id {
                    let work = self.record(work, "Work")?;
                    if input
                        .context
                        .project_id
                        .as_deref()
                        .is_some_and(|project| text(&work, "projectId") != project)
                    {
                        return Err(bad(
                            "INVALID_REFERENCE",
                            "Selected work is not in captured project",
                        ));
                    }
                }
                if let Some(existing) = self
                    .all("ConversationItem")
                    .iter()
                    .find(|message| text(message, "clientMessageId") == input.client_message_id)
                {
                    if existing["text"] != json!(input.text)
                        || existing["context"] != encode(&input.context)?
                    {
                        return Err(bad(
                            "COMMAND_ID_REUSED",
                            "clientMessageId was reused with different captured content",
                        ));
                    }
                    return Ok(Response::ok(
                        "",
                        json!({"messageId":existing["id"],"intakeTurnId":existing["intakeTurnId"]}),
                    ));
                }
                let mut body = encode(&input)?;
                body["role"] = json!("human");
                body["status"] = json!("Complete");
                if let Some(work_id) = &scoped_work {
                    body["workId"] = json!(work_id);
                }
                let mut message = self.create("ConversationItem", body);
                if let Some(work_id) = &scoped_work {
                    let work = self.record(work_id, "Work")?;
                    if Self::executor_mode(&work) {
                        let turn = self.enqueue_executor_input(
                            &work,
                            &input.text,
                            Some(&message),
                            "HumanMessage",
                        )?;
                        message["intakeTurnId"] = turn["id"].clone();
                        let message = self.put(message);
                        self.emit_message("MessageRecorded", &message);
                        return Ok(Response::ok(
                            "",
                            json!({"messageId":message["id"],"intakeTurnId":turn["id"]}),
                        ));
                    }
                    if work.get("executionMode").is_none()
                        || work["executionMode"] == "ClaimingExecutor"
                    {
                        return Err(bad("WORK_EXECUTOR_CLAIM_REQUIRED","Claim this historical work's actual executor before submitting new execution input"));
                    }
                }
                let scope = scoped_work.as_deref().unwrap_or(&input.conversation_id);
                let queued = self
                    .related("CoordinationTurn", "scopeId", scope)
                    .into_iter()
                    .find(|turn| {
                        text(turn, "state") == "Queued" && turn.get("invocationId").is_none()
                    });
                let turn =
                    queued.unwrap_or_else(|| {
                        self.create("CoordinationTurn",json!({
                    "scopeId":scope,"conversationId":input.conversation_id,"state":"Queued"
                }))
                    });
                let intake_id = turn["id"].clone();
                message["intakeTurnId"] = intake_id.clone();
                let message = self.put(message);
                self.emit_message("MessageRecorded", &message);
                self.queue_coordination(
                    scoped_work.as_deref(),
                    Some(&input.conversation_id),
                    if scoped_work.is_some() {
                        "WorkMessage"
                    } else {
                        "IntakeMessage"
                    },
                    &message,
                )?;
                if global {
                    for mut invocation in
                        self.related("Invocation", "conversationId", &input.conversation_id)
                    {
                        if invocation["scope"] == "Global"
                            && ["Dispatching", "Running"].contains(&text(&invocation, "state"))
                        {
                            invocation["supersededByInput"] = message["id"].clone();
                            let invocation = self.put(invocation);
                            // Invoke and stop effects execute independently; wait for registration.
                            if invocation["state"] == "Running"
                                || invocation["dispatchRecorded"] == true
                            {
                                self.stop_invocation(&invocation, "NewConversationInput")?;
                            }
                        }
                    }
                }
                self.coordinate()?;
                Ok(Response::ok(
                    "",
                    json!({"messageId":message["id"],"intakeTurnId":intake_id}),
                ))
            }
            "conversation.resolve_intents" => {
                self.matched(request, &[])?;
                let input: ResolveIntents = parse(&request.params)?;
                let turn = self.coordinator(principal, None)?;
                if !text(&turn, "workId").is_empty() {
                    return Err(bad(
                        "FORBIDDEN",
                        "Work-scoped input is already bound; it cannot be resolved as new intake",
                    ));
                }
                if text(&turn, "id") != input.turn_id && !matches!(principal, Principal::Service) {
                    return Err(bad("FORBIDDEN", "Intake turn is not bound"));
                }
                let message = self.record(&input.message_id, "ConversationItem")?;
                if text(&message, "conversationId") != text(&turn, "conversationId") {
                    return Err(bad("FORBIDDEN", "Message is outside this intake"));
                }
                if input.intents.is_empty() {
                    return Err(bad(
                        "INVALID_ARGUMENT",
                        "Intake must resolve at least one intent or ask a question",
                    ));
                }
                let mut intent_ids = Vec::new();
                let mut affected = BTreeSet::new();
                for intent in input.intents {
                    if ![
                        "ImmediateQuestion",
                        "WorkDiscussion",
                        "NewWork",
                        "ContextAddition",
                        "ChangeProposal",
                    ]
                    .contains(&intent.kind.as_str())
                    {
                        return Err(bad("INVALID_ARGUMENT", "Unknown intent kind"));
                    }
                    if !text(&message, "declaredIntent").is_empty()
                        && text(&message, "declaredIntent") != intent.kind
                    {
                        return Err(bad(
                            "INVALID_ARGUMENT",
                            "Resolved intent contradicts explicit declaredIntent",
                        ));
                    }
                    if intent.kind == "NewWork" {
                        nonempty(intent.draft_goal.as_deref().unwrap_or(""), "draftGoal")?;
                    } else if intent.kind != "ImmediateQuestion" {
                        let work = self.record(intent.work_id.as_deref().unwrap_or(""), "Work")?;
                        if message["context"]["scope"] == "Global" {
                            self.authorized_read(principal, &work)?;
                        } else if work["projectId"] != message["context"]["projectId"] {
                            return Err(bad(
                                "FORBIDDEN",
                                "Resolved work is outside captured project",
                            ));
                        }
                        affected.insert(text(&work, "id").to_owned());
                    }
                    if intent.kind == "ChangeProposal" {
                        let proposal = self.record(
                            intent.change_proposal_id.as_deref().unwrap_or(""),
                            "ChangeProposal",
                        )?;
                        if proposal["workId"] != json!(intent.work_id)
                            || text(&proposal, "status") != "Proposed"
                        {
                            return Err(bad(
                                "INVALID_REFERENCE",
                                "Change intent must reference the matching pending proposal",
                            ));
                        }
                    }
                    for reference in &intent.referenced_message_ids {
                        let referenced = self.record(reference, "ConversationItem")?;
                        if message["context"]["scope"] == "Global" {
                            self.authorized_read(principal, &referenced)?;
                        }
                    }
                    let mut body = encode(&intent)?;
                    body["messageId"] = json!(input.message_id);
                    body["turnId"] = json!(input.turn_id);
                    let intent = self.create("ResolvedIntent", body);
                    intent_ids.push(intent["id"].clone());
                }
                let mut message = message;
                message["intentIds"] = json!(intent_ids);
                let message = self.put(message);
                self.emit_message("MessageRecorded", &message);
                Ok(Response::ok(
                    "",
                    json!({"intentIds":intent_ids,"affectedWorkIds":affected}),
                ))
            }
            "conversation.request_input" => {
                self.matched(request, &[])?;
                let input: IntakeInput = parse(&request.params)?;
                let turn = self.coordinator(principal, None)?;
                if text(&turn, "id") != input.turn_id
                    || text(&turn, "conversationId") != input.conversation_id
                {
                    return Err(bad("FORBIDDEN", "Question is outside bound intake"));
                }
                let message = self.record(&input.message_id, "ConversationItem")?;
                let conversation = self.record(&input.conversation_id, "Conversation")?;
                let historical_work_message = !text(&turn, "workId").is_empty()
                    && conversation["workId"] == turn["workId"]
                    && values(&conversation, "messages")
                        .iter()
                        .any(|item| item["id"] == message["id"]);
                if text(&message, "conversationId") != input.conversation_id
                    && !historical_work_message
                {
                    return Err(bad(
                        "INVALID_REFERENCE",
                        "Question source message is in another conversation",
                    ));
                }
                Self::validate_schema(&input.response_schema)?;
                nonempty(&input.question, "question")?;
                let mut body = encode(&input)?;
                body["status"] = json!("Open");
                if let Some(work_id) = turn.get("workId") {
                    body["workId"] = work_id.clone();
                }
                let question = self.create("IntakeRequest", body);
                self.emit("IntakeRequested", &question);
                let mut response = Response::needs_input(
                    "",
                    InputRequest {
                        kind: "Intake".into(),
                        id: text(&question, "id").into(),
                        version: number(&question, "version"),
                        response_schema: input.response_schema,
                    },
                );
                response.subjects = vec![Self::reference(&question)];
                Ok(response)
            }
            "conversation.answer_input" => {
                Self::human(principal)?;
                let input: AnswerIntake = parse(&request.params)?;
                let mut question = self.record(&input.request_id, "IntakeRequest")?;
                self.matched(request, &[&question])?;
                if text(&question, "status") != "Open" {
                    return Err(bad("STALE_VERSION", "Question already answered/cancelled"));
                }
                match input.action.as_str() {
                    "Answer" => {
                        let answer = input
                            .value
                            .as_ref()
                            .ok_or_else(|| bad("INVALID_ARGUMENT", "Answer requires value"))?;
                        Self::validate_answer(&question["responseSchema"], answer)?;
                    }
                    "Cancel" => {
                        if input.value.is_some() {
                            return Err(bad("INVALID_ARGUMENT", "Cancel forbids value"));
                        }
                    }
                    _ => return Err(bad("INVALID_ARGUMENT", "Unknown intake action")),
                }
                question["status"] = json!(if input.action == "Answer" {
                    "Answered"
                } else {
                    "Cancelled"
                });
                if let Some(value) = input.value {
                    question["answer"] = value;
                }
                let question = self.put(question);
                self.emit(
                    if input.action == "Answer" {
                        "IntakeAnswered"
                    } else {
                        "IntakeCancelled"
                    },
                    &question,
                );
                if let Some(work_id) = question.get("workId").and_then(Value::as_str) {
                    let work = self.record(work_id, "Work")?;
                    if Self::executor_mode(&work) {
                        self.enqueue_executor_input(
                            &work,
                            &format!("Typed input resolution: {}", question),
                            Some(&question),
                            "InputAnswered",
                        )?;
                        if question.get("answer").is_some() {
                            let answer = &question["answer"];
                            let message=self.create("ConversationItem",json!({"workId":work_id,
                                "conversationId":question["conversationId"],"role":"human","status":"Complete",
                                "text":answer.as_str().map(str::to_owned).unwrap_or_else(||answer.to_string()),
                                "attachments":[],"inputRequestId":question["id"]}));
                            self.emit_message("MessageRecorded", &message);
                        }
                        return Ok(Response::ok(
                            "",
                            json!({"requestId":question["id"],"state":question["status"]}),
                        ));
                    }
                }
                self.queue_coordination(
                    question.get("workId").and_then(Value::as_str),
                    Some(text(&question, "conversationId")),
                    "IntakeAnswered",
                    &question,
                )?;
                Ok(Response::ok(
                    "",
                    json!({"requestId":question["id"],"state":question["status"]}),
                ))
            }
            "task.report_progress" => {
                self.matched(request, &[])?;
                let input: Progress = parse(&request.params)?;
                let (dispatch, _, _) =
                    self.bound(principal, &input.dispatch_id, input.task_revision, true)?;
                for artifact in &input.artifacts {
                    self.authorized_read(
                        principal,
                        &self.record(&artifact.artifact_id, "Artifact")?,
                    )?;
                }
                self.artifacts(&input.artifacts)?;
                nonempty(&input.activity, "activity")?;
                if let Some(blocker) = &input.blocker_request_id {
                    let record = self.records.get(blocker).ok_or_else(|| {
                        bad(
                            "INVALID_REFERENCE",
                            "Progress blocker must name a recorded request",
                        )
                    })?;
                    if text(record, "workId") != text(&dispatch, "workId")
                        || !["ContextRequest", "DecisionRequest", "AttentionItem"]
                            .contains(&text(record, "kind"))
                    {
                        return Err(bad("INVALID_REFERENCE", "Blocker belongs to another scope"));
                    }
                }
                if let Some(coordination) = &input.coordination_request {
                    nonempty(&coordination.reason, "coordinationRequest.reason")?;
                    for task_id in &coordination.affected_task_ids {
                        let task = self.record(task_id, "Task")?;
                        if task["workId"] != dispatch["workId"] {
                            return Err(bad(
                                "FORBIDDEN",
                                "Coordination request targets another work",
                            ));
                        }
                    }
                }
                let mut body = encode(&input)?;
                body["workId"] = dispatch["workId"].clone();
                body["taskId"] = dispatch["taskId"].clone();
                body["attemptId"] = dispatch["attemptId"].clone();
                if serde_json::to_vec(&body).map_or(true, |bytes| bytes.len() > 65_536) {
                    return Err(bad(
                        "INVALID_ARGUMENT",
                        "Progress body exceeds 65536 bytes; capture large diagnostics as artifacts",
                    ));
                }
                let report = self.create("ProgressReport", body);
                let event_id = self.emit("ProgressReported", &report);
                if input.coordination_request.is_some() {
                    self.queue_coordination(
                        Some(text(&dispatch, "workId")),
                        None,
                        "CoordinationRequested",
                        &report,
                    )?;
                }
                Ok(Response::ok(
                    "",
                    json!({"reportId":report["id"],"eventId":event_id}),
                ))
            }
            "task.request_context" => {
                self.matched(request, &[])?;
                let input: RequestContext = parse(&request.params)?;
                let (dispatch, mut attempt, _) =
                    self.bound(principal, &input.dispatch_id, input.task_revision, true)?;
                if text(&dispatch, "dispatchKind") != "ProduceResult" {
                    return Err(bad(
                        "METHOD_UNSUPPORTED",
                        "Evaluation units do not open implementation context rounds",
                    ));
                }
                nonempty(&input.question, "question")?;
                self.artifacts(&input.inputs)?;
                match input.target.kind.as_str() {
                    "Coordinator" => {
                        if input.target.task_id.is_some() {
                            return Err(bad(
                                "INVALID_ARGUMENT",
                                "Coordinator target forbids taskId",
                            ));
                        }
                    }
                    "Task" => {
                        let target =
                            self.record(input.target.task_id.as_deref().unwrap_or(""), "Task")?;
                        if target["workId"] != dispatch["workId"] {
                            return Err(bad("FORBIDDEN", "Context target is outside work"));
                        }
                    }
                    _ => return Err(bad("INVALID_ARGUMENT", "Unknown context target")),
                }
                let work = self.record(text(&dispatch, "workId"), "Work")?;
                let grant = self.record(text(&work, "currentGrantId"), "ExecutionGrant")?;
                if number(&attempt, "contextRounds") >= number(&grant["limits"], "contextRounds") {
                    return Err(bad(
                        "BAD_STATE",
                        "Approved context-round allowance is exhausted; record an explicit coordination request",
                    ));
                }
                if input.blocking && attempt.get("waitingRequestId").is_some() {
                    return Err(bad("BAD_STATE", "Attempt already has a blocking request"));
                }
                let mut body = encode(&input)?;
                body["workId"] = dispatch["workId"].clone();
                body["taskId"] = dispatch["taskId"].clone();
                body["attemptId"] = attempt["id"].clone();
                body["invocationId"] = dispatch["invocationId"].clone();
                body["status"] = json!("Open");
                body["responseSchema"] = json!({"type":"string","minLength":1});
                let context = self.create("ContextRequest", body);
                attempt["contextRounds"] = json!(number(&attempt, "contextRounds") + 1);
                if input.blocking {
                    attempt["waitingRequestId"] = context["id"].clone();
                    attempt["state"] = json!("WaitingForContext");
                }
                self.put(attempt);
                self.emit("ContextRequested", &context);
                self.queue_coordination(
                    Some(text(&context, "workId")),
                    None,
                    "ContextRequested",
                    &context,
                )?;
                Ok(Response::needs_input(
                    "",
                    InputRequest {
                        kind: "Context".into(),
                        id: text(&context, "id").into(),
                        version: number(&context, "version"),
                        response_schema: context["responseSchema"].clone(),
                    },
                ))
            }
            "task.answer_context" => {
                let input: AnswerContext = parse(&request.params)?;
                let context = self.record(&input.request_id, "ContextRequest")?;
                self.coordinator(principal, Some(text(&context, "workId")))?;
                self.matched(request, &[&context])?;
                let (answer, delivery) = self.answer_context(&context, &input)?;
                Ok(Response::ok(
                    "",
                    json!({"answerId":answer,"delivery":delivery}),
                ))
            }
            "decision.request" => {
                let input: DecisionRequestBody = parse(&request.params)?;
                self.coordinator(principal, Some(&input.work_id))?;
                let subject = self.record(&input.subject.id, &input.subject.kind)?;
                self.matched(request, &[&subject])?;
                if number(&subject, "version") != input.subject.version
                    || (text(&subject, "workId") != input.work_id
                        && text(&subject, "id") != input.work_id)
                {
                    return Err(bad(
                        "STALE_VERSION",
                        "Decision subject changed or belongs to another work",
                    ));
                }
                if input.purpose != "TaskInput" {
                    return Err(bad(
                        "METHOD_UNSUPPORTED",
                        "Experimental decisions currently support TaskInput; scope/grant/human-gate approval requires its explicit application controller",
                    ));
                }
                closed(&input.application, &["requestId"], &[])?;
                let context =
                    self.record(text(&input.application, "requestId"), "ContextRequest")?;
                if text(&context, "workId") != input.work_id
                    || text(&context, "status") != "Open"
                    || context["blocking"] != true
                    || input.context_request_id.as_deref() != Some(text(&context, "id"))
                {
                    return Err(bad(
                        "INVALID_REFERENCE",
                        "TaskInput must link the exact open ContextRequest",
                    ));
                }
                Self::validate_schema(&input.response_schema)?;
                nonempty(&input.question, "question")?;
                let mut body = encode(&input)?;
                body["status"] = json!("Open");
                let decision = self.create("DecisionRequest", body);
                self.emit("DecisionRequested", &decision);
                self.attention(
                    &input.work_id,
                    text(&decision, "id"),
                    "Decision",
                    "Answer the recorded question with its current version",
                );
                Ok(Response::needs_input(
                    "",
                    InputRequest {
                        kind: "Decision".into(),
                        id: text(&decision, "id").into(),
                        version: number(&decision, "version"),
                        response_schema: input.response_schema,
                    },
                ))
            }
            "decision.answer" => {
                Self::human(principal)?;
                let input: DecisionAnswer = parse(&request.params)?;
                let mut decision = self.record(&input.decision_id, "DecisionRequest")?;
                self.matched(request, &[&decision])?;
                if text(&decision, "status") != "Open" {
                    return Err(bad("STALE_VERSION", "Decision already answered"));
                }
                Self::validate_answer(&decision["responseSchema"], &input.value)?;
                let context = self.record(
                    text(&decision["application"], "requestId"),
                    "ContextRequest",
                )?;
                if text(&context, "status") != "Open" {
                    return Err(bad(
                        "STALE_DISPATCH",
                        "Decision application target was superseded",
                    ));
                }
                let answer=self.create("DecisionAnswer",json!({"workId":decision["workId"],"decisionId":decision["id"],"actor":principal.key(),"value":input.value}));
                let application=self.create("DecisionApplication",json!({"workId":decision["workId"],"answerId":answer["id"],"target":decision["application"],"status":"Pending"}));
                decision["status"] = json!("Answered");
                decision["answerId"] = answer["id"].clone();
                decision["applicationId"] = application["id"].clone();
                let decision = self.put(decision);
                self.emit("DecisionAnswered", &decision);
                let body = AnswerContext {
                    request_id: text(&context, "id").into(),
                    answer: input
                        .value
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| input.value.to_string()),
                    evidence: vec![],
                    compatibility: "ExistingInputs".into(),
                };
                self.answer_context(&context, &body)?;
                Ok(Response::ok(
                    "",
                    json!({"answerId":answer["id"],"applicationId":application["id"],"state":"Recorded"}),
                ))
            }
            "coordination.finish" => {
                self.matched(request, &[])?;
                let input: CoordinationFinish = parse(&request.params)?;
                let mut turn = self.coordinator(principal, None)?;
                if text(&turn, "id") != input.turn_id || turn.get("terminalRecordId").is_some() {
                    return Err(bad(
                        "BAD_STATE",
                        "Turn identity is not active or is already finished",
                    ));
                }
                match input.outcome.as_str() {
                    "Answered" => {
                        if input.message_id.as_deref() != Some(text(&turn, "replyMessageId")) {
                            return Err(bad(
                                "INVALID_REFERENCE",
                                "Answered requires the preallocated reply message",
                            ));
                        }
                        let message =
                            self.record(text(&turn, "replyMessageId"), "ConversationItem")?;
                        if !values(&message, "parts")
                            .iter()
                            .any(|part| !text(part, "text").trim().is_empty())
                        {
                            return Err(bad(
                                "BAD_STATE",
                                "Answered requires nonempty correlated runtime text",
                            ));
                        }
                    }
                    "ActionsRecorded" => {
                        if input.command_ids.is_empty() && input.operation_ids.is_empty() {
                            return Err(bad(
                                "BAD_STATE",
                                "ActionsRecorded requires committed command receipts or pending operations",
                            ));
                        }
                        for command_id in &input.command_ids {
                            let body:Option<String>=self.db.query_row("SELECT response FROM commands WHERE principal=?1 AND command_id=?2",params![principal.key(),command_id],|row|row.get(0))
                                .optional().map_err(|error|bad("EXECUTION_FAILED",error.to_string()))?;
                            let Some(body) = body else {
                                return Err(bad(
                                    "INVALID_REFERENCE",
                                    "Coordinator finish names no committed command receipt",
                                ));
                            };
                            let response: Response = serde_json::from_str(&body)
                                .map_err(|error| bad("EXECUTION_FAILED", error.to_string()))?;
                            if !["ok", "pending", "needs_input"].contains(&response.status.as_str())
                            {
                                return Err(bad(
                                    "BAD_STATE",
                                    "Failed command is not a recorded action",
                                ));
                            }
                        }
                        for operation_id in &input.operation_ids {
                            let operation = self.record(operation_id, "Operation")?;
                            if text(&operation, "workId") != text(&turn, "workId")
                                || !["Pending", "Running", "Succeeded"]
                                    .contains(&text(&operation, "status"))
                            {
                                return Err(bad(
                                    "INVALID_REFERENCE",
                                    "Finish operation does not belong to this turn's work",
                                ));
                            }
                        }
                    }
                    "WaitingOnRecordedSubject" => {
                        let reference = input.waiting_subject.as_ref().ok_or_else(|| {
                            bad("INVALID_ARGUMENT", "Waiting requires waitingSubject")
                        })?;
                        let subject = self.record(&reference.id, &reference.kind)?;
                        if number(&subject, "version") != reference.version
                            || ![
                                "Open",
                                "Pending",
                                "Running",
                                "Queued",
                                "Ready",
                                "WaitingForContext",
                            ]
                            .contains(&text(&subject, "status"))
                                && !["Queued", "Ready", "WaitingForContext"]
                                    .contains(&text(&subject, "state"))
                        {
                            return Err(bad(
                                "BAD_STATE",
                                "Waiting subject is not currently actionable/open",
                            ));
                        }
                        if !text(&turn, "workId").is_empty()
                            && text(&subject, "workId") != text(&turn, "workId")
                        {
                            return Err(bad("FORBIDDEN", "Waiting subject is outside this work"));
                        }
                        if !text(&turn, "conversationId").is_empty()
                            && self.record(text(&turn, "conversationId"), "Conversation")?["scope"]
                                == "Global"
                        {
                            self.authorized_read(principal, &subject)?;
                        }
                    }
                    "NoActionNeeded" => {
                        let triggers =
                            self.related("CoordinationTrigger", "turnId", &input.turn_id);
                        let resolved =
                            triggers
                                .iter()
                                .all(|trigger| match text(trigger, "reason") {
                                    "WorkStarted" => self
                                        .records
                                        .get(text(trigger, "workId"))
                                        .is_some_and(|work| work.get("planId").is_some()),
                                    "ContextRequested" => self
                                        .records
                                        .get(text(trigger, "subjectId"))
                                        .is_some_and(|context| text(context, "status") != "Open"),
                                    "ChangesRequested" => self
                                        .records
                                        .get(text(trigger, "subjectId"))
                                        .is_some_and(|result| {
                                            result
                                                .get("reworkId")
                                                .and_then(Value::as_str)
                                                .and_then(|id| self.records.get(id))
                                                .is_some_and(|rework| {
                                                    text(rework, "status") == "Applied"
                                                })
                                        }),
                                    "HumanContributionRecorded" => self
                                        .records
                                        .get(text(trigger, "subjectId"))
                                        .is_some_and(|contribution| {
                                            self.records.get(text(trigger, "workId")).is_some_and(
                                                |work| {
                                                    number(work, "currentPlanRevision")
                                                        >= number(contribution, "planRevision")
                                                        && work["requiresReplan"] != true
                                                        && self
                                                            .records
                                                            .get(text(contribution, "workspaceId"))
                                                            .is_some_and(|workspace| {
                                                                workspace["lastHumanContribution"]
                                                                    == contribution["artifact"]
                                                                    && text(workspace, "writer")
                                                                        == "None"
                                                            })
                                                },
                                            )
                                        }),
                                    _ => false,
                                });
                        if !resolved {
                            return Err(bad(
                                "BAD_STATE",
                                "Captured triggering conditions still require a recorded action",
                            ));
                        }
                    }
                    _ => return Err(bad("INVALID_ARGUMENT", "Unknown coordination outcome")),
                }
                let finish = self.create("CoordinationFinish", encode(&input)?);
                turn["terminalRecordId"] = finish["id"].clone();
                turn["outcome"] = json!(input.outcome);
                self.put(turn);
                Ok(Response::ok("", json!({"finishId":finish["id"]})))
            }
            _ => Err(bad("METHOD_UNSUPPORTED", &request.method)),
        }
    }

    fn answer_context(
        &mut self,
        original: &Value,
        input: &AnswerContext,
    ) -> DomainResult<(String, &'static str)> {
        let mut context = self.record(text(original, "id"), "ContextRequest")?;
        if text(&context, "status") != "Open" {
            return Err(bad(
                "STALE_VERSION",
                "Context request was already answered/superseded",
            ));
        }
        nonempty(&input.answer, "answer")?;
        self.artifacts(&input.evidence)?;
        if !["ExistingInputs", "RequiresRevision"].contains(&input.compatibility.as_str()) {
            return Err(bad("INVALID_ARGUMENT", "Unknown compatibility"));
        }
        let attempt = self.record(text(&context, "attemptId"), "Attempt")?;
        let invocation = self.record(text(&context, "invocationId"), "Invocation")?;
        if ["Succeeded", "Failed", "Cancelled"].contains(&text(&attempt, "state"))
            || ["Releasing", "Released"].contains(&text(&invocation, "state"))
        {
            return Err(bad(
                "BAD_STATE",
                "Attempt ended; context must be recorded for a new attempt, not delivered to a replacement session",
            ));
        }
        let answer=self.create("ContextAnswer",json!({"workId":context["workId"],"requestId":context["id"],"answer":input.answer,"evidence":encode(&input.evidence)?,"compatibility":input.compatibility}));
        context["answerId"] = answer["id"].clone();
        context["answer"] = json!(input.answer);
        context["evidence"] = encode(&input.evidence)?;
        context["status"] = json!("Answered");
        let delivery = if input.compatibility == "RequiresRevision" {
            context["status"] = json!("Superseded");
            self.stop_invocation(&invocation, "Hold")?;
            self.queue_coordination(
                Some(text(&context, "workId")),
                None,
                "InputRevisionRequired",
                &context,
            )?;
            "RequiresRevision"
        } else if context["blocking"] == true {
            let dispatch = self.record(text(&context, "dispatchId"), "TaskDispatch")?;
            let continuation=self.create("Continuation",json!({"workId":context["workId"],"requestId":context["id"],"answerId":answer["id"],
                "dispatchId":dispatch["id"],"taskRevision":dispatch["taskRevision"],"bindingGeneration":invocation["bindingGeneration"],
                "answer":input.answer,"evidence":encode(&input.evidence)?,"compatibleInputManifestDigest":dispatch["inputManifestDigest"],"state":"Pending"}));
            context["continuationId"] = continuation["id"].clone();
            "Queued"
        } else {
            "RecordedForFuture"
        };
        let context = self.put(context);
        self.emit("ContextAnswered", &context);
        if delivery == "Queued" {
            self.dispatch_continuation(&invocation, &context)?;
        }
        Ok((text(&answer, "id").into(), delivery))
    }

    pub(super) fn dispatch_continuation(
        &mut self,
        invocation: &Value,
        context: &Value,
    ) -> DomainResult<()> {
        if text(invocation, "state") != "Idle" || text(context, "status") != "Answered" {
            return Ok(());
        }
        let mut continuation = self.record(text(context, "continuationId"), "Continuation")?;
        if text(&continuation, "state") != "Pending" {
            return Ok(());
        }
        let mut wire = continuation.clone();
        if let Some(object) = wire.as_object_mut() {
            for key in ["kind", "version", "createdAt", "workId", "state"] {
                object.remove(key);
            }
        }
        let continuation_payload: Continuation = parse(&wire)?;
        self.effect(
            "runtime.continue",
            json!({"invocationId":invocation["id"],"continuation":encode(&continuation_payload)?}),
            Some(text(context, "workId")),
        );
        continuation["state"] = json!("Dispatching");
        self.put(continuation);
        Ok(())
    }

    pub(super) fn validate_schema(schema: &Value) -> DomainResult<()> {
        // This explicitly bounded Draft-2020-12 subset rejects unsupported keywords, rather
        // than accepting answers against a schema the engine did not actually evaluate.
        closed(
            schema,
            &["type"],
            &[
                "$schema",
                "title",
                "description",
                "enum",
                "properties",
                "required",
                "additionalProperties",
                "items",
                "minLength",
                "maxLength",
                "minimum",
                "maximum",
                "minItems",
                "maxItems",
            ],
        )?;
        if schema
            .get("$schema")
            .and_then(Value::as_str)
            .is_some_and(|uri| uri != "https://json-schema.org/draft/2020-12/schema")
        {
            return Err(bad(
                "METHOD_UNSUPPORTED",
                "Only JSON Schema Draft 2020-12 is supported",
            ));
        }
        if !["object", "array", "string", "integer", "boolean"].contains(&text(schema, "type")) {
            return Err(bad(
                "METHOD_UNSUPPORTED",
                "Unsupported response schema type",
            ));
        }
        if text(schema, "type") == "object" {
            let properties = schema
                .get("properties")
                .and_then(Value::as_object)
                .ok_or_else(|| {
                    bad(
                        "INVALID_ARGUMENT",
                        "Object response schema requires properties",
                    )
                })?;
            if schema["additionalProperties"] != false {
                return Err(bad(
                    "METHOD_UNSUPPORTED",
                    "Object response schemas must set additionalProperties false",
                ));
            }
            for property in properties.values() {
                Self::validate_schema(property)?;
            }
            for required in values(schema, "required") {
                if required
                    .as_str()
                    .is_none_or(|name| !properties.contains_key(name))
                {
                    return Err(bad("INVALID_ARGUMENT", "Unknown required schema property"));
                }
            }
        }
        if text(schema, "type") == "array" {
            Self::validate_schema(&schema["items"])?;
        }
        Ok(())
    }

    pub(super) fn validate_answer(schema: &Value, value: &Value) -> DomainResult<()> {
        Self::validate_schema(schema)?;
        if let Some(choices) = schema.get("enum") {
            let choices = choices
                .as_array()
                .ok_or_else(|| bad("INVALID_ARGUMENT", "Schema enum must be an array"))?;
            if !choices.contains(value) {
                return Err(bad(
                    "INVALID_ARGUMENT",
                    "Answer is not an allowed enum value",
                ));
            }
        }
        let valid = match text(schema, "type") {
            "string" => value.is_string(),
            "integer" => value.is_i64() || value.is_u64(),
            "boolean" => value.is_boolean(),
            "array" => value.is_array(),
            "object" => value.is_object(),
            _ => false,
        };
        if !valid {
            return Err(bad(
                "INVALID_ARGUMENT",
                "Answer does not match responseSchema type",
            ));
        }
        if let Some(text_value) = value.as_str() {
            let length = text_value.chars().count() as u64;
            if schema
                .get("minLength")
                .and_then(Value::as_u64)
                .is_some_and(|min| length < min)
                || schema
                    .get("maxLength")
                    .and_then(Value::as_u64)
                    .is_some_and(|max| length > max)
            {
                return Err(bad(
                    "INVALID_ARGUMENT",
                    "Answer string violates schema length",
                ));
            }
        }
        if let Some(number) = value.as_i64() {
            if schema
                .get("minimum")
                .and_then(Value::as_i64)
                .is_some_and(|min| number < min)
                || schema
                    .get("maximum")
                    .and_then(Value::as_i64)
                    .is_some_and(|max| number > max)
            {
                return Err(bad(
                    "INVALID_ARGUMENT",
                    "Answer violates schema numeric bounds",
                ));
            }
        }
        if let Some(array) = value.as_array() {
            if schema
                .get("minItems")
                .and_then(Value::as_u64)
                .is_some_and(|min| array.len() < (min as usize))
                || schema
                    .get("maxItems")
                    .and_then(Value::as_u64)
                    .is_some_and(|max| array.len() > (max as usize))
            {
                return Err(bad(
                    "INVALID_ARGUMENT",
                    "Answer array violates schema length",
                ));
            }
            for item in array {
                Self::validate_answer(&schema["items"], item)?;
            }
        }
        if let Some(object) = value.as_object() {
            let properties = schema["properties"]
                .as_object()
                .ok_or_else(|| bad("INVALID_ARGUMENT", "Invalid schema properties"))?;
            for (key, value) in object {
                let property = properties
                    .get(key)
                    .ok_or_else(|| bad("INVALID_ARGUMENT", "Answer contains unknown property"))?;
                Self::validate_answer(property, value)?;
            }
            for key in values(schema, "required") {
                if !object.contains_key(key.as_str().unwrap_or("")) {
                    return Err(bad("INVALID_ARGUMENT", "Answer omits required property"));
                }
            }
        }
        Ok(())
    }
}
