// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

use super::*;

impl Engine {
    pub(super) fn invocation_live(&self, invocation: &Value) -> bool {
        ["Dispatching", "Running", "Idle"].contains(&text(invocation, "state"))
            && invocation["health"] != "Unknown"
            && self
                .records
                .get(text(invocation, "runtimeId"))
                .is_some_and(|runtime| runtime["status"] == "Registered")
            && ((invocation["subject"]["kind"] == "Work" && invocation["state"] == "Idle")
                || time::OffsetDateTime::parse(
                    text(&invocation["limits"], "deadlineUtc"),
                    &time::format_description::well_known::Rfc3339,
                )
                .is_ok_and(|deadline| deadline > time::OffsetDateTime::now_utc()))
    }

    pub(super) fn work_continuation(&self, work: &Value) -> Value {
        let work_id = text(work, "id");
        let invocations = self.related("Invocation", "workId", work_id);
        let unsettled: Vec<_> = invocations
            .iter()
            .filter(|invocation| invocation["state"] != "Released")
            .collect();
        let questions = self.related("IntakeRequest", "workId", work_id);
        let decisions = self.related("DecisionRequest", "workId", work_id);
        let (state, reason) = if work["lifecycle"] == "Completed" {
            ("Completed", "")
        } else if work["lifecycle"] == "Cancelled" {
            ("Cancelled", "")
        } else if work["desiredAdvancement"] == "Cancel" {
            (
                "NeedsRecovery",
                "Cancellation is awaiting execution settlement",
            )
        } else if work.get("continuationRecovery").is_some()
            || (work["executionMode"] == "ClaimingExecutor"
                && work["executorClaim"]["status"] == "Settling")
            || unsettled
                .iter()
                .any(|invocation| !self.invocation_live(invocation))
        {
            (
                "NeedsRecovery",
                "Execution is expired, disconnected, or awaiting proven settlement",
            )
        } else if work["executionMode"] == "ClaimingExecutor"
            && work["executorClaim"]["status"] == "Held"
        {
            (
                "Paused",
                "Executor claim was held; an explicit continuation is required",
            )
        } else if let Some(failure) = work.get("continuationFailure") {
            ("Unavailable", text(failure, "message"))
        } else if work.get("executionMode").is_none() {
            ("Unavailable","This historical work requires an explicit actual-executor claim; a coordinator is not its execution session")
        } else if Self::executor_mode(work)
            && self
                .related("AttentionItem", "workId", work_id)
                .iter()
                .any(|item| item["status"] == "Open" && item["reason"] == "AllowanceExhausted")
        {
            ("NeedsRecovery","Execution allowance is exhausted; approve a new allowance before queued inputs can run")
        } else if questions
            .iter()
            .chain(decisions.iter())
            .any(|item| item["status"] == "Open")
            || self
                .related("ContextRequest", "workId", work_id)
                .iter()
                .any(|item| item["status"] == "Open")
        {
            (
                "WaitingForInput",
                "A recorded question or decision needs an answer",
            )
        } else if !unsettled.is_empty() {
            if Self::executor_mode(work)
                && unsettled
                    .iter()
                    .all(|invocation| invocation["state"] == "Idle")
            {
                ("Ready","The work executor is idle and retains its session, background processes, and workspace reservation")
            } else if unsettled
                .iter()
                .all(|invocation| invocation["state"] == "Dispatching")
            {
                ("Ready", "Execution dispatch is pending; provider session startup has not been confirmed")
            } else {
                ("Running", "")
            }
        } else if work["desiredAdvancement"] == "Hold" && work["lifecycle"] == "Active" {
            ("Paused", "Work advancement is held")
        } else if work["lifecycle"] == "Draft" {
            (
                "Ready",
                "Approval through work.start is required before execution",
            )
        } else if self
            .records
            .get(text(work, "projectId"))
            .is_none_or(|project| {
                self.runtime_for(
                    text(
                        project,
                        if Self::executor_mode(work) || work["executionMode"] == "ClaimingExecutor"
                        {
                            "workerCapabilityId"
                        } else {
                            "coordinatorCapabilityId"
                        },
                    ),
                    if Self::executor_mode(work) || work["executionMode"] == "ClaimingExecutor" {
                        "ExecuteWork"
                    } else {
                        "Coordinate"
                    },
                )
                .is_none()
            })
        {
            (
                "Unavailable",
                "The approved coordinator capability is not connected",
            )
        } else if self
            .related("AttentionItem", "workId", work_id)
            .iter()
            .any(|item| item["status"] == "Open")
            || self
                .related("Task", "workId", work_id)
                .iter()
                .any(|task| task["state"] == "Blocked")
        {
            (
                "NeedsRecovery",
                "Recorded execution or validation failures require coordinator recovery",
            )
        } else {
            ("Ready", "")
        };
        let mut summary = json!({"state":state});
        summary["activeResponses"] = json!(invocations
            .iter()
            .filter(|invocation| {
                ["Coordination", "Work"].contains(&text(&invocation["subject"], "kind"))
                    && ["Dispatching", "Running"].contains(&text(invocation, "state"))
                    && self.invocation_live(invocation)
            })
            .filter_map(|invocation| {
                self.records
                    .get(text(invocation, "replyMessageId"))
                    .filter(|message| {
                        message["role"] == "assistant"
                            && message["status"] == "Streaming"
                            && message["invocationId"] == invocation["id"]
                            && message["workId"] == work["id"]
                    })
                    .map(|message| {
                        json!({
                            "messageId":message["id"],
                            "deadlineUtc":invocation["limits"]["deadlineUtc"]
                        })
                    })
            })
            .collect::<Vec<_>>());
        summary["activity"] = json!(if unsettled
            .iter()
            .any(|invocation| invocation["subject"]["kind"] == "Work"
                && invocation["state"] == "Idle")
        {
            "Idle"
        } else {
            state
        });
        summary["executionMode"] = work
            .get("executionMode")
            .cloned()
            .unwrap_or(json!("LegacyTasks"));
        summary["canClaimExecutor"] = json!(
            work["lifecycle"] == "Active"
                && !Self::executor_mode(work)
                && work["desiredAdvancement"] != "Cancel"
                && (work["executionMode"] != "ClaimingExecutor"
                    || work["executorClaim"]["status"] == "Held")
        );
        summary["pendingInputCount"] = json!(self
            .related("WorkExecutionTurn", "workId", work_id)
            .iter()
            .filter(|turn| turn["state"] == "Queued")
            .count());
        let session_unavailable = work["continuationFailure"]["code"]
            == "SESSION_RESUME_UNAVAILABLE"
            || work["continuationFailure"]["code"] == "EXECUTOR_SESSION_UNAVAILABLE"
            || self.primary_session_ref(work).is_err_and(|failure| {
                failure
                    .failure
                    .is_some_and(|failure| failure.code == "SESSION_RESUME_UNAVAILABLE")
            });
        let workspace_available =
            self.records
                .get(text(work, "workspaceId"))
                .is_some_and(|workspace| {
                    workspace["status"] == "Ready"
                        && workspace["manualHold"] != true
                        && workspace["writer"] != "Human"
                });
        let provider_available = self
            .records
            .get(text(work, "projectId"))
            .is_some_and(|project| {
                self.runtime_for(
                    text(
                        project,
                        if Self::executor_mode(work) || work["executionMode"] == "ClaimingExecutor"
                        {
                            "workerCapabilityId"
                        } else {
                            "coordinatorCapabilityId"
                        },
                    ),
                    if Self::executor_mode(work) || work["executionMode"] == "ClaimingExecutor" {
                        "ExecuteWork"
                    } else {
                        "Coordinate"
                    },
                )
                .is_some()
            });
        summary["canRestartSession"] = json!(
            work["lifecycle"] == "Active"
                && work["desiredAdvancement"] != "Cancel"
                && work.get("continuationRecovery").is_none()
                && unsettled.is_empty()
                && session_unavailable
                && workspace_available
                && provider_available
                && self.validate_continue_work(work).is_ok()
                && work["executorClaim"]["status"] != "Settling"
        );
        if !reason.is_empty() {
            summary["reason"] = json!(reason);
        }
        if let Some(updated) = work.get("continuationUpdatedAt") {
            summary["updatedAt"] = updated.clone();
        }
        if let Some(operation) = unsettled.iter().find_map(|invocation| {
            let field = if invocation["state"] == "Releasing" {
                "releaseOperationId"
            } else {
                "stopOperationId"
            };
            self.records
                .get(text(invocation, field))
                .filter(|operation| {
                    ["Failed", "RepairRequired"].contains(&text(operation, "status"))
                        && operation["effect"]["params"]["invocationId"] == invocation["id"]
                        && operation["failure"].is_object()
                })
        }) {
            summary["recoveryFailure"] = operation["failure"].clone();
            summary["recoveryFailure"]["operationId"] = operation["id"].clone();
        }
        summary
    }

