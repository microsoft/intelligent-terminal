// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

use super::*;

pub(super) fn draft(f: &mut Fixture) -> Value {
    let response=f.command(Principal::Human,"work.create_draft",json!({
        "projectId":f.project,"goal":"Maintain this development server","scope":["reports"],"exclusions":[],
        "criteria":[{"id":"report","description":"Readable report","evidenceRule":"artifact:report"}],
        "context":[],"delivery":{"kind":"Report"},"sourceMessageIds":[]
    }),vec![]);
    assert_eq!(response.status, "ok", "{response:?}");
    f.engine
        .record(text(response.data.as_ref().unwrap(), "workId"), "Work")
        .unwrap()
}

pub(super) fn actor(f: &mut Fixture, work: &Value) -> Value {
    f.engine
        .related("Invocation", "workId", text(work, "id"))
        .into_iter()
        .find(|v| v["subject"]["kind"] == "Work" && v["state"] != "Released")
        .unwrap()
}

fn opened(f: &mut Fixture, work: &Value) -> Value {
    let response = f.command(
        Principal::Human,
        "work.open",
        json!({"workId":work["id"]}),
        vec![],
    );
    assert_eq!(response.status, "ok", "{response:?}");
    response.data.unwrap()
}

fn submit(f: &mut Fixture, view: &Value, message: &str) -> Response {
    let mut context = view["context"].clone();
    context.as_object_mut().unwrap().remove("conversationId");
    f.command(Principal::Human,"conversation.submit",json!({"conversationId":view["conversation"]["id"],"clientMessageId":id(),"text":message,"attachments":[],"context":context}),vec![])
}

pub(super) fn finish(f: &mut Fixture, invocation: &Value) {
    let current = f
        .engine
        .record(text(invocation, "id"), "Invocation")
        .unwrap();
    let turn = f
        .engine
        .record(text(&current, "currentExecutorTurnId"), "WorkExecutionTurn")
        .unwrap();
    let response = f.report(
        &current,
        "ExecutorTurnEnded",
        json!({"turnId":turn["id"],"turnNumber":turn["turnNumber"],"finish":"Normal"}),
    );
    assert_eq!(response.status, "ok", "{response:?}");
}

