// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

use super::*;

impl Engine {
    pub(super) fn executor_summary(&self, work: &Value) -> Value {
        let mut turns = self.related("WorkExecutionTurn", "workId", text(work, "id"));
        turns.sort_by_key(|turn| number(turn, "ordinal"));
        let responses: Vec<_> = turns
            .iter()
            .rev()
            .filter_map(|turn| {
                let message = self.records.get(text(turn, "replyMessageId"))?;
                if message["kind"] != "ConversationItem"
                    || message["role"] != "assistant"
                    || message["workId"] != work["id"]
                    || message["executorTurnId"] != turn["id"]
                    || message["invocationId"] != turn["invocationId"]
                {
                    return None;
                }
                let content = values(message, "parts")
                    .iter()
                    .filter_map(|part| part["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("");
                Some(json!({
                    "turnId":turn["id"],"ordinal":turn["ordinal"],"turnState":turn["state"],
                    "messageId":message["id"],"messageStatus":message["status"],
                    "text":content.chars().take(4096).collect::<String>(),
                    "textTruncated":content.chars().count() > 4096
                }))
            })
            .take(3)
            .collect();
        json!({
            "source":"RecordedExecutorConversation",
            "acceptance":"Executor replies are not independent verification or human acceptance",
            "recentResponses":responses,
            "responseLimit":3,
            "turnCount":turns.len()
        })
    }

    pub(super) fn executor_mode(work: &Value) -> bool {
        work["executionMode"] == "WorkExecutor"
    }

    pub(super) fn legacy_mode(work: &Value) -> bool {
        work.get("executionMode").is_none() || work["executionMode"] == "LegacyTasks"
    }

    pub(super) fn executor_binding(
        &self,
        principal: &Principal,
        work_id: &str,
    ) -> DomainResult<Value> {
        let Principal::Invocation { invocation_id } = principal else {
            return Err(bad(
                "FORBIDDEN",
                "An approved work executor binding is required",
            ));
        };
        let invocation = self.record(invocation_id, "Invocation")?;
        let work = self.record(work_id, "Work")?;
        let workspace = self.record(text(&work, "workspaceId"), "Workspace")?;
        if !Self::executor_mode(&work)
            || invocation["subject"]["kind"] != "Work"
            || invocation["workId"] != work_id
            || invocation["state"] != "Running"
            || workspace["executorInvocationId"] != invocation["id"]
            || !self.invocation_live(&invocation)
        {
            return Err(bad(
                "FORBIDDEN",
                "Executor does not own this work's active input and workspace",
            ));
        }
        self.validate_continue_work(&work)?;
        if work["desiredAdvancement"] != "Advance" {
            return Err(bad("FORBIDDEN", "Work execution is held or cancelled"));
        }
        if self
            .related("IntakeRequest", "workId", work_id)
            .iter()
            .any(|question| question["status"] == "Open")
        {
            return Err(bad("FORBIDDEN", "The executor is waiting for human input"));
        }
        Ok(invocation)
    }

    pub(super) fn classify_executor_exit(
        &mut self,
        mut invocation: Value,
        ended: &TurnEnded,
    ) -> DomainResult<()> {
        let work_id = text(&invocation, "workId").to_owned();
        let mut work = self.record(&work_id, "Work")?;
        let mut turn = self.record(
            text(&invocation, "currentExecutorTurnId"),
            "WorkExecutionTurn",
        )?;
        if ["Running", "Dispatching"].contains(&text(&turn, "state")) {
            turn["state"] = json!("Interrupted");
            self.put(turn);
            let mut message =
                self.record(text(&invocation, "replyMessageId"), "ConversationItem")?;
            message["status"] = json!("Interrupted");
            let message = self.put(message);
            self.emit_message("MessageCompleted", &message);
        }
        if invocation.get("stopReason").is_none() {
            work["desiredAdvancement"] = json!("Hold");
            let error = ended
                .error_text
                .as_deref()
                .unwrap_or("The executor process ended");
            work["continuationFailure"] = json!({"code":if error.contains("SESSION_RESUME_UNAVAILABLE") {"SESSION_RESUME_UNAVAILABLE"}else{"EXECUTOR_DISCONNECTED"},"message":error});
            work = self.put(work);
        }
        invocation["state"] = json!("Releasing");
        invocation["terminalDisposition"] = json!("ExecutorStopped");
        let operation = self.effect(
            "runtime.release",
            json!({"invocationId":invocation["id"],"terminalDisposition":"ExecutorStopped"}),
            Some(&work_id),
        );
        invocation["releaseOperationId"] = operation["id"].clone();
        self.put(invocation);
        self.emit("WorkChanged", &work);
        Ok(())
    }

    pub(super) fn enqueue_executor_input(
        &mut self,
        work: &Value,
        prompt: &str,
        source: Option<&Value>,
        reason: &str,
    ) -> DomainResult<Value> {
        if !Self::executor_mode(work) {
            return Err(bad(
                "WORK_EXECUTOR_CLAIM_REQUIRED",
                "This historical work requires an explicit executor claim",
            ));
        }
        if !["Draft", "Active"].contains(&text(work, "lifecycle"))
            || work["desiredAdvancement"] == "Cancel"
        {
            return Err(bad(
                "BAD_STATE",
                "Terminal work cannot accept executor input",
            ));
        }
        let turns = self.related("WorkExecutionTurn", "workId", text(work, "id"));
        if let Some(source) = source {
            if let Some(existing) = turns
                .iter()
                .find(|turn| turn["sourceId"] == source["id"] && turn["reason"] == reason)
            {
                return Ok(existing.clone());
            }
        }
        if turns
            .iter()
            .filter(|turn| turn["state"] == "Queued")
            .count()
            >= 256
        {
            return Err(bad("LIMIT_EXCEEDED", "The work input queue is full"));
        }
        let conversation = self.ensure_work_conversation(text(work, "id"))?;
        let mut body = json!({"workId":work["id"],"conversationId":conversation["id"],
            "ordinal":turns.iter().map(|turn|number(turn,"ordinal")).max().unwrap_or(0)+1,
            "state":"Queued","reason":reason,"prompt":prompt});
        if let Some(source) = source {
            body["sourceId"] = source["id"].clone();
        }
        let turn = self.create("WorkExecutionTurn", body);
        self.emit("ExecutorInputQueued", &turn);
        self.emit("WorkChanged", work);
        Ok(turn)
    }

    pub(super) fn executor_session_ref(&self, work: &Value) -> DomainResult<Option<Value>> {
        if work["restartExecutorSession"] == true {
            return Ok(None);
        }
        if let Some(session) = work.get("executorSession") {
            let source = self.record(text(session, "invocationId"), "Invocation")?;
            let project = self.record(text(work, "projectId"), "Project")?;
            let worker = source["subject"]["kind"] == "Task"
                && source["dispatch"]["kind"] == "ProduceResult"
                && source["dispatch"]["workspaceId"] == work["workspaceId"];
            if source["state"] != "Released"
                || source["lastTurnEnd"]["quiescent"] != true
                || source["workId"] != work["id"]
                || !(source["subject"]["kind"] == "Work" || worker)
                || source["capabilityId"] != project["workerCapabilityId"]
                || source["providerSessionId"] != session["providerSessionId"]
                || source["providerConfigurationDigest"] != session["providerConfigurationDigest"]
                || source["sessionCwd"] != session["cwd"]
                || text(session, "providerSessionId").is_empty()
                || text(session, "providerConfigurationDigest").is_empty()
                || text(session, "cwd").is_empty()
            {
                return Err(bad("SESSION_RESUME_UNAVAILABLE", "The actual executor session cannot be verified for this settled work, provider, and workspace"));
            }
            return Ok(Some(session.clone()));
        }
        if self
            .related("Invocation", "workId", text(work, "id"))
            .iter()
            .any(|invocation| invocation["subject"]["kind"] == "Work")
        {
            return Err(bad("SESSION_RESUME_UNAVAILABLE", "The previous executor has no verified saved session; reconstruction requires explicit consent"));
        }
        Ok(None)
    }

    pub(super) fn drive_executors(&mut self) -> DomainResult<()> {
        self.settle_executor_claims()?;
        for mut work in self.all("Work") {
            if !Self::executor_mode(&work)
                || work["lifecycle"] != "Active"
                || work["desiredAdvancement"] != "Advance"
                || work.get("continuationRecovery").is_some()
                || work.get("continuationFailure").is_some()
            {
                continue;
            }
            if self.validate_continue_work(&work).is_err() {
                continue;
            }
            let work_id = text(&work, "id").to_owned();
            let active: Vec<_> = self
                .related("Invocation", "workId", &work_id)
                .into_iter()
                .filter(|invocation| invocation["state"] != "Released")
                .collect();
            if active.iter().any(|invocation| {
                invocation["subject"]["kind"] != "Work" || invocation["state"] != "Idle"
            }) || active.len() > 1
            {
                continue;
            }
            if self
                .related("IntakeRequest", "workId", &work_id)
                .iter()
                .any(|question| question["status"] == "Open")
            {
                continue;
            }
            let Some(mut turn) = self
                .related("WorkExecutionTurn", "workId", &work_id)
                .into_iter()
                .filter(|turn| turn["state"] == "Queued")
                .min_by_key(|turn| number(turn, "ordinal"))
            else {
                continue;
            };
            let grant = self.record(text(&work, "currentGrantId"), "ExecutionGrant")?;
            if number(&work["usage"], "executionAttempts")
                >= number(&grant["limits"], "executionAttempts")
            {
                self.attention(&work_id,&work_id,"AllowanceExhausted","Approve an explicit execution allowance before admitting another executor input");
                continue;
            }
            let project = self.record(text(&work, "projectId"), "Project")?;
            let runtime = self.runtime_for(text(&project, "workerCapabilityId"), "ExecuteWork");
            let Some(runtime) = runtime else {
                self.attention(
                    &work_id,
                    &work_id,
                    "ExecutorUnavailable",
                    "Register the approved work executor capability",
                );
                continue;
            };
            let mut workspace = self.record(text(&work, "workspaceId"), "Workspace")?;
            if self.all("Attempt").iter().any(|attempt| {
                attempt["workspaceId"] == workspace["id"] && attempt["reservationHeld"] == true
            }) {
                continue;
            }
            let existing = active.first();
            if existing.is_none() {
                let occupied = self
                    .all("Invocation")
                    .iter()
                    .filter(|invocation| {
                        invocation["projectId"] == project["id"]
                            && ["Task", "Work"].contains(&text(&invocation["subject"], "kind"))
                            && invocation["state"] != "Released"
                    })
                    .count() as u64;
                if occupied >= number(&project["limits"], "concurrency") {
                    continue;
                }
            }
            let session = if existing.is_none() {
                match self.executor_session_ref(&work) {
                    Ok(session) => session,
                    Err(failure) => {
                        work["continuationFailure"] = encode(&failure.failure)?;
                        let work = self.put(work);
                        self.emit("WorkChanged", &work);
                        continue;
                    }
                }
            } else {
                None
            };
            let invocation_id = existing
                .map(|invocation| text(invocation, "id").to_owned())
                .unwrap_or_else(id);
            let message = self.create("ConversationItem",json!({"workId":work_id,"conversationId":turn["conversationId"],
                "role":"assistant","parts":[],"status":"Streaming","invocationId":invocation_id,"executorTurnId":turn["id"]}));
            turn["replyMessageId"] = message["id"].clone();
            turn["invocationId"] = json!(invocation_id);
            turn["state"] = json!("Dispatching");
            turn["deadlineUtc"] = json!(utc_after(number(&grant["limits"], "executionSeconds")));
            turn["turnNumber"] = json!(existing
                .map_or(1, |invocation| number(invocation, "lastExecutorTurnNumber")
                    + 1));
            let input = json!({"workId":work_id,"conversationId":turn["conversationId"],"workspaceId":workspace["id"],
                "turnId":turn["id"],"turnNumber":turn["turnNumber"],"replyMessageId":message["id"],"prompt":turn["prompt"],
                "deadlineUtc":turn["deadlineUtc"],"grantId":grant["id"],"specRevision":work["currentSpecRevision"],
                "work":self.view(&work),"conversation":self.view(&self.record(text(&turn,"conversationId"),"Conversation")?)});
            if let Some(mut invocation) = existing.cloned() {
                invocation["state"] = json!("Running");
                invocation["replyMessageId"] = message["id"].clone();
                invocation["currentExecutorTurnId"] = turn["id"].clone();
                invocation["limits"]["deadlineUtc"] = turn["deadlineUtc"].clone();
                self.put(invocation);
                self.effect(
                    "runtime.work_input",
                    json!({"invocationId":invocation_id,"input":input}),
                    Some(&work_id),
                );
            } else {
                let mut input = input;
                let mut invocation = json!({"id":invocation_id,"kind":"Invocation","workId":work_id,"projectId":project["id"],
                    "subject":{"kind":"Work","id":work_id},"runtimeId":runtime["id"],"capabilityId":project["workerCapabilityId"],
                    "replyMessageId":message["id"],"bindingGeneration":1,
                    "limits":{"deadlineUtc":turn["deadlineUtc"],"remainingExecutionAllowance":number(&grant["limits"],"executionAttempts")-number(&work["usage"],"executionAttempts"),
                        "remainingContextRounds":grant["limits"]["contextRounds"]},
                    "availableToolNames":["work_get","project_get","artifact_get","artifact_read","artifact_capture","work_request_input"],
                    "state":"Dispatching","lastSequence":0,"createdAt":utc_after(0)});
                if let Some(session) = session {
                    invocation["sessionReuseRef"] = session["invocationId"].clone();
                    input["sessionSource"] = session;
                }
                invocation["executorInput"] = input;
                let wire = Self::wire_invocation(&invocation)?;
                invocation["currentExecutorTurnId"] = turn["id"].clone();
                self.put(invocation);
                workspace["executorInvocationId"] = json!(invocation_id);
                self.put(workspace);
                self.effect("runtime.invoke", json!({"invocation":wire}), Some(&work_id));
            }
            self.put(turn);
            self.resolve_attention(&work_id);
            work["usage"]["executionAttempts"] =
                json!(number(&work["usage"], "executionAttempts") + 1);
            work.as_object_mut()
                .map(|fields| fields.remove("restartExecutorSession"));
            let work = self.put(work);
            self.emit_message("MessageRecorded", &message);
            self.emit("WorkChanged", &work);
        }
        Ok(())
    }

    pub(super) fn executor_command(
        &mut self,
        principal: &Principal,
        request: &Request,
    ) -> DomainResult<Response> {
        if request.method == "work.request_input" {
            self.matched(request, &[])?;
            let input: ExecutorInputRequest = parse(&request.params)?;
            let invocation = self.executor_binding(principal, &input.work_id)?;
            Self::validate_schema(&input.response_schema)?;
            nonempty(&input.question, "question")?;
            let turn = self.record(
                text(&invocation, "currentExecutorTurnId"),
                "WorkExecutionTurn",
            )?;
            let question=self.create("IntakeRequest",json!({"workId":input.work_id,"conversationId":turn["conversationId"],
                "turnId":turn["id"],"messageId":turn["replyMessageId"],"question":input.question,"responseSchema":input.response_schema,"status":"Open"}));
            self.emit("IntakeRequested", &question);
            return Ok(Response::needs_input(
                "",
                InputRequest {
                    kind: "Intake".into(),
                    id: text(&question, "id").into(),
                    version: number(&question, "version"),
                    response_schema: input.response_schema,
                },
            ));
        }
        Self::human(principal)?;
        let input: ClaimExecutor = parse(&request.params)?;
        let mut work = self.record(&input.work_id, "Work")?;
        self.matched(request, &[&work])?;
        self.validate_executor_claim(&work, input.restart_session.unwrap_or(false))?;
        work["executionMode"] = json!("ClaimingExecutor");
        work["desiredAdvancement"] = json!("Hold");
        work["executorClaim"] =
            json!({"restartSession":input.restart_session.unwrap_or(false),"status":"Settling"});
        work.as_object_mut()
            .map(|fields| fields.remove("continuationFailure"));
        let work = self.put(work);
        self.reconcile_executor_claim(&input.work_id)?;
        self.emit("WorkChanged", &work);
        self.settle_executor_claims()?;
        self.drive_executors()?;
        let work = self.record(&input.work_id, "Work")?;
        Ok(Response::ok("", self.opened_work(&work)?))
    }

    pub(super) fn validate_executor_claim(&self, work: &Value, restart: bool) -> DomainResult<()> {
        if work["lifecycle"] != "Active"
            || work["desiredAdvancement"] == "Cancel"
            || work.get("currentAcceptanceId").is_some()
        {
            return Err(bad(
                "BAD_STATE",
                "Only approved nonterminal work can claim an executor",
            ));
        }
        if Self::executor_mode(work) {
            return Err(bad("BAD_STATE", "This work already owns an executor"));
        }
        if work["executorClaim"]["status"] == "Settling" {
            return Err(bad(
                "BAD_STATE",
                "Executor claim is already settling; continue reconciles the existing claim",
            ));
        }
        if work["executorClaim"]["status"] == "NeedsConsent" && !restart {
            return Err(bad(
                "EXECUTOR_SESSION_UNAVAILABLE",
                "Explicit reconstruction consent is required",
            ));
        }
        let mut claimed = work.clone();
        claimed["executionMode"] = json!("ClaimingExecutor");
        self.validate_continue_work(&claimed)
    }

    pub(super) fn reconcile_executor_claim(&mut self, work_id: &str) -> DomainResult<()> {
        for invocation in self.related("Invocation", "workId", work_id) {
            if invocation["state"] == "Releasing" {
                self.reconcile_invocation_release(&invocation)?;
            } else if invocation["state"] != "Released" {
                self.stop_invocation(&invocation, "WorkContinuationRecovery")?;
            }
        }
        Ok(())
    }

    fn settle_executor_claims(&mut self) -> DomainResult<()> {
        for mut work in self.all("Work") {
            if work["executionMode"] != "ClaimingExecutor"
                || work["executorClaim"]["status"] != "Settling"
                || work["lifecycle"] != "Active"
                || work["desiredAdvancement"] == "Cancel"
            {
                continue;
            }
            if self
                .related("Invocation", "workId", text(&work, "id"))
                .iter()
                .any(|invocation| invocation["state"] != "Released")
            {
                continue;
            }
            let restart = work["executorClaim"]["restartSession"] == true;
            let project = self.record(text(&work, "projectId"), "Project")?;
            let mut candidates = self
                .related("Invocation", "workId", text(&work, "id"))
                .into_iter()
                .filter(|invocation| {
                    invocation["subject"]["kind"] == "Task"
                        && invocation["dispatch"]["kind"] == "ProduceResult"
                        && invocation["dispatch"]["workspaceId"] == work["workspaceId"]
                })
                .collect::<Vec<_>>();
            candidates.sort_by(|left, right| text(left, "createdAt").cmp(text(right, "createdAt")));
            let source = candidates.pop().filter(|invocation| {
                invocation["capabilityId"] == project["workerCapabilityId"]
                    && invocation["lastTurnEnd"]["quiescent"] == true
                    && !text(invocation, "providerSessionId").is_empty()
                    && !text(invocation, "providerConfigurationDigest").is_empty()
                    && !text(invocation, "sessionCwd").is_empty()
                    && self.all("Operation").iter().any(|operation| {
                        operation["effect"]["method"] == "runtime.invoke"
                            && operation["effect"]["params"]["invocation"]["id"] == invocation["id"]
                    })
            });
            if !restart && source.is_none() {
                work["executorClaim"]["status"] = json!("NeedsConsent");
                work["continuationFailure"] = json!({"code":"EXECUTOR_SESSION_UNAVAILABLE",
                    "message":"No verified actual worker session can be adopted. Explicit reconstruction consent is required; coordinator history is not executor history."});
                let work = self.put(work);
                self.emit("WorkChanged", &work);
                continue;
            }
            if let Some(coordinator) = work.get("primarySession").cloned() {
                work["legacyCoordinatorSession"] = coordinator;
            }
            if !restart {
                let source = source.ok_or_else(|| {
                    bad(
                        "EXECUTOR_SESSION_UNAVAILABLE",
                        "Actual worker session disappeared",
                    )
                })?;
                let operation = self
                    .all("Operation")
                    .into_iter()
                    .find(|operation| {
                        operation["effect"]["method"] == "runtime.invoke"
                            && operation["effect"]["params"]["invocation"]["id"] == source["id"]
                    })
                    .ok_or_else(|| {
                        bad(
                            "EXECUTOR_SESSION_UNAVAILABLE",
                            "Original worker invocation intent is missing",
                        )
                    })?;
                work["executorSession"] = json!({"invocationId":source["id"],"providerSessionId":source["providerSessionId"],
                    "capabilityId":source["capabilityId"],"providerConfigurationDigest":source["providerConfigurationDigest"],"cwd":source["sessionCwd"],
                    "legacyWorker":true,"executionIdentity":source["executionIdentity"],
                    "originalInvocation":operation["effect"]["params"]["invocation"]});
                work["primarySession"] = work["executorSession"].clone();
            } else {
                work.as_object_mut().map(|fields| {
                    fields.remove("primarySession");
                    fields.remove("executorSession");
                });
                work["restartExecutorSession"] = json!(true);
            }
            work["executionMode"] = json!("WorkExecutor");
            work["executorClaim"]["status"] = json!("Claimed");
            work["desiredAdvancement"] = json!("Advance");
            work.as_object_mut()
                .map(|fields| fields.remove("continuationFailure"));
            let work = self.put(work);
            self.enqueue_executor_input(&work,"Continue this approved work using the actual executor history and the current work brief.",None,"ExecutorClaimed")?;
            self.emit("WorkChanged", &work);
        }
        Ok(())
    }
}