    /// The binding belongs to the service, not to any UI tab or terminal workspace.
    pub(super) fn ensure_work_conversation(&mut self, work_id: &str) -> DomainResult<Value> {
        let work = self.record(work_id, "Work")?;
        if let Some(conversation) = self.related("Conversation", "workId", work_id).first() {
            return Ok(conversation.clone());
        }
        let conversation_id = id();
        let console_id = id();
        let mut messages = Vec::new();
        let mut included = BTreeSet::new();
        let mut sources = values(&work["spec"], "sourceMessageIds");
        sources.extend(values(&work, "sourceMessageIds"));
        for intent in self.related("ResolvedIntent", "workId", work_id) {
            sources.push(intent["messageId"].clone());
        }
        for source in sources.iter().filter_map(Value::as_str) {
            if let Some(message) = self.records.get(source) {
                if message["kind"] == "ConversationItem"
                    && message["role"] == "human"
                    && included.insert(source.to_owned())
                {
                    messages.push(message.clone());
                    if let Some(reply) = self
                        .records
                        .get(text(message, "intakeTurnId"))
                        .and_then(|turn| self.records.get(text(turn, "replyMessageId")))
                        .filter(|reply| {
                            reply["kind"] == "ConversationItem" && reply["role"] == "assistant"
                        })
                    {
                        if included.insert(text(reply, "id").to_owned()) {
                            messages.push(reply.clone());
                        }
                    }
                }
            }
        }
        for mut message in self.related("ConversationItem", "workId", work_id) {
            let main = message["role"] == "human"
                || self
                    .records
                    .get(text(&message, "invocationId"))
                    .is_some_and(|invocation| {
                        invocation["subject"]["kind"] == "Coordination"
                            && invocation["workId"] == work_id
                    });
            if main {
                message["conversationId"] = json!(conversation_id);
                let message = self.put(message);
                if included.insert(text(&message, "id").to_owned()) {
                    messages.push(message);
                }
            }
        }
        messages.sort_by(|a, b| text(a, "createdAt").cmp(text(b, "createdAt")));
        self.put(json!({"id":console_id,"kind":"ConsoleSession","conversationId":conversation_id,
            "projectId":work["projectId"],"selectedWorkId":work_id,"contextVersion":1,"createdAt":utc_after(0)}));
        Ok(self.put(json!({"id":conversation_id,"kind":"Conversation","workId":work_id,
            "consoleSessionId":console_id,"projectId":work["projectId"],"messages":messages,"createdAt":utc_after(0)})))
    }