#[test]
fn executor_summary_reads_only_owned_recorded_replies_without_execution() {
    let mut f = Fixture::new();
    let work = draft(&mut f);
    f.start(&work);
    let invocation = actor(&mut f, &work);
    f.start_invocation(&invocation);
    let reply = "Investigated startup configuration; review report.md before making changes.";
    let report = f.report(
        &invocation,
        "TextDelta",
        json!({
            "messageId":invocation["replyMessageId"],"partId":"turn-1","chunkIndex":0,"text":reply
        }),
    );
    assert_eq!(report.status, "ok");
    finish(&mut f, &invocation);
    let saved_work = f.engine.record(text(&work, "id"), "Work").unwrap();
    let saved_actor = actor(&mut f, &work);
    let before_turns = f.engine.all("WorkExecutionTurn");
    let response = f.engine.handle(
        &Principal::Human,
        Request::new("work.get", json!({"workId":work["id"]})),
    );
    assert_eq!(response.status, "ok", "{response:?}");
    let view = response.data.unwrap();
    assert_eq!(
        view["executionSummary"]["source"],
        "RecordedExecutorConversation"
    );
    assert_eq!(
        view["executionSummary"]["recentResponses"][0]["text"],
        reply
    );
    assert_eq!(
        view["executionSummary"]["recentResponses"][0]["textTruncated"],
        false
    );
    assert_eq!(view["continuation"]["activity"], "Idle");
    assert_eq!(view["work"]["lifecycle"], "Active");
    assert_eq!(
        f.engine.record(text(&work, "id"), "Work").unwrap(),
        saved_work
    );
    assert_eq!(actor(&mut f, &work), saved_actor);
    assert_eq!(f.engine.all("WorkExecutionTurn"), before_turns);
    let other = draft(&mut f);
    assert!(f.engine.executor_summary(&other)["recentResponses"]
        .as_array()
        .unwrap()
        .is_empty());
    let mut message = f
        .engine
        .record(text(&invocation, "replyMessageId"), "ConversationItem")
        .unwrap();
    message["parts"] = json!([{"text":"界".repeat(5000)}]);
    f.engine.put(message.clone());
    let summary = f.engine.executor_summary(&saved_work);
    assert_eq!(
        summary["recentResponses"][0]["text"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        4096
    );
    assert_eq!(summary["recentResponses"][0]["textTruncated"], true);
    message["workId"] = json!(id());
    f.engine.put(message);
    assert!(f.engine.executor_summary(&saved_work)["recentResponses"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn executor_summary_caps_recent_responses_and_preserves_streaming_status() {
    let mut f = Fixture::new();
    let work = draft(&mut f);
    f.start(&work);
    let initial = actor(&mut f, &work);
    f.start_invocation(&initial);
    let view = opened(&mut f, &work);
    for ordinal in 1..=5 {
        if ordinal > 1 {
            assert_eq!(submit(&mut f, &view, "Continue this work").status, "ok");
        }
        let current = actor(&mut f, &work);
        f.report(
            &current,
            "TextDelta",
            json!({
                "messageId":current["replyMessageId"],"partId":"reply","chunkIndex":0,
                "text":format!("Response {ordinal}")
            }),
        );
        let summary = f.engine.executor_summary(&work);
        assert_eq!(
            summary["recentResponses"].as_array().unwrap().len(),
            ordinal.min(3)
        );
        assert_eq!(summary["recentResponses"][0]["ordinal"], ordinal);
        assert_eq!(summary["recentResponses"][0]["messageStatus"], "Streaming");
        finish(&mut f, &current);
    }
    let summary = f.engine.executor_summary(&work);
    assert_eq!(summary["turnCount"], 5);
    assert_eq!(summary["responseLimit"], 3);
    let responses = summary["recentResponses"].as_array().unwrap();
    for (index, response) in responses.iter().enumerate() {
        assert_eq!(response["ordinal"], 5 - index);
        assert_eq!(response["text"], format!("Response {}", 5 - index));
        assert_eq!(response["messageStatus"], "Complete");
    }
    let legacy = f.draft("Legacy projection remains unchanged");
    assert!(f.engine.view(&legacy).get("executionSummary").is_none());
}

#[test]
fn default_work_opens_without_dispatch_and_approval_starts_only_the_executor() {
    let mut f = Fixture::new();
    let work = draft(&mut f);
    assert_eq!(work["executionMode"], "WorkExecutor");
    let version = work["version"].clone();
    let first = opened(&mut f, &work);
    let second = opened(&mut f, &work);
    assert_eq!(first["context"], second["context"]);
    assert_eq!(
        f.engine.record(text(&work, "id"), "Work").unwrap()["version"],
        version
    );
    assert!(f
        .engine
        .related("Invocation", "workId", text(&work, "id"))
        .is_empty());
    assert!(f
        .engine
        .related("WorkExecutionTurn", "workId", text(&work, "id"))
        .is_empty());
    f.start(&work);
    let invocation = actor(&mut f, &work);
    assert_eq!(invocation["executorInput"]["workId"], work["id"]);
    assert!(f
        .engine
        .related("CoordinationTurn", "workId", text(&work, "id"))
        .is_empty());
    assert!(f
        .engine
        .related("Task", "workId", text(&work, "id"))
        .is_empty());
}

#[test]
fn busy_inputs_are_fifo_same_actor_and_idle_keeps_ownership_without_completion() {
    let mut f = Fixture::new();
    let work = draft(&mut f);
    f.start(&work);
    let invocation = actor(&mut f, &work);
    f.start_invocation(&invocation);
    let view = opened(&mut f, &work);
    assert_eq!(submit(&mut f, &view, "First follow-up").status, "ok");
    assert_eq!(submit(&mut f, &view, "Second follow-up").status, "ok");
    assert_eq!(
        f.engine
            .related("Invocation", "workId", text(&work, "id"))
            .len(),
        1
    );
    finish(&mut f, &invocation);
    let first = actor(&mut f, &work);
    assert_eq!(first["id"], invocation["id"]);
    let turn = f
        .engine
        .record(text(&first, "currentExecutorTurnId"), "WorkExecutionTurn")
        .unwrap();
    assert_eq!(turn["prompt"], "First follow-up");
    finish(&mut f, &invocation);
    let current = actor(&mut f, &work);
    assert_eq!(
        f.engine
            .record(text(&current, "currentExecutorTurnId"), "WorkExecutionTurn")
            .unwrap()["prompt"],
        "Second follow-up"
    );
    finish(&mut f, &invocation);
    let current = actor(&mut f, &work);
    assert_eq!(current["state"], "Idle");
    let mut expired = current.clone();
    expired["limits"]["deadlineUtc"] = json!("2000-01-01T00:00:00Z");
    f.engine.put(expired);
    let current_work = f.engine.record(text(&work, "id"), "Work").unwrap();
    assert_eq!(f.engine.work_continuation(&current_work)["state"], "Ready");
    assert_eq!(
        f.engine.work_continuation(&current_work)["activity"],
        "Idle"
    );
    assert_eq!(current_work["lifecycle"], "Active");
    assert_eq!(
        f.engine
            .record(text(&current_work, "workspaceId"), "Workspace")
            .unwrap()["executorInvocationId"],
        invocation["id"]
    );
    assert!(f
        .engine
        .all("Operation")
        .iter()
        .all(|op| op["effect"]["method"] != "runtime.release"));
    let usage = current_work["usage"].clone();
    let response = f.command(
        Principal::Human,
        "work.continue",
        json!({"workId":work["id"]}),
        vec![current_work],
    );
    assert_eq!(response.status, "ok", "{response:?}");
    assert_eq!(
        f.engine.record(text(&work, "id"), "Work").unwrap()["usage"],
        usage
    );
}

#[test]
fn executor_tool_authority_is_work_scoped_and_hold_fences_immediately() {
    let mut f = Fixture::new();
    let work = draft(&mut f);
    let other = draft(&mut f);
    f.start(&work);
    let invocation = actor(&mut f, &work);
    f.start_invocation(&invocation);
    let principal = Principal::Invocation {
        invocation_id: text(&invocation, "id").into(),
    };
    assert!(f
        .engine
        .executor_binding(&principal, text(&work, "id"))
        .is_ok());
    assert!(f
        .engine
        .executor_binding(&principal, text(&other, "id"))
        .is_err());
    let current = f.engine.record(text(&work, "id"), "Work").unwrap();
    let response = f.command(
        Principal::Human,
        "work.control",
        json!({"workId":work["id"],"action":"Hold"}),
        vec![current],
    );
    assert!(
        ["ok", "pending"].contains(&response.status.as_str()),
        "{response:?}"
    );
    assert!(f
        .engine
        .executor_binding(&principal, text(&work, "id"))
        .is_err());
    assert_eq!(actor(&mut f, &work)["state"], "Stopping");
}

#[test]
fn legacy_claim_settles_the_worker_and_adopts_its_session_not_the_coordinator() {
    let mut f = Fixture::new();
    let work = f.draft("Historical worker");
    f.start(&work);
    let coordinator = f.coordinator(&work);
    let worker = f.plan(&work, &coordinator, false);
    f.start_invocation(&worker);
    let current = f.engine.record(text(&work, "id"), "Work").unwrap();
    let mut claim = Request::new("work.claim_executor", json!({"workId":work["id"]}));
    claim.command_id = Some(id());
    claim.if_match = vec![Engine::reference(&current)];
    let response = f.engine.handle(&Principal::Human, claim.clone());
    assert_eq!(response.status, "ok", "{response:?}");
    assert_eq!(
        f.engine.record(text(&work, "id"), "Work").unwrap()["executionMode"],
        "ClaimingExecutor"
    );
    assert!(f
        .engine
        .related("Invocation", "workId", text(&work, "id"))
        .iter()
        .all(|invocation| invocation["subject"]["kind"] != "Work"));
    let replay = f.engine.handle(&Principal::Human, claim);
    assert_eq!(replay.status, "ok");
    f.finish_release(&worker);
    let current = f.engine.record(text(&work, "id"), "Work").unwrap();
    assert_eq!(current["executionMode"], "WorkExecutor");
    assert_eq!(current["executorSession"]["invocationId"], worker["id"]);
    assert_eq!(
        current["legacyCoordinatorSession"]["invocationId"],
        coordinator["id"]
    );
    let invocation = actor(&mut f, &work);
    assert_eq!(invocation["sessionReuseRef"], worker["id"]);
    assert_eq!(
        invocation["executorInput"]["sessionSource"]["originalInvocation"]["subject"]["kind"],
        "Task"
    );
    f.start_invocation(&invocation);
    let current = f.engine.record(text(&work, "id"), "Work").unwrap();
    assert_eq!(
        current["primarySession"]["providerSessionId"],
        format!("session-{}", text(&worker, "id"))
    );
    assert_eq!(
        f.engine.related("Task", "workId", text(&work, "id")).len(),
        1
    );
}

#[test]
fn coordinator_only_history_requires_explicit_reconstruction_consent() {
    let mut f = Fixture::new();
    let work = f.draft("No actual worker history");
    f.start(&work);
    let coordinator = f.coordinator(&work);
    let mut held = f.engine.record(text(&work, "id"), "Work").unwrap();
    held["desiredAdvancement"] = json!("Hold");
    f.engine.put(held);
    f.finish_release(&coordinator);
    let current = f.engine.record(text(&work, "id"), "Work").unwrap();
    let response = f.command(
        Principal::Human,
        "work.claim_executor",
        json!({"workId":work["id"]}),
        vec![current],
    );
    assert_eq!(response.status, "ok", "{response:?}");
    let current = f.engine.record(text(&work, "id"), "Work").unwrap();
    assert_eq!(current["executorClaim"]["status"], "NeedsConsent");
    let continuation = f.engine.work_continuation(&current);
    assert_eq!(continuation["canClaimExecutor"], false);
    assert_eq!(continuation["canRestartSession"], true);
    assert_eq!(
        current["continuationFailure"]["code"],
        "EXECUTOR_SESSION_UNAVAILABLE"
    );
    assert!(f
        .engine
        .related("Invocation", "workId", text(&work, "id"))
        .iter()
        .all(|invocation| invocation["subject"]["kind"] != "Work"));
    let repeated = f.command(
        Principal::Human,
        "work.claim_executor",
        json!({"workId":work["id"]}),
        vec![current.clone()],
    );
    assert_eq!(
        repeated.failure.unwrap().code,
        "EXECUTOR_SESSION_UNAVAILABLE"
    );
    let grant = f
        .engine
        .record(text(&current, "currentGrantId"), "ExecutionGrant")
        .unwrap();
    let workspace = f
        .engine
        .record(text(&current, "workspaceId"), "Workspace")
        .unwrap();
    let runtime = f.engine.record(&f.runtime, "Runtime").unwrap();
    let owner = f
        .engine
        .record(text(&coordinator, "id"), "Invocation")
        .unwrap();
    for (original, field, invalid) in [
        (&grant, "status", json!("Revoked")),
        (&grant, "policyRevision", json!(999)),
        (&grant, "allowedCapabilities", json!([])),
        (&workspace, "manualHold", json!(true)),
        (&workspace, "writer", json!("Human")),
        (&workspace, "status", json!("Provisioning")),
        (&runtime, "status", json!("Disconnected")),
        (&owner, "state", json!("Stopping")),
    ] {
        let mut changed = original.clone();
        changed[field] = invalid;
        f.engine.put(changed);
        assert_eq!(
            f.engine.work_continuation(&current)["canRestartSession"],
            false,
            "{field}"
        );
        f.engine.put(original.clone());
    }
    assert_eq!(
        f.engine.work_continuation(&current)["canRestartSession"],
        true
    );
    let response = f.command(
        Principal::Human,
        "work.continue",
        json!({"workId":work["id"],"restartSession":true}),
        vec![current],
    );
    assert_eq!(response.status, "ok", "{response:?}");
    let invocation = actor(&mut f, &work);
    assert!(invocation.get("sessionReuseRef").is_none());
    assert!(invocation["executorInput"].get("sessionSource").is_none());
    assert_eq!(
        f.engine.record(text(&work, "id"), "Work").unwrap()["executorClaim"]["restartSession"],
        true
    );
}

#[test]
fn typed_questions_and_answers_stay_in_the_same_executor_chat() {
    let mut f = Fixture::new();
    let work = draft(&mut f);
    f.start(&work);
    let invocation = actor(&mut f, &work);
    f.start_invocation(&invocation);
    let principal = Principal::Invocation {
        invocation_id: text(&invocation, "id").into(),
    };
    let response=f.command(principal.clone(),"work.request_input",json!({"workId":work["id"],
        "question":"Which port?","responseSchema":{"type":"integer","minimum":1024,"maximum":65535}}),vec![]);
    assert_eq!(response.status, "needs_input", "{response:?}");
    let question = f
        .engine
        .related("IntakeRequest", "workId", text(&work, "id"))
        .pop()
        .unwrap();
    assert!(f
        .engine
        .executor_binding(&principal, text(&work, "id"))
        .is_err());
    finish(&mut f, &invocation);
    let view = opened(&mut f, &work);
    assert_eq!(view["continuation"]["state"], "WaitingForInput");
    let response = f.command(
        Principal::Human,
        "conversation.answer_input",
        json!({"requestId":question["id"],"action":"Answer","value":8080}),
        vec![question],
    );
    assert_eq!(response.status, "ok", "{response:?}");
    assert_eq!(actor(&mut f, &work)["id"], invocation["id"]);
    let messages = f.engine.related(
        "ConversationItem",
        "conversationId",
        text(&view["conversation"], "id"),
    );
    assert!(messages
        .iter()
        .any(|message| message["role"] == "human" && message["text"] == "8080"));
    assert!(f
        .engine
        .related("CoordinationTurn", "workId", text(&work, "id"))
        .is_empty());
}

#[test]
fn queued_inputs_cannot_bypass_the_approved_execution_allowance() {
    let mut f = Fixture::new();
    let mut project = f.engine.record(&f.project, "Project").unwrap();
    project["limits"]["executionAttempts"] = json!(1);
    f.engine.put(project);
    let work = draft(&mut f);
    f.start(&work);
    let invocation = actor(&mut f, &work);
    f.start_invocation(&invocation);
    let view = opened(&mut f, &work);
    assert_eq!(submit(&mut f, &view, "Another execution").status, "ok");
    finish(&mut f, &invocation);
    assert_eq!(actor(&mut f, &work)["state"], "Idle");
    let current = f.engine.record(text(&work, "id"), "Work").unwrap();
    assert_eq!(current["usage"]["executionAttempts"], 1);
    assert_eq!(
        f.engine.work_continuation(&current)["state"],
        "NeedsRecovery"
    );
    assert_eq!(f.engine.work_continuation(&current)["pendingInputCount"], 1);
    let response = f.command(
        Principal::Human,
        "work.continue",
        json!({"workId":work["id"]}),
        vec![current],
    );
    assert_eq!(response.status, "ok", "{response:?}");
    assert_eq!(
        f.engine
            .related("Invocation", "workId", text(&work, "id"))
            .len(),
        1
    );
}

#[test]
fn expired_executor_continuation_waits_for_release_then_loads_the_same_session() {
    let mut f = Fixture::new();
    let work = draft(&mut f);
    f.start(&work);
    let invocation = actor(&mut f, &work);
    f.start_invocation(&invocation);
    let mut expired = f
        .engine
        .record(text(&invocation, "id"), "Invocation")
        .unwrap();
    expired["limits"]["deadlineUtc"] = json!("2000-01-01T00:00:00Z");
    f.engine.put(expired);
    let current = f.engine.record(text(&work, "id"), "Work").unwrap();
    let mut request = Request::new("work.continue", json!({"workId":work["id"]}));
    request.command_id = Some(id());
    request.if_match = vec![Engine::reference(&current)];
    assert_eq!(
        f.engine.handle(&Principal::Human, request.clone()).status,
        "ok"
    );
    assert_eq!(f.engine.handle(&Principal::Human, request).status, "ok");
    assert_eq!(
        f.engine
            .related("Invocation", "workId", text(&work, "id"))
            .len(),
        1
    );
    assert_eq!(actor(&mut f, &work)["state"], "Stopping");
    f.finish_release(&invocation);
    let resumed = actor(&mut f, &work);
    assert_ne!(resumed["id"], invocation["id"]);
    assert_eq!(resumed["sessionReuseRef"], invocation["id"]);
    f.start_invocation(&resumed);
    let current = f.engine.record(text(&work, "id"), "Work").unwrap();
    assert_eq!(
        current["primarySession"]["providerSessionId"],
        format!("session-{}", text(&invocation, "id"))
    );
}

#[test]
fn intervening_hold_or_cancel_revokes_pending_claim_even_with_reconstruction_consent() {
    for restart in [false, true] {
        for action in ["Hold", "Cancel"] {
            let mut f = Fixture::new();
            let work = f.draft("Pending claim control race");
            f.start(&work);
            let coordinator = f.coordinator(&work);
            let worker = f.plan(&work, &coordinator, false);
            f.start_invocation(&worker);
            let current = f.engine.record(text(&work, "id"), "Work").unwrap();
            let mut claim = Request::new(
                "work.claim_executor",
                json!({"workId":work["id"],"restartSession":restart}),
            );
            claim.command_id = Some(id());
            claim.if_match = vec![Engine::reference(&current)];
            assert_eq!(
                f.engine.handle(&Principal::Human, claim.clone()).status,
                "ok"
            );
            let settling = f.engine.record(text(&work, "id"), "Work").unwrap();
            let summary = f.engine.work_continuation(&settling);
            assert_eq!(summary["state"], "NeedsRecovery");
            assert_eq!(summary["canClaimExecutor"], false);
            assert_eq!(summary["canRestartSession"], false);
            let repeated = f.command(
                Principal::Human,
                "work.claim_executor",
                json!({"workId":work["id"],"restartSession":!restart}),
                vec![settling.clone()],
            );
            assert_eq!(repeated.failure.unwrap().code, "BAD_STATE");
            let reconciled = f.command(
                Principal::Human,
                "work.continue",
                json!({"workId":work["id"],"restartSession":!restart}),
                vec![settling],
            );
            assert_eq!(reconciled.status, "ok", "{reconciled:?}");
            let current = f.engine.record(text(&work, "id"), "Work").unwrap();
            assert_eq!(current["executorClaim"]["restartSession"], restart);
            let stopped = f.command(
                Principal::Human,
                "work.control",
                json!({"workId":work["id"],"action":action}),
                vec![current],
            );
            assert_eq!(stopped.status, "ok", "{stopped:?}");
            assert_eq!(f.engine.handle(&Principal::Human, claim).status, "ok");
            f.finish_release(&worker);
            f.engine.drive().unwrap();
            let current = f.engine.record(text(&work, "id"), "Work").unwrap();
            assert_eq!(current["executionMode"], "ClaimingExecutor");
            assert_eq!(current["desiredAdvancement"], action);
            assert_eq!(current["executorClaim"]["restartSession"], false);
            assert_eq!(
                current["executorClaim"]["status"],
                if action == "Hold" {
                    "Held"
                } else {
                    "Cancelled"
                }
            );
            assert!(f
                .engine
                .related("WorkExecutionTurn", "workId", text(&work, "id"))
                .is_empty());
            assert!(f
                .engine
                .related("Invocation", "workId", text(&work, "id"))
                .iter()
                .all(|invocation| invocation["subject"]["kind"] != "Work"));
            if action == "Hold" {
                assert_eq!(f.engine.work_continuation(&current)["state"], "Paused");
                assert_eq!(
                    f.engine.work_continuation(&current)["canClaimExecutor"],
                    true
                );
                let resumed = f.command(
                    Principal::Human,
                    "work.control",
                    json!({"workId":work["id"],"action":"Resume"}),
                    vec![current],
                );
                assert_eq!(resumed.status, "ok");
                let current = f.engine.record(text(&work, "id"), "Work").unwrap();
                assert_eq!(current["executorClaim"]["status"], "Held");
                assert!(f
                    .engine
                    .related("WorkExecutionTurn", "workId", text(&work, "id"))
                    .is_empty());
                let continued = f.command(
                    Principal::Human,
                    "work.continue",
                    json!({"workId":work["id"]}),
                    vec![current],
                );
                assert_eq!(continued.status, "ok", "{continued:?}");
                assert_eq!(actor(&mut f, &work)["sessionReuseRef"], worker["id"]);
            } else {
                assert_eq!(
                    f.engine.work_continuation(&current)["canClaimExecutor"],
                    false
                );
                assert_eq!(
                    f.engine.work_continuation(&current)["canRestartSession"],
                    false
                );
                let continued = f.command(
                    Principal::Human,
                    "work.continue",
                    json!({"workId":work["id"],"restartSession":true}),
                    vec![current],
                );
                assert_ne!(continued.status, "ok");
                assert!(f
                    .engine
                    .related("WorkExecutionTurn", "workId", text(&work, "id"))
                    .is_empty());
            }
        }
    }
}

#[test]
fn hold_or_cancel_after_explicit_reconstruction_fences_delayed_start() {
    for action in ["Hold", "Cancel"] {
        let mut f = Fixture::new();
        let work = f.draft("Reconstruction delayed dispatch");
        f.start(&work);
        let coordinator = f.coordinator(&work);
        let current = f.engine.record(text(&work, "id"), "Work").unwrap();
        assert_eq!(
            f.command(
                Principal::Human,
                "work.claim_executor",
                json!({"workId":work["id"]}),
                vec![current]
            )
            .status,
            "ok"
        );
        f.finish_release(&coordinator);
        let current = f.engine.record(text(&work, "id"), "Work").unwrap();
        assert_eq!(current["executorClaim"]["status"], "NeedsConsent");
        assert_eq!(
            f.engine.work_continuation(&current)["canRestartSession"],
            true
        );
        assert_eq!(
            f.command(
                Principal::Human,
                "work.continue",
                json!({"workId":work["id"],"restartSession":true}),
                vec![current]
            )
            .status,
            "ok"
        );
        let invocation = actor(&mut f, &work);
        let current = f.engine.record(text(&work, "id"), "Work").unwrap();
        assert_eq!(
            f.command(
                Principal::Human,
                "work.control",
                json!({"workId":work["id"],"action":action}),
                vec![current]
            )
            .status,
            "ok"
        );
        f.start_invocation(&invocation);
        let principal = Principal::Invocation {
            invocation_id: text(&invocation, "id").into(),
        };
        assert!(f
            .engine
            .executor_binding(&principal, text(&work, "id"))
            .is_err());
        f.finish_release(&invocation);
        f.engine.drive().unwrap();
        let current = f.engine.record(text(&work, "id"), "Work").unwrap();
        assert_eq!(current["desiredAdvancement"], action);
        assert_eq!(
            f.engine
                .related("Invocation", "workId", text(&work, "id"))
                .iter()
                .filter(|invocation| invocation["subject"]["kind"] == "Work")
                .count(),
            1
        );
        assert!(f
            .engine
            .related("Invocation", "workId", text(&work, "id"))
            .iter()
            .all(|invocation| invocation["state"] == "Released"));
    }
}
