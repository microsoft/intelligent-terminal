// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

use super::*;

impl Engine {
    pub(super) fn artifacts(&self, refs: &[ArtifactRef]) -> DomainResult<()> {
        for reference in refs {
            let artifact = self.record(&reference.artifact_id, "Artifact")?;
            let unavailable = |message: String| {
                let mut response = bad("ARTIFACT_UNAVAILABLE", message);
                response.subjects = vec![Self::reference(&artifact)];
                if let Some(failure) = &mut response.failure {
                    failure.subjects = response.subjects.clone();
                }
                response
            };
            if text(&artifact, "digest") != reference.digest
                || text(&artifact, "availability") != "Ready"
            {
                return Err(unavailable(
                    "Artifact is missing, unavailable or has a different digest".into(),
                ));
            }
            let digest = reference.digest.strip_prefix("sha256:").unwrap_or("");
            if digest.len() != 64
                || !digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return Err(bad("INVALID_ARGUMENT", "Invalid artifact SHA-256 digest"));
            }
            // The engine actor runs on its blocking worker, not a Tokio executor.
            super::super::runtime::artifacts::verify(&artifact).map_err(|error| {
                unavailable(format!("Immutable artifact verification failed: {error:#}"))
            })?;
        }
        Ok(())
    }

    pub(super) fn read_artifact(record: &Value, params: &Value) -> DomainResult<Value> {
        let offset = params.get("offset").map_or(Ok(0), |value| {
            value
                .as_u64()
                .ok_or_else(|| bad("INVALID_ARGUMENT", "offset must be a nonnegative integer"))
        })?;
        let limit = params.get("limit").map_or(Ok(16_384), |value| {
            value
                .as_u64()
                .filter(|limit| (1..=32_768).contains(limit))
                .ok_or_else(|| bad("INVALID_ARGUMENT", "limit must be 1..32768 bytes"))
        })?;
        let relative_path = params
            .get("relativePath")
            .map(|value| {
                value.as_str().ok_or_else(|| {
                    bad(
                        "INVALID_ARGUMENT",
                        "relativePath must be a captured member path",
                    )
                })
            })
            .transpose()?;
        super::super::runtime::read_artifact(record, relative_path, offset, limit)
            .map_err(|error| bad("ARTIFACT_UNAVAILABLE", format!("{error:#}")))
    }

    pub(super) fn bound(
        &self,
        principal: &Principal,
        dispatch_id: &str,
        revision: u64,
        require_ack: bool,
    ) -> DomainResult<(Value, Value, Value)> {
        let Principal::Invocation { invocation_id } = principal else {
            return Err(bad(
                "FORBIDDEN",
                "Work tools require an out-of-band invocation binding",
            ));
        };
        let invocation = self.record(invocation_id, "Invocation")?;
        let dispatch = self.record(dispatch_id, "TaskDispatch")?;
        if text(&dispatch, "invocationId") != invocation_id {
            return Err(bad("FORBIDDEN", "Dispatch is not bound to this invocation"));
        }
        let attempt = self.record(text(&dispatch, "attemptId"), "Attempt")?;
        let task = self.record(text(&dispatch, "taskId"), "Task")?;
        let work = self.record(text(&dispatch, "workId"), "Work")?;
        if revision != number(&dispatch, "taskRevision")
            || revision != number(&task, "revision")
            || ["Released", "Ended", "Releasing", "Stopping"].contains(&text(&invocation, "state"))
            || text(&work, "desiredAdvancement") != "Advance"
        {
            return Err(bad(
                "STALE_DISPATCH",
                "Dispatch is obsolete, stopped, held or terminal",
            ));
        }
        if require_ack
            && (attempt["acknowledged"] != true || text(&attempt, "state") == "WaitingForContext")
        {
            return Err(bad(
                "CONTRACT_UNACKNOWLEDGED",
                "Acknowledge the current dispatch/continuation before work",
            ));
        }
        Ok((dispatch, attempt, invocation))
    }

    pub(super) fn runtime_for(&self, capability_id: &str, kind: &str) -> Option<Value> {
        self.all("Runtime").into_iter().find(|runtime| {
            text(runtime, "status") == "Registered"
                && values(runtime, "capabilities").iter().any(|capability| {
                    text(capability, "id") == capability_id
                        && values(capability, "kinds").contains(&json!(kind))
                        && capability["supportsScopedStop"] == true
                        && (kind == "EvaluateGate" || capability["supportsContinuation"] == true)
                })
        })
    }

    pub(super) fn admit_task(
        &mut self,
        stale_work: &Value,
        task: &Value,
        inputs: Vec<Value>,
        unit: Option<&Value>,
    ) -> DomainResult<()> {
        let work_id = text(stale_work, "id");
        let mut work = self.record(work_id, "Work")?;
        if !Self::legacy_mode(&work) {
            return Ok(());
        }
        let project = self.record(text(&work, "projectId"), "Project")?;
        let grant = self.record(text(&work, "currentGrantId"), "ExecutionGrant")?;
        if text(&grant, "status") != "Effective" {
            return Err(bad("FORBIDDEN", "Effective execution grant was revoked"));
        }
        let category = if unit.is_some() {
            "evaluationAttempts"
        } else {
            "executionAttempts"
        };
        let occupied = self
            .all("Invocation")
            .iter()
            .filter(|invocation| {
                text(invocation, "projectId") == text(&project, "id")
                    && ["Task", "Work"].contains(&text(&invocation["subject"], "kind"))
                    && text(invocation, "state") != "Released"
            })
            .count() as u64;
        if occupied >= number(&project["limits"], "concurrency") {
            return Ok(());
        }
        if number(&work["usage"], category) >= number(&grant["limits"], category) {
            self.attention(
                work_id,
                text(task, "id"),
                "AllowanceExhausted",
                "Approve an explicit new allowance or hold/cancel work",
            );
            return Ok(());
        }
        let kind = unit.map_or("ProduceResult", |unit| text(unit, "dispatchKind"));
        let capability = if kind == "EvaluateGate" {
            text(&project, "checkCapabilityId")
        } else if kind == "ReviewResult" {
            text(&task["reviewPolicy"], "reviewerCapabilityId")
        } else {
            text(task, "capabilityId")
        };
        let Some(runtime) = self.runtime_for(capability, kind) else {
            self.attention(work_id,text(task,"id"),"CapabilityUnavailable","Register the approved capability with required tools, continuation and scoped stop");
            return Ok(());
        };
        let workspace_id = text(&task["resourceRequirements"], "workspaceId");
        let workspace = self.record(workspace_id, "Workspace")?;
        if text(&workspace, "status") != "Ready" || text(&workspace, "writer") == "Human" {
            return Ok(());
        }
        if workspace
            .get("executorInvocationId")
            .and_then(Value::as_str)
            .and_then(|id| self.records.get(id))
            .is_some_and(|invocation| invocation["state"] != "Released")
        {
            return Ok(());
        }
        // A physical workspace cannot be shared with a live writer, even for a read/check.
        let conflicts = self.all("Attempt").iter().any(|attempt| {
            text(attempt, "workspaceId") == workspace_id
                && attempt["reservationHeld"] == true
                && (text(&task["resourceRequirements"], "mode") == "ExclusiveWrite"
                    || text(attempt, "mode") == "ExclusiveWrite")
        });
        if conflicts {
            return Ok(());
        }
        let attempt_id = id();
        let dispatch_id = id();
        let invocation_id = id();
        let context = self.create("ContextSnapshot",json!({"workId":work_id,"taskId":task["id"],"inputs":inputs,
            "specRevision":work["currentSpecRevision"],"planRevision":work["currentPlanRevision"],"effectiveGrantId":grant["id"]}));
        let input_digest = digest(&json!(inputs))?;
        let contract_digest = digest(&task["contract"])?;
        let mut dispatch = json!({"id":dispatch_id,"kind":"TaskDispatch","dispatchKind":kind,
            "workId":work_id,"taskId":task["id"],"taskRevision":task["revision"],
            "attemptId":attempt_id,"invocationId":invocation_id,
            "role":if kind=="EvaluateGate" {"Check"} else if kind=="ReviewResult" {"Review"} else {text(task,"role")},
            "objective":task["objective"],"scope":task["scope"],"exclusions":task["exclusions"],
            "inputs":inputs,"inputManifestDigest":input_digest,"contextSnapshotId":context["id"],
            "workspaceId":workspace_id,"outputs":task["outputs"],"criteria":task["criteria"],
            "gateDefinitions":task["gateDefinitions"],"reviewPolicy":task["reviewPolicy"],"effectiveGrantId":grant["id"],
            "contractDigest":contract_digest,"reportingContractVersion":1,"createdAt":utc_after(0)});
        if text(task, "role") == "Integration" {
            dispatch["integrationBase"] = json!({"headGeneration":number(&work,"headGeneration")});
            if let Some(parent) = work.get("integrationResultId") {
                dispatch["integrationBase"]["parentResultId"] = parent.clone();
            }
        }
        if let Some(rework) = self.records.get(text(task, "reworkId")) {
            dispatch["rework"] = rework["instruction"].clone();
        }
        if let Some(unit) = unit {
            let result = self.record(text(unit, "resultId"), "TaskResult")?;
            dispatch["outputs"] = self.producing_dispatch(&result)?["outputs"].clone();
            dispatch["subjectResultId"] = unit["resultId"].clone();
            dispatch["evaluationRound"] = unit["evaluationRound"].clone();
            dispatch["evaluationUnitId"] = unit["id"].clone();
            if let Some(gate) = unit.get("gateDefinitionId") {
                dispatch["gateDefinitionId"] = gate.clone();
            }
            if let Some(manifest) = unit.get("evidenceManifestDigest") {
                dispatch["evidenceManifestDigest"] = manifest.clone();
            }
            // Evaluation manifests bind the original input layer and the exact submitted output layer.
            dispatch["inputManifestDigest"] = unit["inputManifestDigest"].clone();
        }
        let dispatch = self.put(dispatch);
        let attempt = self.put(json!({"id":attempt_id,"kind":"Attempt","workId":work_id,"taskId":task["id"],
            "dispatchId":dispatch_id,"invocationId":invocation_id,"workspaceId":workspace_id,
            "mode":if unit.is_some() {"ReadOnly"} else {text(&task["resourceRequirements"],"mode")},
            "state":"Dispatching","acknowledged":false,"reservationHeld":true,"contextRounds":0,
            "specRevision":work["currentSpecRevision"],"planRevision":work["currentPlanRevision"],"createdAt":utc_after(0)}));
        let mut wire_dispatch = dispatch.clone();
        wire_dispatch["kind"] = json!(kind);
        wire_dispatch.as_object_mut().map(|object| {
            object.remove("dispatchKind");
            object.remove("version");
            object.remove("createdAt");
        });
        let message = self.create("ConversationItem",json!({"workId":work_id,"role":"worker","parts":[],"status":"Streaming","invocationId":invocation_id}));
        let invocation = self.put(json!({"id":invocation_id,"kind":"Invocation","workId":work_id,"projectId":work["projectId"],
            "subject":{"kind":"Task","id":attempt_id},"runtimeId":runtime["id"],"capabilityId":capability,
            "dispatch":wire_dispatch,"transcriptMessageId":message["id"],"bindingGeneration":1,
            "limits":{"deadlineUtc":utc_after(number(&grant["limits"],"executionSeconds")),
                "remainingExecutionAllowance":number(&grant["limits"],category)-number(&work["usage"],category),
                "remainingContextRounds":grant["limits"]["contextRounds"]},
            "availableToolNames":if kind=="EvaluateGate" {vec!["task_acknowledge","artifact_capture","gate_submit"]}
                else if kind=="ReviewResult" {vec!["task_acknowledge","artifact_get","result_get","task_get","review_submit","task_report_progress"]}
                else {vec!["task_acknowledge","task_get","task_report_progress","task_request_context","artifact_capture","artifact_get","result_submit"]},
            "state":"Dispatching","lastSequence":0,"createdAt":utc_after(0)}));
        if let Some(unit) = unit {
            let mut unit = unit.clone();
            unit["state"] = json!("Running");
            unit["attemptId"] = attempt["id"].clone();
            self.put(unit);
        } else {
            let mut task = task.clone();
            task["state"] = json!("Running");
            task["currentAttemptId"] = attempt["id"].clone();
            self.put(task);
        }
        work["usage"][category] = json!(number(&work["usage"], category) + 1);
        let work = self.put(work);
        self.emit("DispatchCreated", &dispatch);
        self.emit("WorkChanged", &work);
        let wire_invocation = Self::wire_invocation(&invocation)?;
        self.effect(
            "runtime.invoke",
            json!({"invocation":wire_invocation}),
            Some(work_id),
        );
        Ok(())
    }

    pub(super) fn wire_invocation(invocation: &Value) -> DomainResult<Value> {
        let mut wire = invocation.clone();
        if let Some(object) = wire.as_object_mut() {
            for field in [
                "kind",
                "version",
                "createdAt",
                "state",
                "lastSequence",
                "workId",
                "projectId",
                "scope",
                "conversationId",
                "globalPolicyId",
                "globalPolicyVersion",
                "authorizedProjectIds",
                "executionIdentity",
                "adapterKind",
                "providerSessionId",
                "providerConfigurationDigest",
                "sessionCwd",
                "sessionLoaded",
                "terminalRecordId",
                "waitingTurnEnd",
                "releaseOperationId",
                "stopOperationId",
            ] {
                object.remove(field);
            }
        }
        let invocation: Invocation = parse(&wire)?;
        if [
            invocation.dispatch.is_some(),
            invocation.coordination_input.is_some(),
            invocation.executor_input.is_some(),
        ]
        .into_iter()
        .filter(|present| *present)
        .count()
            != 1
        {
            return Err(bad(
                "INVALID_ARGUMENT",
                "Invocation requires exactly one task, coordination, or work executor input",
            ));
        }
        encode(&invocation)
    }

    pub(super) fn runtime_command(
        &mut self,
        principal: &Principal,
        request: &Request,
    ) -> DomainResult<Response> {
        self.matched(request, &[])?;
        match request.method.as_str() {
            "task.acknowledge" => {
                let input: Acknowledge = parse(&request.params)?;
                let (dispatch, mut attempt, mut invocation) =
                    self.bound(principal, &input.dispatch_id, input.task_revision, false)?;
                if text(&invocation, "state") != "Running" && text(&invocation, "state") != "Idle" {
                    return Err(bad(
                        "BAD_STATE",
                        "Runtime must report Started before acknowledgment",
                    ));
                }
                if !["Accepted", "Declined"].contains(&input.disposition.as_str()) {
                    return Err(bad(
                        "INVALID_ARGUMENT",
                        "Unknown acknowledgment disposition",
                    ));
                }
                if input.disposition == "Declined" {
                    nonempty(input.reason.as_deref().unwrap_or(""), "reason")?;
                    attempt["endReason"] = json!("ContractDeclined");
                    attempt["declineReason"] = json!(input.reason);
                    self.put(attempt);
                    self.stop_invocation(&invocation, "ContractDeclined")?;
                } else if let Some(continuation_id) = input.continuation_id {
                    let continuation = self.record(&continuation_id, "Continuation")?;
                    if text(&continuation, "dispatchId") != input.dispatch_id
                        || number(&continuation, "taskRevision") != input.task_revision
                        || text(&attempt, "waitingRequestId") != text(&continuation, "requestId")
                        || !["Queued", "Dispatching"].contains(&text(&continuation, "state"))
                    {
                        return Err(bad(
                            "STALE_DISPATCH",
                            "Continuation is not the queued answer for this waiting attempt",
                        ));
                    }
                    let mut context =
                        self.record(text(&continuation, "requestId"), "ContextRequest")?;
                    context["status"] = json!("Applied");
                    let context = self.put(context);
                    let mut continuation = continuation;
                    continuation["state"] = json!("Applied");
                    self.put(continuation);
                    attempt
                        .as_object_mut()
                        .map(|o| o.remove("waitingRequestId"));
                    attempt["state"] = json!("Running");
                    attempt["acknowledged"] = json!(true);
                    self.put(attempt);
                    invocation["state"] = json!("Running");
                    self.put(invocation);
                    self.emit("ContextApplied", &context);
                    for mut decision in
                        self.related("DecisionRequest", "contextRequestId", text(&context, "id"))
                    {
                        if text(&decision, "status") == "Answered" {
                            let mut application = self
                                .record(text(&decision, "applicationId"), "DecisionApplication")?;
                            application["status"] = json!("Acknowledged");
                            application["receipt"] = json!({"continuationId":continuation_id,"contextRequestId":context["id"]});
                            self.put(application);
                            decision["status"] = json!("Resolved");
                            let decision = self.put(decision);
                            self.emit("DecisionApplied", &decision);
                            self.resolve_attention(text(&decision, "id"));
                        }
                    }
                } else {
                    if attempt["acknowledged"] == true || attempt.get("waitingRequestId").is_some()
                    {
                        return Err(bad(
                            "BAD_STATE",
                            "Dispatch already acknowledged or needs continuationId",
                        ));
                    }
                    attempt["acknowledged"] = json!(true);
                    attempt["state"] = json!("Running");
                    self.put(attempt);
                }
                let mut acknowledgment_body = json!({"workId":dispatch["workId"],"taskId":dispatch["taskId"],"dispatchId":dispatch["id"],"disposition":input.disposition});
                if let Some(reason) = input.reason {
                    acknowledgment_body["reason"] = json!(reason);
                }
                if let Some(continuation_id) = request.params.get("continuationId") {
                    acknowledgment_body["continuationId"] = continuation_id.clone();
                }
                let acknowledgment = self.create("Acknowledgment", acknowledgment_body);
                self.emit("DispatchAcknowledged", &dispatch);
                Ok(Response::ok(
                    "",
                    json!({"acknowledgmentId":acknowledgment["id"]}),
                ))
            }
            "artifact.capture" => {
                let input: Capture = parse(&request.params)?;
                let workspace = self.record(&input.workspace_id, "Workspace")?;
                if text(&workspace, "status") != "Ready" {
                    return Err(bad("BAD_STATE", "Workspace is not provisioned"));
                }
                match principal {
                    Principal::Human | Principal::Service => {}
                    Principal::Invocation { invocation_id } => {
                        let invocation = self.record(invocation_id, "Invocation")?;
                        if invocation["subject"]["kind"] == "Work" {
                            self.executor_binding(principal, text(&workspace, "workId"))?;
                        } else {
                            let dispatch = &invocation["dispatch"];
                            self.bound(
                                principal,
                                text(dispatch, "id"),
                                number(dispatch, "taskRevision"),
                                true,
                            )?;
                            if text(dispatch, "workspaceId") != input.workspace_id {
                                return Err(bad(
                                    "FORBIDDEN",
                                    "Capture is outside the invocation workspace",
                                ));
                            }
                        }
                    }
                    _ => {
                        return Err(bad(
                            "FORBIDDEN",
                            "Runtime capture must use its bound invocation",
                        ));
                    }
                }
                if input.sources.is_empty() || input.sources.len() > 64 {
                    return Err(bad("INVALID_ARGUMENT", "Capture requires 1..64 sources"));
                }
                if !["Output", "Evidence", "ManualInput"].contains(&input.purpose.as_str()) {
                    return Err(bad("INVALID_ARGUMENT", "Unknown capture purpose"));
                }
                for source in &input.sources {
                    match source.kind.as_str() {
                        "File" | "Tree" => {
                            if source.commit_id.is_some() {
                                return Err(bad("INVALID_ARGUMENT", "File/Tree forbids commitId"));
                            }
                            Self::relative_path(source.relative_path.as_deref().unwrap_or(""))?;
                        }
                        "GitCommit" => {
                            if source.relative_path.is_some() {
                                return Err(bad(
                                    "INVALID_ARGUMENT",
                                    "GitCommit forbids relativePath",
                                ));
                            }
                            let commit = source.commit_id.as_deref().unwrap_or("");
                            if ![40, 64].contains(&commit.len())
                                || !commit.bytes().all(|byte| byte.is_ascii_hexdigit())
                            {
                                return Err(bad(
                                    "INVALID_ARGUMENT",
                                    "GitCommit requires a full immutable commit ID",
                                ));
                            }
                        }
                        _ => return Err(bad("INVALID_ARGUMENT", "Unknown capture source kind")),
                    }
                }
                let mut params = encode(&input)?;
                params["workId"] = workspace["workId"].clone();
                params["root"] = workspace["localRoot"].clone();
                let operation =
                    self.effect("artifact.capture", params, Some(text(&workspace, "workId")));
                Ok(Response::pending(
                    "",
                    text(&operation, "id").into(),
                    json!({"operationId":operation["id"]}),
                ))
            }
            "runtime.report" => {
                let input: RuntimeReport = parse(&request.params)?;
                uuid(&input.observation_id)?;
                let mut invocation = self.record(&input.invocation_id, "Invocation")?;
                if !matches!(principal, Principal::Service)
                    && !matches!(principal,Principal::Runtime{runtime_id} if *runtime_id==text(&invocation,"runtimeId"))
                {
                    return Err(bad(
                        "FORBIDDEN",
                        "Observation runtime does not own the invocation",
                    ));
                }
                if let Some(old) = self.records.get(&input.observation_id) {
                    if old["body"] == encode(&input)? {
                        return Ok(Response::ok(
                            "",
                            json!({"observationId":input.observation_id,"lastSequence":invocation["lastSequence"]}),
                        ));
                    }
                    return Err(bad(
                        "COMMAND_ID_REUSED",
                        "Observation identity reused with different content",
                    ));
                }
                if input.binding_generation != number(&invocation, "bindingGeneration") {
                    return Err(bad(
                        "STALE_DISPATCH",
                        "Observation binding generation is stale",
                    ));
                }
                if input.sequence != number(&invocation, "lastSequence") + 1 {
                    return Err(bad(
                        "BAD_STATE",
                        "Observation sequence gap or regression; replay the missing observation before advancing",
                    ));
                }
                if text(&invocation, "state") == "Released" {
                    return Err(bad("STALE_DISPATCH", "Invocation has been released"));
                }
                match input.kind.as_str() {
                    "Started" => {
                        let started: Started = parse(&input.data)?;
                        let stopping = invocation["state"] == "Stopping"
                            && invocation.get("executionIdentity").is_none();
                        if text(&invocation, "state") != "Dispatching" && !stopping {
                            return Err(bad("BAD_STATE", "Invocation already started"));
                        }
                        nonempty(&started.execution_identity, "executionIdentity")?;
                        match started.adapter_kind.as_str() {
                            "ACP" => nonempty(
                                started.provider_session_id.as_deref().unwrap_or(""),
                                "providerSessionId",
                            )?,
                            "Command" => {
                                if started.provider_session_id.is_some() {
                                    return Err(bad(
                                        "INVALID_ARGUMENT",
                                        "Command execution forbids providerSessionId",
                                    ));
                                }
                            }
                            _ => return Err(bad("INVALID_ARGUMENT", "Unknown adapter kind")),
                        }
                        if started.adapter_kind == "Command"
                            && text(&invocation["dispatch"], "kind") != "EvaluateGate"
                        {
                            return Err(bad(
                                "FORBIDDEN",
                                "Command adapter may execute only a declared check",
                            ));
                        }
                        if !stopping {
                            invocation["state"] = json!("Running");
                        }
                        invocation["executionIdentity"] = json!(started.execution_identity);
                        invocation["adapterKind"] = json!(started.adapter_kind);
                        if let Some(session) = &started.provider_session_id {
                            invocation["providerSessionId"] = json!(session);
                        }
                        if let Some(configuration) = &started.provider_configuration_digest {
                            invocation["providerConfigurationDigest"] = json!(configuration);
                        }
                        if let Some(cwd) = &started.session_cwd {
                            invocation["sessionCwd"] = json!(cwd);
                        }
                        if let Some(loaded) = started.session_loaded {
                            invocation["sessionLoaded"] = json!(loaded);
                        }
                        if ["Coordination", "Work"].contains(&text(&invocation["subject"], "kind"))
                            && !text(&invocation, "workId").is_empty()
                        {
                            let mut work = self.record(text(&invocation, "workId"), "Work")?;
                            if invocation.get("sessionReuseRef").is_some() {
                                let primary = &work[if invocation["subject"]["kind"] == "Work" {
                                    "executorSession"
                                } else {
                                    "primarySession"
                                }];
                                if started.session_loaded != Some(true)
                                    || primary["invocationId"] != invocation["sessionReuseRef"]
                                    || primary["providerSessionId"].as_str()
                                        != started.provider_session_id.as_deref()
                                    || primary["cwd"].as_str() != started.session_cwd.as_deref()
                                    || primary["providerConfigurationDigest"].as_str()
                                        != started.provider_configuration_digest.as_deref()
                                {
                                    return Err(bad("SESSION_RESUME_UNAVAILABLE", "Runtime did not prove loading this work's saved primary session"));
                                }
                            }
                            if let (Some(session), Some(configuration), Some(cwd)) = (
                                &started.provider_session_id,
                                &started.provider_configuration_digest,
                                &started.session_cwd,
                            ) {
                                nonempty(configuration, "providerConfigurationDigest")?;
                                nonempty(cwd, "sessionCwd")?;
                                work["primarySession"] = json!({"invocationId":invocation["id"],"providerSessionId":session,
                                    "capabilityId":invocation["capabilityId"],"providerConfigurationDigest":configuration,"cwd":cwd});
                                if invocation["subject"]["kind"] == "Work" {
                                    work["executorSession"] = work["primarySession"].clone();
                                }
                                work["continuationUpdatedAt"] = json!(utc_after(0));
                                work.as_object_mut()
                                    .map(|object| object.remove("continuationFailure"));
                                let work = self.put(work);
                                self.emit("WorkChanged", &work);
                            }
                        }
                        if text(&invocation["subject"], "kind") == "Task" {
                            let mut attempt =
                                self.record(text(&invocation["subject"], "id"), "Attempt")?;
                            attempt["state"] = json!("Running");
                            self.put(attempt);
                        } else if invocation["subject"]["kind"] == "Work" {
                            let mut turn = self.record(
                                text(&invocation, "currentExecutorTurnId"),
                                "WorkExecutionTurn",
                            )?;
                            turn["state"] = json!("Running");
                            self.put(turn);
                        } else {
                            let mut turn = self
                                .record(text(&invocation["subject"], "id"), "CoordinationTurn")?;
                            turn["state"] = json!("Running");
                            self.put(turn);
                        }
                    }
                    "ExecutorTurnEnded" => {
                        closed(&input.data, &["turnId", "turnNumber", "finish"], &[])?;
                        if invocation["subject"]["kind"] != "Work"
                            || invocation["currentExecutorTurnId"] != input.data["turnId"]
                            || input.data["finish"] != "Normal"
                            || number(&input.data, "turnNumber")
                                <= number(&invocation, "lastExecutorTurnNumber")
                        {
                            return Err(bad(
                                "BAD_STATE",
                                "Executor reply does not match its current admitted input",
                            ));
                        }
                        let mut turn =
                            self.record(text(&input.data, "turnId"), "WorkExecutionTurn")?;
                        if turn["invocationId"] != invocation["id"]
                            || turn["turnNumber"] != input.data["turnNumber"]
                        {
                            return Err(bad("INVALID_REFERENCE", "Executor turn binding mismatch"));
                        }
                        turn["state"] = json!("Complete");
                        self.put(turn);
                        invocation["lastExecutorTurnNumber"] = input.data["turnNumber"].clone();
                        if invocation["state"] != "Stopping" {
                            invocation["state"] = json!("Idle");
                        }
                        let mut message =
                            self.record(text(&invocation, "replyMessageId"), "ConversationItem")?;
                        message["status"] = json!("Complete");
                        let message = self.put(message);
                        self.emit_message("MessageCompleted", &message);
                        let work = self.record(text(&invocation, "workId"), "Work")?;
                        self.emit("WorkChanged", &work);
                    }
                    "TextDelta" => {
                        let delta: TextDelta = parse(&input.data)?;
                        let message_id = invocation
                            .get("replyMessageId")
                            .or_else(|| invocation.get("transcriptMessageId"))
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        if delta.message_id != message_id {
                            return Err(bad(
                                "FORBIDDEN",
                                "TextDelta targets an unallocated message",
                            ));
                        }
                        let mut message = self.record(message_id, "ConversationItem")?;
                        let mut parts = values(&message, "parts");
                        if let Some(old) = parts.iter().find(|part| {
                            text(part, "partId") == delta.part_id
                                && number(part, "chunkIndex") == delta.chunk_index
                        }) {
                            if old["text"] != json!(delta.text) {
                                return Err(bad(
                                    "COMMAND_ID_REUSED",
                                    "Text chunk identity changed content",
                                ));
                            }
                        } else {
                            let expected = parts
                                .iter()
                                .filter(|part| text(part, "partId") == delta.part_id)
                                .count() as u64;
                            if delta.chunk_index != expected {
                                return Err(bad("BAD_STATE", "Text chunk index gap"));
                            }
                            parts.push(encode(&delta)?);
                            message["parts"] = json!(parts);
                            let message = self.put(message);
                            self.emit_message("MessageDelta", &message);
                        }
                    }
                    "ToolActivity" => {
                        let activity: ToolActivity = parse(&input.data)?;
                        if !["Started", "Ended"].contains(&activity.phase.as_str()) {
                            return Err(bad("INVALID_ARGUMENT", "Unknown tool phase"));
                        }
                    }
                    "TurnEnded" => {
                        let ended: TurnEnded = parse(&input.data)?;
                        if !["Normal", "Cancelled", "Error"].contains(&ended.finish.as_str())
                            || ended.turn_number == 0
                        {
                            return Err(bad("INVALID_ARGUMENT", "Invalid TurnEnded"));
                        }
                        if ended.turn_number <= number(&invocation, "lastTurnNumber") {
                            return Err(bad("BAD_STATE", "Provider turn was already classified"));
                        }
                        invocation["lastTurnNumber"] = json!(ended.turn_number);
                        invocation["lastTurnEnd"] = encode(&ended)?;
                        invocation["state"] =
                            json!(if ended.quiescent { "Idle" } else { "Settling" });
                    }
                    "Settled" => {
                        let settled: Settled = parse(&input.data)?;
                        if !settled.quiescent
                            || settled.execution_identity != text(&invocation, "executionIdentity")
                        {
                            return Err(bad(
                                "INVALID_REFERENCE",
                                "Settlement does not identify the tracked execution",
                            ));
                        }
                        for operation_id in &settled.completed_operation_ids {
                            let mut operation = self.record(operation_id, "Operation")?;
                            if text(&operation["effect"], "method") != "runtime.stop"
                                || text(&operation["effect"]["params"], "invocationId")
                                    != input.invocation_id
                            {
                                return Err(bad(
                                    "FORBIDDEN",
                                    "Settlement names an unrelated control operation",
                                ));
                            }
                            operation["status"] = json!("Succeeded");
                            operation["steps"] =
                                json!([{"state":"Confirmed","intent":"runtime.stop"}]);
                            let operation = self.put(operation);
                            self.emit("OperationChanged", &operation);
                            self.resolve_attention(operation_id);
                        }
                        if text(&invocation, "state") != "Releasing" {
                            invocation["state"] = json!("Idle");
                        }
                        if invocation.get("lastTurnEnd").is_none() {
                            invocation["lastTurnEnd"] = json!({"turnNumber":number(&invocation,"lastTurnNumber")+1,"finish":"Cancelled","quiescent":true});
                        } else {
                            invocation["lastTurnEnd"]["quiescent"] = json!(true);
                        }
                    }
                    "Disconnected" => {
                        closed(&input.data, &["reason"], &[])?;
                        invocation["health"] = json!("Unknown");
                        self.attention(
                            text(&invocation, "workId"),
                            &input.invocation_id,
                            "RuntimeDisconnected",
                            "Reconcile the recorded invocation; reservations remain held",
                        );
                    }
                    _ => return Err(bad("INVALID_ARGUMENT", "Unknown runtime observation kind")),
                }
                invocation["lastSequence"] = json!(input.sequence);
                let invocation = self.put(invocation);
                self.put(json!({"id":input.observation_id,"kind":"RuntimeObservation","invocationId":input.invocation_id,
                    "body":encode(&input)?,"createdAt":utc_after(0)}));
                self.emit("RuntimeObserved", &invocation);
                if input.kind == "Started"
                    && invocation["scope"] == "Global"
                    && invocation.get("supersededByInput").is_some()
                {
                    self.stop_invocation(&invocation, "NewConversationInput")?;
                }
                if input.kind == "TurnEnded" || input.kind == "Settled" {
                    self.classify_turn(&invocation)?;
                }
                Ok(Response::ok(
                    "",
                    json!({"observationId":input.observation_id,"lastSequence":input.sequence}),
                ))
            }
            _ => Err(bad("METHOD_UNSUPPORTED", &request.method)),
        }
    }

    pub(super) fn stop_invocation(
        &mut self,
        invocation: &Value,
        reason: &str,
    ) -> DomainResult<String> {
        if let Some(operation) = invocation.get("stopOperationId").and_then(Value::as_str) {
            let retryable = reason == "WorkContinuationRecovery"
                && self.records.get(operation).is_some_and(|operation| {
                    ["RepairRequired", "Failed", "Running"].contains(&text(operation, "status"))
                });
            if !retryable {
                return Ok(operation.into());
            }
        }
        let stop_operations: Vec<_> = self
            .all("Operation")
            .into_iter()
            .filter(|operation| {
                operation["effect"]["method"] == "runtime.stop"
                    && operation["effect"]["params"]["invocationId"] == invocation["id"]
                    && operation["status"] != "Succeeded"
            })
            .map(|operation| operation["id"].clone())
            .collect();
        let mut operation = self.effect(
            "runtime.stop",
            json!({"invocationId":invocation["id"],"reason":reason,
                "reconciliation":{"runtimeId":invocation["runtimeId"],"bindingGeneration":invocation["bindingGeneration"],
                    "lastSequence":invocation["lastSequence"],"executionIdentity":text(invocation,"executionIdentity"),
                    "stopOperationIds":stop_operations}}),
            invocation.get("workId").and_then(Value::as_str),
        );
        operation["effect"]["params"]["operationId"] = operation["id"].clone();
        let operation = self.put(operation);
        let mut invocation = invocation.clone();
        invocation["stopOperationId"] = operation["id"].clone();
        invocation["stopReason"] = json!(reason);
        invocation["state"] = json!("Stopping");
        self.put(invocation);
        Ok(text(&operation, "id").into())
    }

    pub(super) fn reconcile_invocation_release(&mut self, invocation: &Value) -> DomainResult<()> {
        let operation = self.record(text(invocation, "releaseOperationId"), "Operation")?;
        if !["RepairRequired", "Failed"].contains(&text(&operation, "status")) {
            return Ok(());
        }
        let previous: Vec<_> = self
            .all("Operation")
            .into_iter()
            .filter(|operation| {
                operation["effect"]["method"] == "runtime.release"
                    && operation["effect"]["params"]["invocationId"] == invocation["id"]
                    && operation["status"] != "Succeeded"
            })
            .map(|operation| operation["id"].clone())
            .collect();
        let operation = self.effect("runtime.release", json!({
            "invocationId":invocation["id"],"terminalDisposition":invocation["terminalDisposition"],
            "reconcilesOperationIds":previous,
            "reconciliation":{"runtimeId":invocation["runtimeId"],"bindingGeneration":invocation["bindingGeneration"],
                "lastSequence":invocation["lastSequence"],"executionIdentity":text(invocation,"executionIdentity"),
                "stopOperationIds":[]}
        }), invocation.get("workId").and_then(Value::as_str));
        let mut invocation = invocation.clone();
        invocation["releaseOperationId"] = operation["id"].clone();
        self.put(invocation);
        Ok(())
    }

    fn classify_turn(&mut self, original: &Value) -> DomainResult<()> {
        let mut invocation = self.record(text(original, "id"), "Invocation")?;
        let ended: TurnEnded = parse(&invocation["lastTurnEnd"])?;
        if !ended.quiescent {
            self.stop_invocation(&invocation, "SettleCompletion")?;
            return Ok(());
        }
        if text(&invocation, "state") == "Releasing" || text(&invocation, "state") == "Released" {
            return Ok(());
        }
        if invocation["subject"]["kind"] == "Work" {
            return self.classify_executor_exit(invocation, &ended);
        }
        let task_invocation = text(&invocation["subject"], "kind") == "Task";
        let mut subject = self.record(
            text(&invocation["subject"], "id"),
            if task_invocation {
                "Attempt"
            } else {
                "CoordinationTurn"
            },
        )?;
        if task_invocation && ended.finish == "Normal" && invocation.get("stopReason").is_none() {
            if let Some(request) = self
                .records
                .get(text(&subject, "waitingRequestId"))
                .cloned()
            {
                if ["Open", "Answered"].contains(&text(&request, "status")) {
                    subject["state"] = json!("WaitingForContext");
                    self.put(subject);
                    invocation["state"] = json!("Idle");
                    self.put(invocation.clone());
                    self.dispatch_continuation(&invocation, &request)?;
                    return Ok(());
                }
            }
        }
        let has_record = subject.get("terminalRecordId").is_some();
        let declined = text(&subject, "endReason") == "ContractDeclined";
        let superseded = !task_invocation
            && invocation["scope"] == "Global"
            && invocation["stopReason"] == "NewConversationInput";
        let success = (ended.finish == "Normal" || superseded && ended.finish == "Cancelled")
            && has_record
            && !declined;
        // Transport cancellation settles a declined contract; it is not a human cancel.
        let disposition = if success {
            "Succeeded"
        } else if ended.finish == "Cancelled" && !declined {
            "Cancelled"
        } else {
            "Failed"
        };
        subject["state"] = json!(disposition);
        if !success && !declined {
            subject["endReason"] = json!(if !has_record && ended.finish == "Normal" {
                if task_invocation && subject["acknowledged"] != true {
                    "ContractUnacknowledged"
                } else {
                    "MissingSubmission"
                }
            } else {
                "ExecutionFailed"
            });
        }
        if task_invocation && !has_record {
            let diagnostic = if declined {
                text(&subject, "declineReason")
            } else {
                ended.error_text.as_deref().unwrap_or("")
            };
            let mut end = diagnostic.len().min(4096);
            while !diagnostic.is_char_boundary(end) {
                end -= 1;
            }
            let diagnostic_truncated = end < diagnostic.len();
            let diagnostic = diagnostic[..end].to_owned();
            let code = if declined {
                "CONTRACT_DECLINED"
            } else if ended.finish == "Cancelled" {
                "CANCELLED"
            } else if ended.finish == "Error" {
                "EXECUTION_FAILED"
            } else {
                "PROTOCOL_INCOMPLETE"
            };
            subject["failureDetails"] = json!({
                "code":code,"finish":ended.finish,
                "phase":if invocation.get("executionIdentity").is_some() {"AfterStarted"} else {"BeforeStarted"},
                "invocationId":invocation["id"],"attemptId":subject["id"],
                "taskId":subject["taskId"],
                "diagnostic":diagnostic,"diagnosticTruncated":diagnostic_truncated,
                "processOutcomeRecorded":false
            });
            for (target, source) in [
                ("resultId", "subjectResultId"),
                ("evaluationUnitId", "evaluationUnitId"),
                ("evaluationRound", "evaluationRound"),
            ] {
                if let Some(value) = invocation["dispatch"].get(source) {
                    subject["failureDetails"][target] = value.clone();
                }
            }
            if let Some(reason) = invocation.get("stopReason") {
                subject["failureDetails"]["stopReason"] = reason.clone();
            }
        }
        if !task_invocation {
            if let Some(error) = ended
                .error_text
                .as_ref()
                .filter(|error| error.contains("SESSION_RESUME_UNAVAILABLE"))
            {
                if !text(&invocation, "workId").is_empty() {
                    let mut work = self.record(text(&invocation, "workId"), "Work")?;
                    work["continuationFailure"] =
                        json!({"code":"SESSION_RESUME_UNAVAILABLE","message":error});
                    let work = self.put(work);
                    self.emit("WorkChanged", &work);
                }
            }
            subject["state"] = json!(if success {
                "Completed"
            } else if superseded {
                "Superseded"
            } else {
                "Blocked"
            });
            if superseded && !success {
                subject["endReason"] = json!("NewConversationInput");
            }
        }
        let subject = self.put(subject);
        invocation["state"] = json!("Releasing");
        invocation["terminalDisposition"] = json!(disposition);
        let operation=self.effect("runtime.release",json!({"invocationId":invocation["id"],"terminalDisposition":invocation["terminalDisposition"],
            "reconciliation":{"runtimeId":invocation["runtimeId"],"bindingGeneration":invocation["bindingGeneration"],
                "lastSequence":invocation["lastSequence"],"executionIdentity":text(&invocation,"executionIdentity"),"stopOperationIds":[]}
        }),invocation.get("workId").and_then(Value::as_str));
        invocation["releaseOperationId"] = operation["id"].clone();
        self.put(invocation.clone());
        if task_invocation && declined {
            self.queue_coordination(
                Some(text(&invocation, "workId")),
                None,
                "ContractDeclined",
                &subject,
            )?;
        }
        if !success
            && !superseded
            && invocation.get("specChangeId").is_none()
            && invocation.get("manualTakeoverWorkspaceId").is_none()
        {
            let work_id = text(&invocation, "workId");
            if !task_invocation {
                self.attention(work_id,text(&subject,"id"),"PROTOCOL_INCOMPLETE","Coordinator ended without a valid typed finish; inspect and explicitly repair the coordination scope");
                self.emit("CoordinationBlocked", &subject);
            } else if text(&invocation["dispatch"], "kind") == "ProduceResult" {
                self.attention(
                    work_id,
                    text(&subject, "taskId"),
                    text(&subject, "endReason"),
                    "Coordinator must record a bounded replacement plan or corrective action",
                );
                if !declined {
                    self.queue_coordination(Some(work_id), None, "MissingSubmission", &subject)?;
                }
            }
        }
        if let Some(message_id) = invocation
            .get("replyMessageId")
            .or_else(|| invocation.get("transcriptMessageId"))
            .and_then(Value::as_str)
        {
            let mut message = self.record(message_id, "ConversationItem")?;
            message["status"] = json!(if success { "Complete" } else { "Interrupted" });
            let message = self.put(message);
            self.emit_message("MessageCompleted", &message);
        }
        Ok(())
    }

    pub fn take_effects(&mut self) -> Result<Vec<Effect>> {
        self.db.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> Result<Vec<Effect>> {
            let mut statement = self.db.prepare(
                "SELECT id,method,body FROM effects WHERE state='Pending' ORDER BY rowid",
            )?;
            let effects = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })?
                .map(|row| {
                    let (id, method, body) = row?;
                    Ok(Effect {
                        id,
                        method,
                        params: serde_json::from_str(&body)?,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            for effect in &effects {
                self.db.execute(
                    "UPDATE effects SET state='Dispatched' WHERE id=?1",
                    [&effect.id],
                )?;
            }
            Ok(effects)
        })();
        match result {
            Ok(effects) => {
                self.db.execute_batch("COMMIT")?;
                Ok(effects)
            }
            Err(error) => {
                self.db.execute_batch("ROLLBACK")?;
                Err(error)
            }
        }
    }

    pub fn complete_effect(&mut self, effect_id: &str, response: Response) -> Result<()> {
        self.db.execute_batch("BEGIN IMMEDIATE")?;
        let backup = self.records.clone();
        let original_position = self.position;
        self.correlation = effect_id.into();
        let result = (|| -> Result<()> {
            let (method, body, state, prior): (String, String, String, Option<String>) =
                self.db.query_row(
                    "SELECT method,body,state,response FROM effects WHERE id=?1",
                    [effect_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )?;
            if state == "Completed" {
                let Some(prior) = prior else {
                    anyhow::bail!("Completed effect lacks receipt");
                };
                let mut prior: Response = serde_json::from_str(&prior)?;
                prior.request_id = response.request_id.clone();
                anyhow::ensure!(
                    serde_json::to_value(prior)? == serde_json::to_value(&response)?,
                    "Effect completed with conflicting receipt"
                );
                return Ok(());
            }
            let params: Value = serde_json::from_str(&body)?;
            self.apply_effect(effect_id, &method, &params, &response)
                .map_err(|failure| anyhow::anyhow!("{:?}", failure.failure))?;
            self.drive()
                .map_err(|failure| anyhow::anyhow!("{:?}", failure.failure))?;
            self.persist_changes(&backup)?;
            self.db.execute(
                "UPDATE effects SET state='Completed',response=?2 WHERE id=?1",
                params![effect_id, serde_json::to_string(&response)?],
            )?;
            Ok(())
        })();
        match result {
            Ok(()) => match self.db.execute_batch("COMMIT") {
                Ok(()) => Ok(()),
                Err(error) => {
                    self.records = backup;
                    self.position = original_position;
                    self.pending_events.clear();
                    let _ = self.db.execute_batch("ROLLBACK");
                    Err(error.into())
                }
            },
            Err(error) => {
                self.records = backup;
                self.position = original_position;
                self.pending_events.clear();
                self.db.execute_batch("ROLLBACK")?;
                Err(error)
            }
        }
    }

    fn apply_effect(
        &mut self,
        effect_id: &str,
        method: &str,
        params: &Value,
        response: &Response,
    ) -> DomainResult<()> {
        let mut operation = self.record(effect_id, "Operation")?;
        if response.status != "ok" && !(method == "runtime.stop" && response.status == "pending") {
            operation["status"] = json!(if method.starts_with("runtime.")
                || (method == "project.create"
                    && response
                        .failure
                        .as_ref()
                        .is_some_and(|failure| failure.code == "OUTCOME_UNKNOWN"))
            {
                "RepairRequired"
            } else {
                "Failed"
            });
            operation["failure"] = encode(&response.failure)?;
            operation["steps"] = json!([{"state":if operation["status"] == "RepairRequired" {"Unknown"} else {"Failed"},"intent":method}]);
            let operation = self.put(operation);
            self.emit("OperationChanged", &operation);
            self.attention(text(&operation,"workId"),effect_id,"EffectFailed","Inspect exact operation failure and repair; no replacement effect is automatically issued");
            if method == "project.create" {
                self.complete_project_action(&operation)?;
            }
            return Ok(());
        }
        let data = response.data.as_ref().unwrap_or(&Value::Null);
        match method {
            "runtime.recover_coordinator" => self.recovered_coordinator(params, data)?,
            "project.create" => {
                let mut project = params["project"].clone();
                let project_id = text(&project, "id");
                uuid(project_id)?;
                let root = Path::new(text(data, "root"));
                if data["projectId"] != project["id"]
                    || data["root"] != project["root"]
                    || !root.is_absolute()
                    || !root.is_dir()
                    || text(data, "commitId").len() < 40
                    || !text(data, "commitId")
                        .chars()
                        .all(|ch| ch.is_ascii_hexdigit())
                    || self.records.contains_key(project_id)
                {
                    return Err(bad(
                        "INVALID_REFERENCE",
                        "New project receipt does not match the approved directory and repository",
                    ));
                }
                project["initialCommitId"] = data["commitId"].clone();
                let project = self.put(project);
                operation["result"] = json!({
                    "projectId":project["id"],"version":project["version"],
                    "policyRevision":project["policyRevision"],"project":project
                });
            }
            "workspace.provision" => {
                let mut workspace = self.record(text(params, "workspaceId"), "Workspace")?;
                let root = text(data, "localRoot");
                if root.is_empty() || !Path::new(root).is_absolute() || !Path::new(root).is_dir() {
                    return Err(bad(
                        "EXECUTION_FAILED",
                        "Workspace receipt must name an existing absolute localRoot",
                    ));
                }
                if data.get("workspaceId").is_some() && data["workspaceId"] != params["workspaceId"]
                {
                    return Err(bad(
                        "INVALID_REFERENCE",
                        "Provision receipt changed workspace identity",
                    ));
                }
                workspace["localRoot"] = json!(root);
                workspace["status"] = json!("Ready");
                for field in ["repositoryIdentity", "branch", "commitId"] {
                    if let Some(value) = data.get(field) {
                        workspace[field] = value.clone();
                    }
                }
                let workspace = self.put(workspace);
                self.emit(
                    "WorkChanged",
                    &self.record(text(&workspace, "workId"), "Work")?,
                );
            }
            "artifact.capture" => {
                let captured = values(data, "artifacts");
                if captured.is_empty() {
                    return Err(bad(
                        "EXECUTION_FAILED",
                        "Capture receipt has no immutable artifacts",
                    ));
                }
                let mut refs = Vec::new();
                for captured in captured {
                    let artifact_id = if text(&captured, "artifactId").is_empty() {
                        id()
                    } else {
                        text(&captured, "artifactId").into()
                    };
                    uuid(&artifact_id)?;
                    if self.records.contains_key(&artifact_id) {
                        return Err(bad(
                            "INVALID_REFERENCE",
                            "Capture attempted to overwrite an existing immutable record identity",
                        ));
                    }
                    let digest = text(&captured, "digest");
                    if digest.strip_prefix("sha256:").is_none_or(|value| {
                        value.len() != 64
                            || !value
                                .bytes()
                                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                    }) {
                        return Err(bad(
                            "INVALID_ARGUMENT",
                            "Capture receipt has invalid digest",
                        ));
                    }
                    if captured.get("manifest").is_none() || text(&captured, "localPath").is_empty()
                    {
                        return Err(bad(
                            "EXECUTION_FAILED",
                            "Capture manager must return manifest and immutable localPath",
                        ));
                    }
                    if !Path::new(text(&captured, "localPath")).exists() {
                        return Err(bad(
                            "ARTIFACT_UNAVAILABLE",
                            "Captured content is not readable locally",
                        ));
                    }
                    let mut artifact = captured.clone();
                    artifact["id"] = json!(artifact_id);
                    artifact["kind"] = json!("Artifact");
                    artifact["artifactKind"] = captured["kind"].clone();
                    artifact["availability"] = json!("Ready");
                    artifact["workId"] = params["workId"].clone();
                    artifact["workspaceId"] = params["workspaceId"].clone();
                    artifact["captureOperationId"] = json!(effect_id);
                    artifact["createdAt"] = json!(utc_after(0));
                    let artifact = self.put(artifact);
                    self.emit("ArtifactReady", &artifact);
                    refs.push(json!({"artifactId":artifact["id"],"digest":artifact["digest"]}));
                }
                operation["result"] = json!({"artifacts":refs});
                if operation.get("handback").is_some() {
                    self.apply_handback(&operation)?;
                }
            }
            "runtime.invoke" => {
                if data["invocationId"] != params["invocation"]["id"]
                    || !["Recorded", "AlreadyRecorded"].contains(&text(data, "disposition"))
                {
                    return Err(bad(
                        "EXECUTION_FAILED",
                        "Runtime invoke receipt is not correlated",
                    ));
                }
                let mut invocation = self.record(text(data, "invocationId"), "Invocation")?;
                invocation["dispatchRecorded"] = json!(true);
                let invocation = self.put(invocation);
                if invocation["scope"] == "Global"
                    && invocation.get("supersededByInput").is_some()
                    && ["Dispatching", "Running"].contains(&text(&invocation, "state"))
                {
                    self.stop_invocation(&invocation, "NewConversationInput")?;
                }
            }
            "runtime.continue" => {
                if data["continuationId"] != params["continuation"]["id"]
                    || !["Recorded", "AlreadyRecorded"].contains(&text(data, "disposition"))
                {
                    return Err(bad(
                        "EXECUTION_FAILED",
                        "Continuation receipt is not correlated",
                    ));
                }
                let mut continuation =
                    self.record(text(&params["continuation"], "id"), "Continuation")?;
                if text(&continuation, "state") == "Dispatching" {
                    continuation["state"] = json!("Queued");
                    self.put(continuation);
                }
            }
            "runtime.work_input" => {
                if data["invocationId"] != params["invocationId"]
                    || data["turnId"] != params["input"]["turnId"]
                    || !["Recorded", "AlreadyRecorded"].contains(&text(data, "disposition"))
                {
                    return Err(bad(
                        "EXECUTION_FAILED",
                        "Executor input receipt is not correlated",
                    ));
                }
                let mut turn =
                    self.record(text(&params["input"], "turnId"), "WorkExecutionTurn")?;
                if turn["state"] == "Dispatching" {
                    turn["state"] = json!("Running");
                    self.put(turn);
                }
            }
            "runtime.stop" => {
                if text(&operation, "status") != "Succeeded" {
                    operation["status"] = json!("Running");
                    operation["steps"] = json!([{"state":"Dispatched","intent":method}]);
                }
            }
            "runtime.release" => {
                if data["released"] != true {
                    return Err(bad(
                        "EXECUTION_FAILED",
                        "Runtime did not release its binding",
                    ));
                }
                let mut invocation = self.record(text(params, "invocationId"), "Invocation")?;
                for previous in values(params, "reconcilesOperationIds") {
                    let previous_id = previous.as_str().ok_or_else(|| {
                        bad(
                            "INVALID_REFERENCE",
                            "Release reconciliation operation must be an ID",
                        )
                    })?;
                    let mut previous = self.record(previous_id, "Operation")?;
                    if previous["effect"]["method"] != "runtime.release"
                        || previous["effect"]["params"]["invocationId"] != invocation["id"]
                    {
                        return Err(bad(
                            "INVALID_REFERENCE",
                            "Release reconciliation names another execution",
                        ));
                    }
                    previous["status"] = json!("Succeeded");
                    previous["reconciledByOperationId"] = json!(effect_id);
                    previous["steps"] = json!([{"state":"Confirmed","intent":"runtime.release"}]);
                    let previous = self.put(previous);
                    self.emit("OperationChanged", &previous);
                    self.resolve_attention(previous_id);
                }
                invocation["state"] = json!("Released");
                if let Some(reuse) = data.get("sessionReuseRef") {
                    invocation["sessionReuseRef"] = reuse.clone();
                }
                let invocation = self.put(invocation);
                if text(&invocation["subject"], "kind") == "Task" {
                    let mut attempt = self.record(text(&invocation["subject"], "id"), "Attempt")?;
                    attempt["reservationHeld"] = json!(false);
                    self.put(attempt.clone());
                    let dispatch = &invocation["dispatch"];
                    if text(dispatch, "kind") != "ProduceResult" {
                        let mut unit =
                            self.record(text(dispatch, "evaluationUnitId"), "EvaluationUnit")?;
                        unit["state"] = json!("Settled");
                        unit["terminalRecordId"] = attempt["terminalRecordId"].clone();
                        if attempt.get("terminalRecordId").is_none() {
                            unit["failure"] = attempt["failureDetails"]["code"]
                                .as_str()
                                .map_or(json!("PROTOCOL_INCOMPLETE"), |code| json!(code));
                            if let Some(details) = attempt.get("failureDetails") {
                                unit["failureDetails"] = details.clone();
                            }
                        }
                        self.put(unit);
                    } else if attempt.get("terminalRecordId").is_none()
                        && invocation.get("specChangeId").is_none()
                    {
                        let mut task = self.record(text(&attempt, "taskId"), "Task")?;
                        task["state"] = json!("Blocked");
                        self.put(task);
                    }
                } else if invocation["subject"]["kind"] == "Work" {
                    let work = self.record(text(&invocation, "workId"), "Work")?;
                    let mut workspace = self.record(text(&work, "workspaceId"), "Workspace")?;
                    if workspace["executorInvocationId"] == invocation["id"] {
                        workspace
                            .as_object_mut()
                            .map(|fields| fields.remove("executorInvocationId"));
                        self.put(workspace);
                    }
                    self.emit("WorkChanged", &work);
                } else {
                    let turn =
                        self.record(text(&invocation["subject"], "id"), "CoordinationTurn")?;
                    self.emit("CoordinationFinished", &turn);
                }
                self.resolve_attention(text(&invocation, "id"));
            }
            _ => return Err(bad("METHOD_UNSUPPORTED", "Unrecognized durable effect")),
        }
        if method != "runtime.stop" {
            operation["status"] = json!("Succeeded");
            operation["steps"] = json!([{"state":"Confirmed","intent":method}]);
        }
        if operation.get("result").is_none() {
            operation["result"] = data.clone();
        }
        let operation = self.put(operation);
        self.emit("OperationChanged", &operation);
        if text(&operation, "status") == "Succeeded" {
            self.resolve_attention(effect_id);
        }
        if method == "project.create" {
            self.complete_project_action(&operation)?;
        }
        Ok(())
    }
}