    pub(super) fn opened_work(&mut self, work: &Value) -> DomainResult<Value> {
        let conversation = self.ensure_work_conversation(text(work, "id"))?;
        let console = self.record(text(&conversation, "consoleSessionId"), "ConsoleSession")?;
        Ok(
            json!({"workView":self.view(work),"conversation":self.view(&conversation),
            "context":{"consoleSessionId":console["id"],"conversationId":conversation["id"],
                "contextVersion":console["contextVersion"],"projectId":work["projectId"],"selectedWorkId":work["id"]},
            "continuation":self.work_continuation(work)}),
        )
    }

    pub(super) fn validate_continue_work(&self, work: &Value) -> DomainResult<()> {
        if work["lifecycle"] != "Active" || work["desiredAdvancement"] == "Cancel" {
            return Err(bad(
                "BAD_STATE",
                "Only approved, nonterminal work can be continued",
            ));
        }
        let grant = self.record(text(work, "currentGrantId"), "ExecutionGrant")?;
        let project = self.record(text(work, "projectId"), "Project")?;
        if grant["workId"] != work["id"]
            || grant["status"] != "Effective"
            || grant["specRevision"] != work["currentSpecRevision"]
            || grant["policyRevision"] != project["policyRevision"]
            || !values(&grant, "allowedCapabilities").contains(
                &project[if Self::executor_mode(work) || work["executionMode"] == "ClaimingExecutor"
                {
                    "workerCapabilityId"
                } else {
                    "coordinatorCapabilityId"
                }],
            )
        {
            return Err(bad(
                "FORBIDDEN",
                "The current work approval no longer matches its spec and policy",
            ));
        }
        let workspace = self.record(text(work, "workspaceId"), "Workspace")?;
        if workspace["status"] != "Ready"
            || workspace["manualHold"] == true
            || workspace["writer"] == "Human"
        {
            return Err(bad(
                "BAD_STATE",
                "The work workspace is not available to the service; complete handback first",
            ));
        }
        Ok(())
    }

