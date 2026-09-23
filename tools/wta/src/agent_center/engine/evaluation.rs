// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

use super::*;

impl Engine {
    pub(super) fn evaluation_command(
        &mut self,
        principal: &Principal,
        request: &Request,
    ) -> DomainResult<Response> {
        match request.method.as_str() {
            "result.submit" => {
                self.matched(request, &[])?;
                let input: TaskResultBody = parse(&request.params)?;
                let (dispatch, mut attempt, _) =
                    self.bound(principal, &input.dispatch_id, input.task_revision, true)?;
                if text(&dispatch, "dispatchKind") != "ProduceResult" {
                    return Err(bad(
                        "WRONG_TERMINAL_RECORD",
                        "This dispatch does not produce TaskResult",
                    ));
                }
                if attempt.get("terminalRecordId").is_some() {
                    return Err(bad(
                        "BAD_STATE",
                        "A producing attempt can submit only one formal result",
                    ));
                }
                if input.input_manifest_digest != text(&dispatch, "inputManifestDigest") {
                    return Err(bad(
                        "STALE_DISPATCH",
                        "Result input manifest differs from the immutable dispatch",
                    ));
                }
                let mut task = self.record(text(&dispatch, "taskId"), "Task")?;
                let current = text(&task, "currentResultId");
                if input.supersedes_result_id.as_deref().unwrap_or("") != current {
                    return Err(bad(
                        "STALE_DISPATCH",
                        "A revised result must name the exact prior current submission",
                    ));
                }
                let contract: TaskContract = parse(&task["contract"])?;
                let mut output_slots = BTreeSet::new();
                for output in &input.outputs {
                    if !output_slots.insert(&output.slot)
                        || !contract
                            .outputs
                            .iter()
                            .any(|declared| declared.slot == output.slot)
                    {
                        return Err(bad(
                            "INVALID_ARGUMENT",
                            "Result contains duplicate or undeclared output slot",
                        ));
                    }
                    self.artifacts(std::slice::from_ref(&output.artifact))?;
                    let artifact = self.record(&output.artifact.artifact_id, "Artifact")?;
                    if text(&artifact, "workId") != text(&dispatch, "workId") {
                        return Err(bad("FORBIDDEN", "Output artifact belongs to another work"));
                    }
                    let declared = contract
                        .outputs
                        .iter()
                        .find(|declared| declared.slot == output.slot)
                        .ok_or_else(|| bad("INVALID_REFERENCE", "Output slot disappeared"))?;
                    let actual = text(&artifact, "artifactKind");
                    let valid = declared.kind == actual
                        || matches!(
                            (declared.kind.as_str(), actual),
                            ("Report", "File")
                                | ("Evidence", "File")
                                | ("Code", "Tree")
                                | ("Code", "GitCommit")
                        );
                    if !valid {
                        return Err(bad(
                            "INVALID_ARGUMENT",
                            "Captured output kind does not match its declared slot",
                        ));
                    }
                }
                if contract
                    .outputs
                    .iter()
                    .any(|output| output.required && !output_slots.contains(&output.slot))
                {
                    return Err(bad("INVALID_ARGUMENT", "Required output is missing"));
                }
                let mut criteria = BTreeSet::new();
                for evidence in &input.criterion_evidence {
                    if !criteria.insert(&evidence.criterion_id)
                        || !contract
                            .criteria
                            .iter()
                            .any(|criterion| criterion.id == evidence.criterion_id)
                    {
                        return Err(bad(
                            "INVALID_ARGUMENT",
                            "Unknown or duplicate criterion evidence",
                        ));
                    }
                    self.artifacts(&evidence.evidence)?;
                    nonempty(&evidence.claim, "criterionEvidence.claim")?;
                }
                for criterion in &contract.criteria {
                    if !input.criterion_evidence.iter().any(|evidence| {
                        evidence.criterion_id == criterion.id && !evidence.evidence.is_empty()
                    }) && !input.known_gaps.iter().any(|gap| {
                        gap == &criterion.id || gap.starts_with(&format!("{}:", criterion.id))
                    }) {
                        return Err(bad(
                            "INVALID_ARGUMENT",
                            "Each criterion requires evidence or a knownGaps entry naming its criterion ID",
                        ));
                    }
                }
                nonempty(&input.summary, "summary")?;
                let body = encode(&input)?;
                let result=self.create("TaskResult",json!({"workId":dispatch["workId"],"taskId":dispatch["taskId"],"taskRevision":input.task_revision,
                    "attemptId":attempt["id"],"contextSnapshotId":dispatch["contextSnapshotId"],"body":body,
                    "disposition":"Submitted","evaluationRound":1,"submissionDigest":digest(&body)?,
                    "producerSettled":false}));
                if !current.is_empty() {
                    let mut prior = self.record(current, "TaskResult")?;
                    prior["supersededByResultId"] = result["id"].clone();
                    prior["disposition"] = json!("Superseded");
                    self.put(prior);
                }
                attempt["terminalRecordId"] = result["id"].clone();
                self.put(attempt);
                task["currentResultId"] = result["id"].clone();
                task["state"] = json!("Submitted");
                self.put(task);
                self.emit("ResultSubmitted", &result);
                Ok(Response::ok(
                    "",
                    json!({"resultId":result["id"],"version":result["version"],"disposition":"Submitted","evaluationRound":1}),
                ))
            }
            "gate.submit" => {
                self.matched(request, &[])?;
                let input: GateSubmission = parse(&request.params)?;
                if input.decision_application_id.is_some() || input.dispatch_id.is_none() {
                    return Err(bad(
                        "FORBIDDEN",
                        "Human gate applications are generated only by the service",
                    ));
                }
                let (dispatch, mut attempt, invocation) = self.bound(
                    principal,
                    input.dispatch_id.as_deref().unwrap_or(""),
                    input.task_revision,
                    true,
                )?;
                if text(&dispatch, "dispatchKind") != "EvaluateGate" {
                    return Err(bad(
                        "WRONG_TERMINAL_RECORD",
                        "Dispatch requires a different terminal record",
                    ));
                }
                if text(&invocation, "adapterKind") != "Command" {
                    return Err(bad(
                        "FORBIDDEN",
                        "process-exit-v1 requires a native command execution, not model prose",
                    ));
                }
                let mut unit = self.check_evaluation(
                    &dispatch,
                    &input.evaluation_unit_id,
                    &input.subject_result_id,
                    input.evaluation_round,
                    input.task_revision,
                )?;
                if input.gate_definition_id != text(&dispatch, "gateDefinitionId")
                    || input.input_manifest_digest != text(&dispatch, "inputManifestDigest")
                    || input.gate_definition_revision != number(&unit["gateDefinition"], "revision")
                {
                    return Err(bad(
                        "STALE_EVALUATION",
                        "Gate submission is not pinned to this evaluation definition/manifest",
                    ));
                }
                if !["Passed", "Failed", "Inconclusive"].contains(&input.outcome.as_str()) {
                    return Err(bad("INVALID_ARGUMENT", "Unknown gate outcome"));
                }
                if attempt.get("terminalRecordId").is_some() {
                    return Err(bad("BAD_STATE", "Evaluation unit already submitted"));
                }
                self.artifacts(&input.evidence)?;
                if input.evidence.is_empty() {
                    return Err(bad(
                        "INVALID_ARGUMENT",
                        "Native check must capture concrete execution evidence",
                    ));
                }
                let mut body = encode(&input)?;
                body["workId"] = dispatch["workId"].clone();
                body["taskId"] = dispatch["taskId"].clone();
                body["resultId"] = json!(input.subject_result_id);
                body["evaluatorIdentity"] = json!(principal.key());
                let gate = self.create("GateResult", body);
                attempt["terminalRecordId"] = gate["id"].clone();
                self.put(attempt);
                unit["terminalRecordId"] = gate["id"].clone();
                self.put(unit);
                self.emit("GateEvaluated", &gate);
                Ok(Response::ok(
                    "",
                    json!({"gateResultId":gate["id"],"resultId":gate["resultId"],"evaluationRound":input.evaluation_round}),
                ))
            }
            "review.submit" => {
                self.matched(request, &[])?;
                let input: ReviewSubmission = parse(&request.params)?;
                let (dispatch, mut attempt, _) =
                    self.bound(principal, &input.dispatch_id, input.task_revision, true)?;
                if text(&dispatch, "dispatchKind") != "ReviewResult" {
                    return Err(bad("WRONG_TERMINAL_RECORD", "Dispatch is not ReviewResult"));
                }
                let mut unit = self.check_evaluation(
                    &dispatch,
                    &input.evaluation_unit_id,
                    &input.subject_result_id,
                    input.evaluation_round,
                    input.task_revision,
                )?;
                if input.evidence_manifest_digest != text(&dispatch, "evidenceManifestDigest") {
                    return Err(bad(
                        "STALE_EVALUATION",
                        "Reviewer evidence manifest is stale",
                    ));
                }
                let expected: BTreeSet<_> = values(&unit, "gateResultIds")
                    .into_iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect();
                let actual: BTreeSet<_> = input.gate_result_ids.iter().cloned().collect();
                if expected != actual || actual.len() != input.gate_result_ids.len() {
                    return Err(bad(
                        "STALE_EVALUATION",
                        "Review must reference the exact completed gate evidence",
                    ));
                }
                if !["Accept", "NeedsEvidence", "ChangesRequested", "Reject"]
                    .contains(&input.recommendation.as_str())
                {
                    return Err(bad("INVALID_ARGUMENT", "Unknown review recommendation"));
                }
                if input.recommendation != "Accept" && input.findings.is_empty() {
                    return Err(bad(
                        "INVALID_ARGUMENT",
                        "Correction requires criterion-bound findings",
                    ));
                }
                for finding in &input.findings {
                    if !values(&dispatch, "criteria")
                        .iter()
                        .any(|criterion| text(criterion, "id") == finding.criterion_id)
                    {
                        return Err(bad(
                            "INVALID_REFERENCE",
                            "Review finding has unknown criterion",
                        ));
                    }
                    self.artifacts(&finding.evidence)?;
                    nonempty(&finding.requested_change, "requestedChange")?;
                }
                self.artifacts(&input.preserve_artifacts)?;
                if attempt.get("terminalRecordId").is_some() {
                    return Err(bad(
                        "BAD_STATE",
                        "Reviewer already submitted its terminal record",
                    ));
                }
                let mut body = encode(&input)?;
                body["workId"] = dispatch["workId"].clone();
                body["taskId"] = dispatch["taskId"].clone();
                body["resultId"] = json!(input.subject_result_id);
                body["reviewerIdentity"] = json!(principal.key());
                let review = self.create("TaskReview", body);
                attempt["terminalRecordId"] = review["id"].clone();
                self.put(attempt);
                unit["terminalRecordId"] = review["id"].clone();
                self.put(unit);
                self.emit("ReviewRecorded", &review);
                Ok(Response::ok(
                    "",
                    json!({"reviewId":review["id"],"resultId":review["resultId"],"evaluationRound":input.evaluation_round}),
                ))
            }
            "result.accept" => {
                let input: ResultVerdict = parse(&request.params)?;
                let result = self.record(&input.result_id, "TaskResult")?;
                self.coordinator(principal, Some(text(&result, "workId")))?;
                self.matched(request, &[&result])?;
                self.round(&result, input.evaluation_round)?;
                if !self.acceptance_ready(&result)? {
                    return Err(bad(
                        "BAD_STATE",
                        "Required checks, review, output evidence or settlement are incomplete",
                    ));
                }
                self.accept_result(&result)?;
                Ok(Response::ok(
                    "",
                    json!({"disposition":"Accepted","outputs":result["body"]["outputs"]}),
                ))
            }
            "result.request_changes" | "result.reject" => {
                let input: ResultChanges = parse(&request.params)?;
                let result = self.record(&input.result_id, "TaskResult")?;
                self.coordinator(principal, Some(text(&result, "workId")))?;
                self.matched(request, &[&result])?;
                self.round(&result, input.evaluation_round)?;
                if input.instruction.result_id != input.result_id {
                    return Err(bad(
                        "INVALID_REFERENCE",
                        "Rework instruction targets another result",
                    ));
                }
                if request.method == "result.reject" && input.instruction.action != "Replan" {
                    return Err(bad("INVALID_ARGUMENT", "Rejected results require Replan"));
                }
                self.validate_instruction(&result, &input.instruction)?;
                let rework = self.changes(
                    &result,
                    input.instruction,
                    request.method == "result.reject",
                )?;
                Ok(Response::ok(
                    "",
                    json!({"disposition":if request.method=="result.reject" {"Rejected"} else {"ChangesRequested"},"reworkId":rework["id"]}),
                ))
            }
            "task.rework" => {
                let input: Rework = parse(&request.params)?;
                let mut task = self.record(&input.task_id, "Task")?;
                let mut result = self.record(&input.result_id, "TaskResult")?;
                self.coordinator(principal, Some(text(&task, "workId")))?;
                self.matched(request, &[&task, &result])?;
                let mut rework = self.record(&input.rework_id, "ReworkInstruction")?;
                if text(&result, "taskId") != input.task_id
                    || text(&task, "currentResultId") != input.result_id
                    || text(&rework, "resultId") != input.result_id
                    || text(&rework, "status") != "Open"
                    || (text(&rework["instruction"], "action") != input.action
                        && !values(&rework["recovery"], "allowedActions")
                            .contains(&json!(input.action)))
                    || !["ChangesRequested", "Rejected"].contains(&text(&result, "disposition"))
                {
                    return Err(bad(
                        "STALE_EVALUATION",
                        "Rework no longer describes the current result/action",
                    ));
                }
                let work = self.record(text(&task, "workId"), "Work")?;
                if !Self::legacy_mode(&work) || text(&work, "desiredAdvancement") != "Advance" {
                    return Err(bad("BAD_STATE", "Work advancement is held"));
                }
                if input
                    .capability_id
                    .as_deref()
                    .is_some_and(|capability| capability != text(&task, "capabilityId"))
                {
                    return Err(bad(
                        "METHOD_UNSUPPORTED",
                        "Capability changes require a compatible plan proposal",
                    ));
                }
                if self
                    .related("Attempt", "taskId", &input.task_id)
                    .iter()
                    .any(|attempt| attempt["reservationHeld"] == true)
                {
                    return Err(bad(
                        "BAD_STATE",
                        "Previous task/evaluation executions must release reservations before rework",
                    ));
                }
                if self
                    .related("AttentionItem", "subjectId", &input.result_id)
                    .iter()
                    .any(|item| {
                        text(item, "status") == "Open" && text(item, "reason") == "RepeatedRework"
                    })
                {
                    return Err(bad("BAD_STATE","An identical correction with unchanged output already consumed allowance; new evidence or a plan change is required"));
                }
                match input.action.as_str() {
                    "CollectEvidence" => {
                        result["evaluationRound"] = json!(number(&result, "evaluationRound") + 1);
                        result["disposition"] = json!("Submitted");
                        result
                            .as_object_mut()
                            .map(|o| o.remove("evaluationStarted"));
                        self.put(result);
                        task["state"] = json!("Submitted");
                    }
                    "ReviseOutput" => {
                        task["state"] = json!("Ready");
                        task["reworkId"] = json!(input.rework_id);
                        task.as_object_mut().map(|o| o.remove("acceptedResultId"));
                    }
                    "Replan" => {
                        return Err(bad(
                            "METHOD_UNSUPPORTED",
                            "Apply an explicit validated plan proposal for Replan; no fake replan operation is recorded",
                        ));
                    }
                    _ => return Err(bad("INVALID_ARGUMENT", "Unknown rework action")),
                }
                rework["status"] = json!("Applied");
                rework["appliedAction"] = json!(input.action);
                self.put(rework);
                let task = self.put(task);
                self.resolve_attention(&input.result_id);
                let operation=self.create("Operation",json!({"workId":task["workId"],"status":"Succeeded",
                    "steps":[{"state":"Confirmed","intent":"task.rework"}],"result":{"action":input.action,"taskId":input.task_id}}));
                self.emit("OperationChanged", &operation);
                Ok(Response::ok(
                    "",
                    json!({"operationId":operation["id"],"scheduledUnitIds":[task["id"]]}),
                ))
            }
            "delivery.prepare" => {
                let input: DeliveryPrepare = parse(&request.params)?;
                let work = self.record(&input.work_id, "Work")?;
                if !Self::legacy_mode(&work) {
                    return Err(bad("EXECUTOR_DELIVERY_UNAVAILABLE","Executor delivery requires a separately approved evidence and acceptance contract; a chat reply is not acceptance"));
                }
                let result = self.record(&input.integration_result_id, "TaskResult")?;
                self.coordinator(principal, Some(&input.work_id))?;
                self.matched(request, &[&work, &result])?;
                let candidate = self.prepare_delivery(&work, &result)?;
                Ok(Response::ok(
                    "",
                    json!({"candidateId":candidate["id"],"version":candidate["version"],"criterionMappings":candidate["criterionMappings"],"destination":candidate["destination"]}),
                ))
            }
            "delivery.accept" => {
                Self::human(principal)?;
                let input: DeliveryAccept = parse(&request.params)?;
                let mut candidate = self.record(&input.candidate_id, "DeliveryCandidate")?;
                let mut work = self.record(text(&candidate, "workId"), "Work")?;
                if !Self::legacy_mode(&work) {
                    return Err(bad("EXECUTOR_DELIVERY_UNAVAILABLE","Legacy delivery evidence cannot accept work modified by the persistent executor"));
                }
                self.matched(request, &[&work, &candidate])?;
                if text(&candidate, "status") != "Proposed"
                    || text(&work, "currentCandidateId") != input.candidate_id
                    || candidate["specRevision"] != work["currentSpecRevision"]
                    || text(&work, "desiredAdvancement") != "Advance"
                {
                    return Err(bad(
                        "STALE_VERSION",
                        "Candidate/specification/advancement is not current",
                    ));
                }
                let artifacts: Vec<ArtifactRef> = parse(&candidate["artifacts"])?;
                self.artifacts(&artifacts)?;
                let result = self.record(text(&candidate, "integrationResultId"), "TaskResult")?;
                if text(&result, "disposition") != "Accepted" || !self.acceptance_ready(&result)? {
                    return Err(bad("BAD_STATE", "Candidate evidence is no longer accepted"));
                }
                if let Some(source_input) = candidate["destination"].get("sourceInput") {
                    let (_, pinned) = self.inherited_delivery_code(&result)?;
                    if &pinned != source_input
                        || candidate["destination"]["artifact"] != pinned["artifact"]
                        || candidate["destination"]["sourceResultId"] != pinned["sourceResultId"]
                    {
                        return Err(bad("STALE_EVALUATION", "Candidate code provenance no longer matches its accepted integration inputs"));
                    }
                }
                let work_id = text(&work, "id").to_owned();
                if self
                    .related("Attempt", "workId", &work_id)
                    .iter()
                    .any(|attempt| attempt["reservationHeld"] == true)
                    || self
                        .related("Workspace", "workId", &work_id)
                        .iter()
                        .any(|workspace| text(workspace, "writer") == "Human")
                {
                    return Err(bad(
                        "BAD_STATE",
                        "Managed writers must settle before accepting delivery",
                    ));
                }
                if self
                    .related("AttentionItem", "workId", &work_id)
                    .iter()
                    .any(|item| {
                        text(item, "status") == "Open"
                            && text(item, "subjectId") != input.candidate_id
                    })
                {
                    return Err(bad(
                        "BAD_STATE",
                        "Completion-relevant obligations remain open",
                    ));
                }
                let acceptance=self.create("Acceptance",json!({"workId":work_id,"candidateId":candidate["id"],
                    "specRevision":candidate["specRevision"],"actor":principal.key(),"evidenceManifest":candidate["criterionMappings"]}));
                candidate["status"] = json!("Accepted");
                let candidate = self.put(candidate);
                work["currentAcceptanceId"] = acceptance["id"].clone();
                self.put(work);
                self.resolve_attention(&input.candidate_id);
                self.emit("DeliveryAccepted", &candidate);
                self.finalize_works()?;
                let current = self.record(&work_id, "Work")?;
                Ok(Response::ok(
                    "",
                    json!({"acceptanceId":acceptance["id"],"version":current["version"],
                    "phase":if text(&current,"lifecycle")=="Completed" {"Completed"} else {"Finalizing"}}),
                ))
            }
            "delivery.request_changes" => {
                Self::human(principal)?;
                let input: DeliveryChanges = parse(&request.params)?;
                let mut candidate = self.record(&input.candidate_id, "DeliveryCandidate")?;
                let mut work = self.record(text(&candidate, "workId"), "Work")?;
                self.matched(request, &[&work, &candidate])?;
                if text(&candidate, "status") != "Proposed"
                    || text(&work, "currentCandidateId") != input.candidate_id
                {
                    return Err(bad("STALE_VERSION", "Candidate is not current"));
                }
                if input.findings.is_empty() {
                    return Err(bad(
                        "INVALID_ARGUMENT",
                        "Revision requires concrete findings",
                    ));
                }
                self.artifacts(&input.preserve_artifacts)?;
                for finding in &input.findings {
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
                let result = self.record(text(&candidate, "integrationResultId"), "TaskResult")?;
                let instruction = ReworkInstruction {
                    result_id: text(&result, "id").into(),
                    review_ids: vec![],
                    gate_result_ids: vec![],
                    reason: "UserRevision".into(),
                    findings: input
                        .findings
                        .into_iter()
                        .map(|finding| ReworkFinding {
                            criterion_id: finding.criterion_id,
                            evidence: vec![],
                            requested_change: finding.requested_change,
                        })
                        .collect(),
                    preserve_artifacts: input.preserve_artifacts,
                    action: "ReviseOutput".into(),
                };
                let rework = self.changes(&result, instruction, false)?;
                candidate["status"] = json!("Rejected");
                let candidate = self.put(candidate);
                work.as_object_mut().map(|o| o.remove("currentCandidateId"));
                work["desiredAdvancement"] = json!(if input.advance { "Advance" } else { "Hold" });
                self.put(work);
                self.resolve_attention(&input.candidate_id);
                self.emit("DeliveryChangesRequested", &candidate);
                Ok(Response::ok(
                    "",
                    json!({"candidateId":candidate["id"],"reworkIds":[rework["id"]]}),
                ))
            }
            _ => Err(bad("METHOD_UNSUPPORTED", &request.method)),
        }
    }

    fn round(&self, result: &Value, round: u64) -> DomainResult<()> {
        let task = self.record(text(result, "taskId"), "Task")?;
        if round != number(result, "evaluationRound")
            || text(&task, "currentResultId") != text(result, "id")
        {
            Err(bad(
                "STALE_EVALUATION",
                "Evaluation round or result currency changed",
            ))
        } else {
            Ok(())
        }
    }

    fn check_evaluation(
        &self,
        dispatch: &Value,
        unit_id: &str,
        result_id: &str,
        round: u64,
        revision: u64,
    ) -> DomainResult<Value> {
        let unit = self.record(unit_id, "EvaluationUnit")?;
        let result = self.record(result_id, "TaskResult")?;
        self.round(&result, round)?;
        if text(dispatch, "evaluationUnitId") != unit_id
            || text(dispatch, "subjectResultId") != result_id
            || number(dispatch, "evaluationRound") != round
            || revision != number(&result, "taskRevision")
            || text(&unit, "state") != "Running"
        {
            return Err(bad(
                "STALE_EVALUATION",
                "Submission targets another evaluation unit",
            ));
        }
        Ok(unit)
    }

    pub(super) fn evaluate_ready(&mut self) -> DomainResult<()> {
        for original in self.all("TaskResult") {
            if !["Submitted", "Reviewing"].contains(&text(&original, "disposition")) {
                continue;
            }
            let work = self.record(text(&original, "workId"), "Work")?;
            if !Self::legacy_mode(&work) || text(&work, "desiredAdvancement") != "Advance" {
                continue;
            }
            let attempt = self.record(text(&original, "attemptId"), "Attempt")?;
            if attempt["reservationHeld"] == true {
                continue;
            }
            let mut result = self.record(text(&original, "id"), "TaskResult")?;
            let task = self.record(text(&result, "taskId"), "Task")?;
            if text(&task, "currentResultId") != text(&result, "id") {
                continue;
            }
            if result["evaluationStarted"] != true {
                if let Some(fields) = result.as_object_mut() {
                    fields.remove("evaluationFailures");
                }
                result["evaluationStarted"] = json!(true);
                result["producerSettled"] = json!(true);
                result["disposition"] = json!("Reviewing");
                let result = self.put(result);
                let inputs = self.evaluation_inputs(&result)?;
                for gate in values(&task, "gateDefinitions") {
                    self.create("EvaluationUnit",json!({"workId":result["workId"],"taskId":task["id"],"resultId":result["id"],
                        "evaluationRound":result["evaluationRound"],"dispatchKind":"EvaluateGate","gateDefinitionId":gate["id"],
                        "gateDefinition":gate,"required":gate["required"],"inputs":inputs,"inputManifestDigest":digest(&json!(inputs))?,"state":"Queued"}));
                }
            }
            let result = self.record(text(&original, "id"), "TaskResult")?;
            let units: Vec<_> = self
                .related("EvaluationUnit", "resultId", text(&result, "id"))
                .into_iter()
                .filter(|unit| unit["evaluationRound"] == result["evaluationRound"])
                .collect();
            let required: Vec<_> = units
                .iter()
                .filter(|unit| {
                    text(unit, "dispatchKind") == "EvaluateGate" && unit["required"] == true
                })
                .collect();
            if required.iter().any(|unit| text(unit, "state") != "Settled") {
                continue;
            }
            let mut failed = Vec::new();
            let mut inconclusive = Vec::new();
            for unit in &required {
                let gate = self.records.get(text(unit, "terminalRecordId"));
                match gate.map(|gate| text(gate, "outcome")) {
                    Some("Passed") => {}
                    Some("Failed") => failed.push((*unit).clone()),
                    _ => inconclusive.push((*unit).clone()),
                }
            }
            if !failed.is_empty() || !inconclusive.is_empty() {
                let failure = !failed.is_empty();
                let selected: Vec<_> = failed.into_iter().chain(inconclusive).collect();
                let needs_repair = selected.iter().any(Self::evaluation_needs_repair);
                let runtime_failure = selected
                    .iter()
                    .any(|unit| text(unit, "failure") == "EXECUTION_FAILED");
                let mut findings = Vec::new();
                let mut gate_ids = Vec::new();
                for unit in selected {
                    let gate = self.records.get(text(&unit, "terminalRecordId"));
                    let evidence = gate
                        .map(|gate| values(gate, "evidence"))
                        .unwrap_or_default();
                    if let Some(gate) = gate {
                        gate_ids.push(text(gate, "id").to_string());
                    }
                    for criterion in values(&unit["gateDefinition"], "criterionIds") {
                        findings.push(ReworkFinding {
                            criterion_id: criterion.as_str().unwrap_or("").into(),
                            evidence: parse(&json!(evidence))?,
                            requested_change: if Self::evaluation_needs_repair(&unit) {
                                Self::evaluation_recovery_finding(&unit)
                            } else {
                                gate
                                    .map(|gate| text(gate, "explanation"))
                                    .filter(|text| !text.is_empty())
                                    .unwrap_or(
                                        "Obtain a complete, settled execution of the declared check",
                                    )
                                    .into()
                            },
                        });
                    }
                }
                self.changes(
                    &result,
                    ReworkInstruction {
                        result_id: text(&result, "id").into(),
                        review_ids: vec![],
                        gate_result_ids: gate_ids,
                        reason: if failure {
                            "ContractViolation"
                        } else {
                            "NeedsEvidence"
                        }
                        .into(),
                        findings,
                        preserve_artifacts: parse(&json!(values(&result["body"], "outputs")
                            .iter()
                            .map(|output| output["artifact"].clone())
                            .collect::<Vec<_>>()))?,
                        action: if needs_repair {
                            if runtime_failure {
                                "ReviseOutput"
                            } else {
                                "Replan"
                            }
                        } else if failure {
                            "ReviseOutput"
                        } else {
                            "CollectEvidence"
                        }
                        .into(),
                    },
                    false,
                )?;
                continue;
            }
            if task["reviewPolicy"]["required"] == true {
                let review_unit = units
                    .iter()
                    .find(|unit| text(unit, "dispatchKind") == "ReviewResult");
                if let Some(unit) = review_unit {
                    if text(unit, "state") != "Settled" {
                        continue;
                    }
                    let review = self.records.get(text(unit, "terminalRecordId")).cloned();
                    if review
                        .as_ref()
                        .is_none_or(|review| text(review, "recommendation") != "Accept")
                    {
                        let recommendation = review
                            .as_ref()
                            .map(|review| text(review, "recommendation"))
                            .unwrap_or("NeedsEvidence");
                        let mut findings = Vec::new();
                        if let Some(review) = &review {
                            for finding in values(review, "findings") {
                                findings.push(ReworkFinding {
                                    criterion_id: text(&finding, "criterionId").into(),
                                    evidence: parse(&finding["evidence"])?,
                                    requested_change: text(&finding, "requestedChange").into(),
                                });
                            }
                        }
                        if findings.is_empty() {
                            for criterion in values(&task, "criteria") {
                                findings.push(ReworkFinding{
                                    criterion_id:text(&criterion,"id").into(),evidence:vec![],
                                    requested_change:if Self::evaluation_needs_repair(unit) {
                                        Self::evaluation_recovery_finding(unit)
                                    } else {
                                        "Obtain the required structured review and its settlement receipt".into()
                                    }
                                });
                            }
                        }
                        self.changes(
                            &result,
                            ReworkInstruction {
                                result_id: text(&result, "id").into(),
                                review_ids: review
                                    .as_ref()
                                    .map(|review| vec![text(review, "id").into()])
                                    .unwrap_or_default(),
                                gate_result_ids: required
                                    .iter()
                                    .filter_map(|unit| {
                                        unit.get("terminalRecordId")
                                            .and_then(Value::as_str)
                                            .map(str::to_owned)
                                    })
                                    .collect(),
                                reason: if recommendation == "NeedsEvidence" {
                                    "NeedsEvidence"
                                } else {
                                    "ContractViolation"
                                }
                                .into(),
                                findings,
                                preserve_artifacts: review
                                    .as_ref()
                                    .map(|review| parse(&review["preserveArtifacts"]))
                                    .transpose()?
                                    .unwrap_or_default(),
                                action: if text(unit, "failure") == "EXECUTION_FAILED" {
                                    "ReviseOutput"
                                } else if Self::evaluation_needs_repair(unit) {
                                    "Replan"
                                } else {
                                    match recommendation {
                                        "NeedsEvidence" => "CollectEvidence",
                                        "Reject" => "Replan",
                                        _ => "ReviseOutput",
                                    }
                                }
                                .into(),
                            },
                            recommendation == "Reject",
                        )?;
                        continue;
                    }
                } else {
                    let gate_ids: Vec<_> = required
                        .iter()
                        .filter_map(|unit| unit.get("terminalRecordId").cloned())
                        .collect();
                    let manifest =
                        json!({"resultDigest":result["submissionDigest"],"gateResultIds":gate_ids});
                    let mut inputs = self.evaluation_inputs(&result)?;
                    for gate_id in &gate_ids {
                        let gate = self.record(gate_id.as_str().unwrap_or(""), "GateResult")?;
                        for (index, artifact) in values(&gate, "evidence").into_iter().enumerate() {
                            inputs.push(json!({"slot":format!("gate:{}:{index}",text(&gate,"gateDefinitionId")),
                                "artifact":artifact,"sourceResultId":result["id"],"sourceGateIds":[gate["id"]]}));
                        }
                    }
                    inputs.sort_by(|a, b| text(a, "slot").cmp(text(b, "slot")));
                    self.create("EvaluationUnit",json!({"workId":result["workId"],"taskId":task["id"],"resultId":result["id"],
                        "evaluationRound":result["evaluationRound"],"dispatchKind":"ReviewResult","required":true,
                        "inputs":inputs,"inputManifestDigest":digest(&json!(inputs))?,"evidenceManifestDigest":digest(&manifest)?,"gateResultIds":gate_ids,"state":"Queued"}));
                    continue;
                }
            }
            if self.acceptance_ready(&result)? {
                self.accept_result(&result)?;
            } else {
                let mut findings = Vec::new();
                for criterion in values(&task, "criteria") {
                    findings.push(ReworkFinding{criterion_id:text(&criterion,"id").into(),evidence:vec![],
                        requested_change:"Supply readable evidence for the declared output/schema criterion; existing known gaps cannot imply acceptance".into()});
                }
                self.changes(
                    &result,
                    ReworkInstruction {
                        result_id: text(&result, "id").into(),
                        review_ids: vec![],
                        gate_result_ids: vec![],
                        reason: "NeedsEvidence".into(),
                        findings,
                        preserve_artifacts: vec![],
                        action: "ReviseOutput".into(),
                    },
                    false,
                )?;
            }
        }
        Ok(())
    }

    fn evaluation_needs_repair(unit: &Value) -> bool {
        ["EXECUTION_FAILED", "CANCELLED", "CONTRACT_DECLINED"].contains(&text(unit, "failure"))
    }

    fn evaluation_recovery_finding(unit: &Value) -> String {
        let details = &unit["failureDetails"];
        let recovery = if text(unit, "failure") == "CANCELLED" {
            "Inspect the recorded cancellation and advancement decision before explicitly replanning; cancellation is not evidence of a defective output."
        } else if text(unit, "failure") == "CONTRACT_DECLINED" {
            "Inspect the declined evaluation contract and reason before explicitly replanning the assignment."
        } else {
            "Inspect the pinned inputs and runtime preparation. Explicitly choose task.rework ReviseOutput to provide corrected captures under the unchanged contract, or CollectEvidence only after repairing the runtime/preparation cause. Preserve the approved check."
        };
        format!(
            "Evaluation {} for result {}: {} ({}). {} No process verdict was recorded. {} Do not blindly repeat CollectEvidence.",
            text(unit, "id"),
            text(unit, "resultId"),
            text(unit, "failure"),
            text(details, "phase"),
            text(details, "diagnostic"),
            recovery,
        )
    }

    pub(super) fn producing_dispatch(&self, result: &Value) -> DomainResult<Value> {
        let attempt = self.record(text(result, "attemptId"), "Attempt")?;
        let dispatch = self.record(text(&attempt, "dispatchId"), "TaskDispatch")?;
        if text(&dispatch, "dispatchKind") != "ProduceResult"
            || dispatch["id"] != result["body"]["dispatchId"]
            || dispatch["workId"] != result["workId"]
            || dispatch["taskId"] != result["taskId"]
            || dispatch["taskRevision"] != result["taskRevision"]
            || dispatch["inputManifestDigest"] != result["body"]["inputManifestDigest"]
            || digest(&dispatch["inputs"])? != text(&dispatch, "inputManifestDigest")
        {
            return Err(bad(
                "INVALID_REFERENCE",
                "Result does not identify its exact producing input manifest",
            ));
        }
        Ok(dispatch)
    }

    fn evaluation_inputs(&self, result: &Value) -> DomainResult<Vec<Value>> {
        let dispatch = self.producing_dispatch(result)?;
        // sourceResultId == this result identifies the output layer. Preserve the
        // producer's slots and provenance verbatim, including overlapping slot names.
        let mut inputs = values(&dispatch, "inputs");
        inputs.extend(values(&result["body"],"outputs").into_iter().map(|output|json!({
            "slot":output["slot"],"artifact":output["artifact"],"sourceResultId":result["id"],"sourceGateIds":[]
        })));
        inputs.sort_by(|a, b| {
            text(a, "slot")
                .cmp(text(b, "slot"))
                .then(text(&a["artifact"], "artifactId").cmp(text(&b["artifact"], "artifactId")))
                .then(text(a, "sourceResultId").cmp(text(b, "sourceResultId")))
                .then(text(&a["artifact"], "digest").cmp(text(&b["artifact"], "digest")))
        });
        Ok(inputs)
    }

    pub(super) fn acceptance_ready(&self, result: &Value) -> DomainResult<bool> {
        let task = self.record(text(result, "taskId"), "Task")?;
        if text(&task, "currentResultId") != text(result, "id") {
            return Ok(false);
        }
        let attempt = self.record(text(result, "attemptId"), "Attempt")?;
        if attempt["reservationHeld"] == true {
            return Ok(false);
        }
        let outputs: Vec<ResultOutput> = parse(&result["body"]["outputs"])?;
        for output in &outputs {
            self.artifacts(std::slice::from_ref(&output.artifact))?;
        }
        for evidence in values(&result["body"], "criterionEvidence") {
            self.artifacts(&parse::<Vec<ArtifactRef>>(&evidence["evidence"])?)?;
        }
        let units: Vec<_> = self
            .related("EvaluationUnit", "resultId", text(result, "id"))
            .into_iter()
            .filter(|unit| unit["evaluationRound"] == result["evaluationRound"])
            .collect();
        for gate in values(&task, "gateDefinitions")
            .iter()
            .filter(|gate| gate["required"] == true)
        {
            let Some(unit) = units
                .iter()
                .find(|unit| unit["gateDefinitionId"] == gate["id"])
            else {
                return Ok(false);
            };
            let Some(record) = self.records.get(text(unit, "terminalRecordId")) else {
                return Ok(false);
            };
            if text(unit, "state") != "Settled" || text(record, "outcome") != "Passed" {
                return Ok(false);
            }
            self.artifacts(&parse::<Vec<ArtifactRef>>(&record["evidence"])?)?;
        }
        // Optional passing checks can also appear in the delivery's criterion
        // mappings, so their evidence must remain readable as well.
        for unit in &units {
            if text(unit, "dispatchKind") == "EvaluateGate" && unit["required"] != true {
                if let Some(gate) = self.records.get(text(unit, "terminalRecordId")) {
                    if text(gate, "outcome") == "Passed" {
                        self.artifacts(&parse::<Vec<ArtifactRef>>(&gate["evidence"])?)?;
                    }
                }
            }
        }
        if task["reviewPolicy"]["required"] == true {
            let Some(unit) = units
                .iter()
                .find(|unit| text(unit, "dispatchKind") == "ReviewResult")
            else {
                return Ok(false);
            };
            let Some(review) = self.records.get(text(unit, "terminalRecordId")) else {
                return Ok(false);
            };
            if text(unit, "state") != "Settled" || text(review, "recommendation") != "Accept" {
                return Ok(false);
            }
            self.artifacts(&parse::<Vec<ArtifactRef>>(&review["preserveArtifacts"])?)?;
            for finding in values(review, "findings") {
                self.artifacts(&parse::<Vec<ArtifactRef>>(&finding["evidence"])?)?;
            }
        }
        for criterion in values(&task, "criteria") {
            for rule in values(&criterion, "requiredEvidence") {
                if let Some(slot) = rule
                    .as_str()
                    .and_then(|rule| rule.strip_prefix("artifact:"))
                {
                    let Some(output) = outputs.iter().find(|output| output.slot == slot) else {
                        return Ok(false);
                    };
                    let evidence = values(&result["body"], "criterionEvidence")
                        .into_iter()
                        .find(|entry| entry["criterionId"] == criterion["id"]);
                    if evidence.as_ref().is_none_or(|entry|!values(entry,"evidence").contains(&json!({"artifactId":output.artifact.artifact_id,"digest":output.artifact.digest}))) {return Ok(false);}
                }
            }
        }
        Ok(true)
    }

    pub(super) fn validate_instruction(
        &self,
        result: &Value,
        instruction: &ReworkInstruction,
    ) -> DomainResult<()> {
        if ![
            "NeedsEvidence",
            "ContractViolation",
            "Misunderstood",
            "UserRevision",
        ]
        .contains(&instruction.reason.as_str())
            || !["CollectEvidence", "ReviseOutput", "Replan"].contains(&instruction.action.as_str())
            || instruction.findings.is_empty()
        {
            return Err(bad(
                "INVALID_ARGUMENT",
                "Rework needs a typed reason/action and criterion-bound findings",
            ));
        }
        let task = self.record(text(result, "taskId"), "Task")?;
        for finding in &instruction.findings {
            if !values(&task, "criteria")
                .iter()
                .any(|criterion| text(criterion, "id") == finding.criterion_id)
            {
                return Err(bad("INVALID_REFERENCE", "Unknown rework criterion"));
            }
            nonempty(&finding.requested_change, "requestedChange")?;
            self.artifacts(&finding.evidence)?;
        }
        self.artifacts(&instruction.preserve_artifacts)?;
        for gate in &instruction.gate_result_ids {
            let gate = self.record(gate, "GateResult")?;
            if text(&gate, "resultId") != text(result, "id")
                || gate["evaluationRound"] != result["evaluationRound"]
            {
                return Err(bad("STALE_EVALUATION", "Rework gate evidence is stale"));
            }
        }
        for review in &instruction.review_ids {
            let review = self.record(review, "TaskReview")?;
            if text(&review, "resultId") != text(result, "id")
                || review["evaluationRound"] != result["evaluationRound"]
            {
                return Err(bad("STALE_EVALUATION", "Rework review evidence is stale"));
            }
        }
        Ok(())
    }

    fn changes(
        &mut self,
        result: &Value,
        instruction: ReworkInstruction,
        rejected: bool,
    ) -> DomainResult<Value> {
        self.validate_instruction(result, &instruction)?;
        if !["Submitted", "Reviewing", "Accepted"].contains(&text(result, "disposition")) {
            return Err(bad(
                "BAD_STATE",
                "Result already has a correction disposition",
            ));
        }
        let failures: Vec<_> = self
            .related("EvaluationUnit", "resultId", text(result, "id"))
            .into_iter()
            .filter(|unit| unit["evaluationRound"] == result["evaluationRound"])
            .filter_map(|unit| {
                let mut details = unit.get("failureDetails")?.clone();
                if let Some(gate) = unit.get("gateDefinitionId") {
                    details["gateDefinitionId"] = gate.clone();
                }
                Some(details)
            })
            .collect();
        let mut normalized_findings: Vec<_> = instruction
            .findings
            .iter()
            .map(|finding| {
                let mut requested_change = finding.requested_change.clone();
                for failure in &failures {
                    for field in ["evaluationUnitId", "invocationId", "attemptId", "resultId"] {
                        let identity = text(failure, field);
                        if !identity.is_empty() {
                            let replacement = if field == "evaluationUnitId" {
                                format!("evaluationUnit:{}", text(failure, "gateDefinitionId"))
                            } else {
                                field.into()
                            };
                            requested_change = requested_change.replace(identity, &replacement);
                        }
                    }
                }
                let mut evidence: Vec<_> = finding
                    .evidence
                    .iter()
                    .map(|artifact| artifact.digest.clone())
                    .collect();
                evidence.sort();
                (finding.criterion_id.clone(), requested_change, evidence)
            })
            .collect();
        normalized_findings.sort();
        let signature_findings: Vec<_> = normalized_findings.into_iter()
            .map(|(criterion, requested_change, evidence)| json!({
                "criterionId":criterion,"requestedChange":requested_change,"evidenceDigests":evidence
            })).collect();
        let signature = digest(&json!({
            "outputs":values(&result["body"],"outputs").iter().map(|output|json!({"slot":output["slot"],"digest":output["artifact"]["digest"]})).collect::<Vec<_>>(),
            "reason":instruction.reason,"action":instruction.action,
            "findings":signature_findings
        }))?;
        let repeated = self
            .related("ReworkInstruction", "taskId", text(result, "taskId"))
            .iter()
            .any(|prior| {
                text(prior, "status") == "Applied" && text(prior, "signature") == signature
            });
        let mut rework=self.create("ReworkInstruction",json!({"workId":result["workId"],"taskId":result["taskId"],"resultId":result["id"],
            "evaluationRound":result["evaluationRound"],"instruction":encode(&instruction)?,"signature":signature,"status":"Open"}));
        let mut result = result.clone();
        if !failures.is_empty() {
            result["evaluationFailures"] = json!(failures);
            rework["evaluationFailures"] = result["evaluationFailures"].clone();
            rework["recovery"] = json!({
                "kind":if failures.iter().any(|failure| failure["code"] == "EXECUTION_FAILED") {"RepairEvaluation"} else {"InspectEvaluation"},
                "action":instruction.action,"automaticRetry":false,
                "preserveApprovedChecks":true
            });
            if failures
                .iter()
                .any(|failure| failure["code"] == "EXECUTION_FAILED")
            {
                rework["recovery"]["allowedActions"] = json!(["ReviseOutput", "CollectEvidence"]);
            }
            rework = self.put(rework);
        }
        result["disposition"] = json!(if rejected {
            "Rejected"
        } else {
            "ChangesRequested"
        });
        result["reworkId"] = rework["id"].clone();
        let result = self.put(result);
        let mut task = self.record(text(&result, "taskId"), "Task")?;
        task["state"] = json!("NeedsRework");
        task.as_object_mut().map(|o| o.remove("acceptedResultId"));
        self.put(task);
        self.emit(
            if rejected {
                "ResultRejected"
            } else {
                "ResultChangesRequested"
            },
            &result,
        );
        self.queue_coordination(
            Some(text(&result, "workId")),
            None,
            "ChangesRequested",
            &result,
        )?;
        if repeated {
            self.attention(text(&result,"workId"),text(&result,"id"),"RepeatedRework",
                "Inspect unchanged output and choose new evidence, a revised plan, or explicit scope/allowance approval");
        }
        Ok(rework)
    }

    fn accept_result(&mut self, original: &Value) -> DomainResult<()> {
        let mut result = self.record(text(original, "id"), "TaskResult")?;
        if text(&result, "disposition") == "Accepted" {
            return Ok(());
        }
        if !self.acceptance_ready(&result)? {
            return Err(bad(
                "BAD_STATE",
                "Required acceptance predicates do not pass",
            ));
        }
        let mut task = self.record(text(&result, "taskId"), "Task")?;
        let mut work = self.record(text(&result, "workId"), "Work")?;
        if text(&work, "desiredAdvancement") != "Advance" {
            return Err(bad("BAD_STATE", "Work is held/cancelled"));
        }
        let plan = self.record(text(&work, "planId"), "Plan")?;
        let integration = text(&task, "clientKey") == text(&plan, "integrationTaskKey");
        if integration {
            let attempt = self.record(text(&result, "attemptId"), "Attempt")?;
            let dispatch = self.record(text(&attempt, "dispatchId"), "TaskDispatch")?;
            if text(&task, "role") == "Integration"
                && (number(&dispatch["integrationBase"], "headGeneration")
                    != number(&work, "headGeneration")
                    || text(&dispatch["integrationBase"], "parentResultId")
                        != text(&work, "integrationResultId"))
            {
                self.attention(
                    text(&work, "id"),
                    text(&result, "id"),
                    "StaleIntegrationHead",
                    "Rebase and re-evaluate against the recorded current integration parent",
                );
                return Ok(());
            }
            for input in values(&dispatch, "inputs") {
                if let Some(source_result) = input.get("sourceResultId").and_then(Value::as_str) {
                    let source = self.record(source_result, "TaskResult")?;
                    let source_task = self.record(text(&source, "taskId"), "Task")?;
                    if text(&source_task, "acceptedResultId") != source_result {
                        self.attention(
                            text(&work, "id"),
                            text(&result, "id"),
                            "StaleIntegrationInput",
                            "Rebuild integration with current accepted contribution versions",
                        );
                        return Ok(());
                    }
                }
            }
        }
        result["disposition"] = json!("Accepted");
        let result = self.put(result);
        task["acceptedResultId"] = result["id"].clone();
        task["state"] = json!("Accepted");
        self.put(task);
        if integration {
            work["integrationResultId"] = result["id"].clone();
            work["headGeneration"] = json!(number(&work, "headGeneration") + 1);
        }
        let work = self.put(work);
        self.resolve_attention(text(&result, "id"));
        self.resolve_attention(text(&result, "taskId"));
        self.emit("TaskResultAccepted", &result);
        self.emit("WorkChanged", &work);
        if integration {
            self.prepare_delivery(&work, &result)?;
        }
        Ok(())
    }

    pub(super) fn inherited_delivery_code(&self, result: &Value) -> DomainResult<(Value, Value)> {
        let dispatch = self.producing_dispatch(result)?;
        let attempt = self.record(text(result, "attemptId"), "Attempt")?;
        if text(&dispatch, "role") != "Integration" || text(&attempt, "mode") != "ReadOnly" {
            return Err(bad(
                "BAD_STATE",
                "Only read-only integration can deliver an unchanged pinned upstream code snapshot",
            ));
        }
        let mut selected: Option<(Value, Value)> = None;
        for input in values(&dispatch, "inputs") {
            let artifact = self.record(text(&input["artifact"], "artifactId"), "Artifact")?;
            if !["GitCommit", "Tree"].contains(&text(&artifact, "artifactKind")) {
                continue;
            }
            let source = self.record(text(&input, "sourceResultId"), "TaskResult")?;
            let source_task = self.record(text(&source, "taskId"), "Task")?;
            if source["workId"] != result["workId"]
                || text(&source, "disposition") != "Accepted"
                || source_task["acceptedResultId"] != source["id"]
                || source_task["currentResultId"] != source["id"]
                || !values(&source["body"], "outputs")
                    .iter()
                    .any(|output| output["artifact"] == input["artifact"])
            {
                return Err(bad(
                    "STALE_EVALUATION",
                    "Pinned delivery code must belong to a current accepted upstream result",
                ));
            }
            self.artifacts(&parse::<Vec<ArtifactRef>>(&json!([input["artifact"]]))?)?;
            if let Some((_, prior)) = &selected {
                if prior["artifact"] != input["artifact"]
                    || prior["sourceResultId"] != input["sourceResultId"]
                {
                    return Err(bad("BAD_STATE", "Read-only integration has multiple code snapshots; submit an explicit immutable integrated code output"));
                }
            } else {
                selected = Some((artifact, input));
            }
        }
        let (code, input) = selected.ok_or_else(|| bad("BAD_STATE", "Local code delivery requires a fixed output or one pinned accepted upstream code snapshot"))?;
        for output in values(&result["body"], "outputs") {
            let artifact = self.record(text(&output["artifact"], "artifactId"), "Artifact")?;
            for entry in values(&artifact["manifest"], "entries") {
                if let Some(original) =
                    values(&code["manifest"], "entries")
                        .into_iter()
                        .find(|original| {
                            text(original, "path").eq_ignore_ascii_case(text(&entry, "path"))
                        })
                {
                    if original["digest"] != entry["digest"] || original["kind"] != entry["kind"] {
                        return Err(bad("BAD_STATE", "Integration output changes the pinned code tree; submit an explicit immutable integrated code snapshot"));
                    }
                }
            }
        }
        Ok((code, input))
    }

    fn prepare_delivery(&mut self, work: &Value, result: &Value) -> DomainResult<Value> {
        if text(result, "disposition") != "Accepted"
            || text(work, "integrationResultId") != text(result, "id")
        {
            return Err(bad(
                "BAD_STATE",
                "Delivery requires the current accepted integration head",
            ));
        }
        for task in self.related("Task", "workId", text(work, "id")) {
            if task["requiredForDelivery"] == true
                && text(&task, "acceptedResultId") != text(&task, "currentResultId")
            {
                return Err(bad(
                    "BAD_STATE",
                    "Required contribution has no current accepted outputs",
                ));
            }
        }
        if let Some(candidate) = self.records.get(text(work, "currentCandidateId")) {
            if candidate["integrationResultId"] == result["id"]
                && text(candidate, "status") == "Proposed"
            {
                return Ok(candidate.clone());
            }
        }
        let workspace = self.record(text(work, "workspaceId"), "Workspace")?;
        let outputs = values(&result["body"], "outputs");
        let mut artifacts: Vec<_> = outputs
            .iter()
            .map(|output| output["artifact"].clone())
            .collect();
        let mut destination = json!({"workspaceId":workspace["id"],"localRoot":workspace["localRoot"],"kind":work["spec"]["delivery"]["kind"]});
        if text(&work["spec"]["delivery"], "kind") == "LocalCode" {
            let own_code = outputs
                .iter()
                .filter_map(|output| self.records.get(text(&output["artifact"], "artifactId")))
                .find(|artifact| ["GitCommit", "Tree"].contains(&text(artifact, "artifactKind")))
                .cloned();
            let (code, inherited) = if let Some(code) = own_code {
                (code, false)
            } else {
                let (code, input) = self.inherited_delivery_code(result)?;
                artifacts.push(input["artifact"].clone());
                destination["sourceResultId"] = input["sourceResultId"].clone();
                destination["sourceInput"] = input;
                (code, true)
            };
            destination["artifact"] = json!({"artifactId":code["id"],"digest":code["digest"]});
            destination["localPath"] = code["localPath"].clone();
            for field in ["commitId", "branch"] {
                if let Some(value) = code.get(field).or_else(|| {
                    if inherited {
                        None
                    } else {
                        workspace.get(field)
                    }
                }) {
                    destination[field] = value.clone();
                }
            }
        } else {
            destination["reports"] = json!(outputs
                .iter()
                .filter_map(|output| self.records.get(text(&output["artifact"], "artifactId")))
                .map(|artifact| artifact["localPath"].clone())
                .collect::<Vec<_>>());
        }
        let task = self.record(text(result, "taskId"), "Task")?;
        let gates: Vec<_> = self
            .related("GateResult", "resultId", text(result, "id"))
            .into_iter()
            .filter(|gate| {
                gate["evaluationRound"] == result["evaluationRound"]
                    && text(gate, "outcome") == "Passed"
            })
            .collect();
        let mappings:Vec<_>=values(&work["spec"],"criteria").iter().map(|criterion|json!({"criterionId":criterion["id"],
            "evidence":values(&result["body"],"criterionEvidence").into_iter().filter(|e|e["criterionId"]==criterion["id"]).collect::<Vec<_>>(),
            "gateResultIds":gates.iter().filter(|gate|values(&task,"gateDefinitions").iter().any(|def|def["id"]==gate["gateDefinitionId"] && values(def,"criterionIds").contains(&criterion["id"]))).map(|gate|gate["id"].clone()).collect::<Vec<_>>()})).collect();
        if let Some(mut previous) = self.records.get(text(work, "currentCandidateId")).cloned() {
            previous["status"] = json!("Superseded");
            self.put(previous.clone());
            self.resolve_attention(text(&previous, "id"));
        }
        let candidate=self.create("DeliveryCandidate",json!({"workId":work["id"],"specRevision":work["currentSpecRevision"],"planRevision":work["currentPlanRevision"],
            "integrationResultId":result["id"],"artifacts":artifacts,"criterionMappings":mappings,"destination":destination,"knownLimitations":result["body"]["knownGaps"],
            "runRecipes":values(&task,"gateDefinitions").into_iter().filter_map(|gate|gate.get("recipe").cloned()).collect::<Vec<_>>(),"status":"Proposed"}));
        let mut work = work.clone();
        work["currentCandidateId"] = candidate["id"].clone();
        self.put(work);
        self.emit("DeliveryProposed", &candidate);
        self.attention(
            text(&candidate, "workId"),
            text(&candidate, "id"),
            "DeliveryAcceptance",
            "Inspect the fixed delivery and accept or request changes",
        );
        Ok(candidate)
    }
}