    pub(super) fn primary_session_ref(&self, work: &Value) -> DomainResult<Option<String>> {
        if Self::executor_mode(work) {
            return self
                .executor_session_ref(work)
                .map(|session| session.map(|session| text(&session, "invocationId").to_owned()));
        }
        let previous = self.related("Invocation", "workId", text(work, "id"));
        if work["restartPrimarySession"] == true {
            return Ok(None);
        }
        if let Some(session) = work.get("primarySession") {
            let invocation = self.record(text(session, "invocationId"), "Invocation")?;
            let project = self.record(text(work, "projectId"), "Project")?;
            if invocation["state"] != "Released"
                || (invocation["lastTurnEnd"]["quiescent"] != true
                    && invocation["releaseKind"] != "CoordinatorAuthorityRevoked")
                || invocation["workId"] != work["id"]
                || invocation["subject"]["kind"] != "Coordination"
                || invocation["providerSessionId"] != session["providerSessionId"]
                || invocation["capabilityId"] != session["capabilityId"]
                || invocation["providerConfigurationDigest"]
                    != session["providerConfigurationDigest"]
                || invocation["sessionCwd"] != session["cwd"]
                || session["capabilityId"] != project["coordinatorCapabilityId"]
                || text(session, "providerSessionId").is_empty()
                || text(session, "providerConfigurationDigest").is_empty()
                || text(session, "cwd").is_empty()
            {
                return Err(bad("SESSION_RESUME_UNAVAILABLE", "The saved primary session cannot be verified against this settled work and provider"));
            }
            return Ok(Some(text(session, "invocationId").to_owned()));
        }
        if previous
            .iter()
            .any(|invocation| invocation["subject"]["kind"] == "Coordination")
        {
            return Err(bad("SESSION_RESUME_UNAVAILABLE", "This historical work has no verified primary session; explicit restartSession approval is required"));
        }
        Ok(None)
    }

    pub(super) fn work_continuation_command(
        &mut self,
        principal: &Principal,
        request: &Request,
    ) -> DomainResult<Response> {
        Self::human(principal)?;
        if request.method == "work.open" {
            closed(&request.params, &["workId"], &[])?;
            self.matched(request, &[])?;
            let work = self.record(text(&request.params, "workId"), "Work")?;
            return Ok(Response::ok("", self.opened_work(&work)?));
        }
        let input: WorkContinue = parse(&request.params)?;
        let mut work = self.record(&input.work_id, "Work")?;
        self.matched(request, &[&work])?;
        if work["executionMode"] == "ClaimingExecutor"
            && work["executorClaim"]["status"] == "Settling"
        {
            self.validate_continue_work(&work)?;
            self.reconcile_executor_claim(&input.work_id)?;
            return Ok(Response::ok("", self.opened_work(&work)?));
        }
        if work["executionMode"] == "ClaimingExecutor"
            && (input.restart_session == Some(true) || work["executorClaim"]["status"] == "Held")
        {
            let mut claim = request.clone();
            claim.method = "work.claim_executor".into();
            return self.executor_command(principal, &claim);
        }
        if work.get("executionMode").is_none() || work["executionMode"] == "ClaimingExecutor" {
            return Err(bad("WORK_EXECUTOR_CLAIM_REQUIRED","Claim the historical work's actual executor; a coordinator is not an executor session"));
        }
        self.validate_continue_work(&work)?;
        let unsettled: Vec<_> = self
            .related("Invocation", "workId", &input.work_id)
            .into_iter()
            .filter(|invocation| invocation["state"] != "Released")
            .collect();
        if !Self::executor_mode(&work)
            && input.restart_session != Some(true)
            && unsettled.len() == 1
            && self.can_recover_coordinator(&unsettled[0])
        {
            self.recover_coordinator(&mut work, &unsettled[0])?;
            return Ok(Response::ok("", self.opened_work(&work)?));
        }
        if !unsettled.is_empty()
            && unsettled
                .iter()
                .all(|invocation| self.invocation_live(invocation))
        {
            return Ok(Response::ok("", self.opened_work(&work)?));
        }
        if work.get("continuationRecovery").is_some() {
            for invocation in &unsettled {
                if invocation["state"] == "Releasing" {
                    self.reconcile_invocation_release(invocation)?;
                } else {
                    self.stop_invocation(invocation, "WorkContinuationRecovery")?;
                }
            }
            return Ok(Response::ok("", self.opened_work(&work)?));
        }
        if !unsettled.is_empty() {
            let mut stops = Vec::new();
            for invocation in unsettled {
                if invocation["state"] == "Releasing" {
                    self.reconcile_invocation_release(&invocation)?;
                } else {
                    stops.push(self.stop_invocation(&invocation, "WorkContinuationRecovery")?);
                }
            }
            work["continuationRecovery"] = json!({"restartSession":input.restart_session.unwrap_or(false),"stopOperationIds":stops});
            work["continuationUpdatedAt"] = json!(utc_after(0));
            let work = self.put(work);
            self.emit("WorkChanged", &work);
            return Ok(Response::ok("", self.opened_work(&work)?));
        }
        self.prepare_work_continuation(work, input.restart_session.unwrap_or(false))?;
        self.drive_executors()?;
        self.coordinate()?;
        let work = self.record(&input.work_id, "Work")?;
        Ok(Response::ok("", self.opened_work(&work)?))
    }

    fn can_recover_coordinator(&self, invocation: &Value) -> bool {
        invocation["subject"]["kind"] == "Coordination"
            && invocation["state"] != "Released"
            && invocation.get("dispatch").is_none()
            && !text(invocation, "providerSessionId").is_empty()
            && self
                .records
                .get(text(invocation, "runtimeId"))
                .is_some_and(|runtime| runtime["status"] != "Registered")
    }

    fn recover_coordinator(&mut self, work: &mut Value, invocation: &Value) -> DomainResult<()> {
        if work["continuationRecovery"]["coordinatorOperationId"]
            .as_str()
            .and_then(|id| self.records.get(id))
            .is_some_and(|operation| ["Pending", "Running"].contains(&text(operation, "status")))
        {
            return Ok(());
        }
        let original: String = self.db.query_row(
            "SELECT body FROM effects WHERE method='runtime.invoke' AND json_extract(body,'$.invocation.id')=?1",
            [text(invocation, "id")],
            |row| row.get(0),
        ).map_err(|error| bad("SESSION_RESUME_UNAVAILABLE", format!("Original invocation receipt is unavailable: {error}")))?;
        let original: Value = serde_json::from_str(&original)
            .map_err(|error| bad("SESSION_RESUME_UNAVAILABLE", error.to_string()))?;
        let operation = self.effect(
            "runtime.recover_coordinator",
            json!({
                "invocation":original["invocation"],
                "providerSessionId":invocation["providerSessionId"],
                "executionIdentity":invocation["executionIdentity"],
                "workId":work["id"],
                "workspaceId":work["workspaceId"]
            }),
            Some(text(work, "id")),
        );
        work["continuationRecovery"] = json!({
            "restartSession":false,"stopOperationIds":[],
            "coordinatorOperationId":operation["id"]
        });
        work["continuationUpdatedAt"] = json!(utc_after(0));
        *work = self.put(work.clone());
        self.emit("WorkChanged", work);
        Ok(())
    }

    pub(super) fn recovered_coordinator(
        &mut self,
        params: &Value,
        data: &Value,
    ) -> DomainResult<()> {
        let mut invocation = self.record(text(&params["invocation"], "id"), "Invocation")?;
        let mut work = self.record(text(params, "workId"), "Work")?;
        if !self.can_recover_coordinator(&invocation)
            || invocation["workId"] != work["id"]
            || data["workId"] != work["id"]
            || data["capabilityId"] != invocation["capabilityId"]
            || data["providerSessionId"] != invocation["providerSessionId"]
            || text(data, "providerConfigurationDigest").is_empty()
            || !Path::new(text(data, "cwd")).is_absolute()
        {
            return Err(bad(
                "SESSION_RESUME_UNAVAILABLE",
                "Recovered session does not match the disconnected coordinator",
            ));
        }
        // Revoke a non-writing coordinator's tool binding, not a worker's writer reservation.
        // Physical process settlement remains explicitly unknown.
        invocation["state"] = json!("Released");
        invocation["releaseKind"] = json!("CoordinatorAuthorityRevoked");
        invocation["processSettlement"] = json!("Unknown");
        invocation["providerConfigurationDigest"] = data["providerConfigurationDigest"].clone();
        invocation["sessionCwd"] = data["cwd"].clone();
        let invocation = self.put(invocation);
        let mut turn = self.record(text(&invocation["subject"], "id"), "CoordinationTurn")?;
        turn["state"] = json!("Superseded");
        turn["endReason"] = json!("CoordinatorAuthorityRevoked");
        let turn = self.put(turn);
        self.emit("CoordinationFinished", &turn);
        if let Some(message_id) = invocation["replyMessageId"].as_str() {
            let mut message = self.record(message_id, "ConversationItem")?;
            if message["status"] == "Streaming" {
                message["status"] = json!("Interrupted");
                let message = self.put(message);
                self.emit_message("MessageCompleted", &message);
            }
        }
        for operation in self.all("Operation").into_iter().filter(|operation| {
            ["runtime.stop", "runtime.release"].contains(&text(&operation["effect"], "method"))
                && operation["effect"]["params"]["invocationId"] == invocation["id"]
        }) {
            self.resolve_attention(text(&operation, "id"));
        }
        work["primarySession"] = data.clone();
        work["primarySession"]["invocationId"] = invocation["id"].clone();
        let work = self.put(work);
        self.emit("WorkChanged", &work);
        Ok(())
    }

    fn prepare_work_continuation(&mut self, mut work: Value, restart: bool) -> DomainResult<()> {
        self.validate_continue_work(&work)?;
        if restart {
            let field = if Self::executor_mode(&work) {
                "restartExecutorSession"
            } else {
                "restartPrimarySession"
            };
            work[field] = json!(true);
        }
        self.primary_session_ref(&work)?;
        work["desiredAdvancement"] = json!("Advance");
        work["continuationUpdatedAt"] = json!(utc_after(0));
        if let Some(object) = work.as_object_mut() {
            object.remove("continuationRecovery");
            object.remove("continuationFailure");
        }
        let work = self.put(work);
        self.emit("WorkChanged", &work);
        if Self::executor_mode(&work) {
            if !self
                .related("WorkExecutionTurn", "workId", text(&work, "id"))
                .iter()
                .any(|turn| turn["state"] == "Queued")
            {
                self.enqueue_executor_input(
                    &work,
                    "Continue the approved work in its saved executor session.",
                    None,
                    "WorkContinued",
                )?;
            }
            Ok(())
        } else {
            self.queue_coordination(Some(text(&work, "id")), None, "WorkContinued", &work)
        }
    }

    pub(super) fn settle_work_continuations(&mut self) -> DomainResult<()> {
        for mut work in self.all("Work") {
            let Some(recovery) = work.get("continuationRecovery").cloned() else {
                continue;
            };
            if work["lifecycle"] != "Active" || work["desiredAdvancement"] == "Cancel" {
                work.as_object_mut()
                    .map(|object| object.remove("continuationRecovery"));
                self.put(work);
                continue;
            }
            if self
                .related("Invocation", "workId", text(&work, "id"))
                .iter()
                .any(|invocation| invocation["state"] != "Released")
            {
                continue;
            }
            if let Err(failure) =
                self.prepare_work_continuation(work.clone(), recovery["restartSession"] == true)
            {
                work.as_object_mut()
                    .map(|object| object.remove("continuationRecovery"));
                work["continuationFailure"] = encode(&failure.failure)?;
                work["desiredAdvancement"] = json!("Hold");
                let work = self.put(work);
                self.emit("WorkChanged", &work);
            }
        }
        Ok(())
    }
}
