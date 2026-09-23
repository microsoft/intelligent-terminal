// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

use super::*;

fn open(f: &mut Fixture, work: &Value) -> Value {
    let response = f.command(
        Principal::Human,
        "work.open",
        json!({"workId":work["id"]}),
        vec![],
    );
    assert_eq!(response.status, "ok", "{response:?}");
    response.data.unwrap()
}

fn continue_work(f: &mut Fixture, work: &Value, restart: bool) -> Response {
    let current = f.engine.record(text(work, "id"), "Work").unwrap();
    f.command(
        Principal::Human,
        "work.continue",
        json!({"workId":work["id"],"restartSession":restart}),
        vec![current],
    )
}

fn finish_coordinator(f: &mut Fixture, work: &Value) {
    let coordinator = f
        .engine
        .related("Invocation", "workId", text(work, "id"))
        .into_iter()
        .find(|invocation| {
            invocation["subject"]["kind"] == "Coordination" && invocation["state"] == "Running"
        })
        .unwrap();
    f.report(
        &coordinator,
        "TextDelta",
        json!({"messageId":coordinator["replyMessageId"],
        "partId":"turn-1","chunkIndex":0,"text":"Recorded work history"}),
    );
    let response = f.command(Principal::Invocation { invocation_id:text(&coordinator,"id").into() },
        "coordination.finish", json!({"turnId":coordinator["subject"]["id"],"outcome":"Answered","messageId":coordinator["replyMessageId"],
            "commandIds":[],"operationIds":[],"explanation":"Awaiting further human input"}), vec![]);
    assert_eq!(response.status, "ok", "{response:?}");
    f.finish_release(&coordinator);
}

fn submit(f: &mut Fixture, opened: &Value, message: &str) -> Response {
    let mut context = opened["context"].clone();
    context.as_object_mut().unwrap().remove("conversationId");
    f.command(
        Principal::Human,
        "conversation.submit",
        json!({
            "conversationId":opened["conversation"]["id"],"clientMessageId":id(),"text":message,
            "attachments":[],"declaredIntent":"WorkDiscussion","context":context
        }),
        vec![],
    )
}

#[test]
fn disconnected_coordinator_recovers_the_original_session_without_releasing_a_writer() {
    let mut f = Fixture::new();
    let work = f.draft("Recover the original conversation after service restart");
    f.start(&work);
    let original = f.coordinator(&work);
    let mut disconnected = f.engine.record(&f.runtime, "Runtime").unwrap();
    let mut replacement = disconnected.clone();
    disconnected["status"] = json!("Disconnected");
    f.engine.put(disconnected);
    replacement["id"] = json!(id());
    f.engine.put(replacement);
    assert_eq!(continue_work(&mut f, &work, false).status, "ok");
    let recovery = f
        .engine
        .all("Operation")
        .into_iter()
        .find(|operation| operation["effect"]["method"] == "runtime.recover_coordinator")
        .unwrap();
    assert_eq!(continue_work(&mut f, &work, false).status, "ok");
    assert_eq!(
        f.engine
            .all("Operation")
            .iter()
            .filter(|operation| { operation["effect"]["method"] == "runtime.recover_coordinator" })
            .count(),
        1
    );
    f.engine.complete_effect(text(&recovery, "id"), Response::ok(id(), json!({
        "workId":work["id"],"capabilityId":original["capabilityId"],
        "providerSessionId":original["providerSessionId"],
        "providerConfigurationDigest":"fixture-provider-configuration","cwd":f.root.to_string_lossy()
    }))).unwrap();
    let retired = f
        .engine
        .record(text(&original, "id"), "Invocation")
        .unwrap();
    assert_eq!(retired["state"], "Released");
    assert_eq!(retired["releaseKind"], "CoordinatorAuthorityRevoked");
    assert_eq!(retired["processSettlement"], "Unknown");
    assert_ne!(retired["lastTurnEnd"]["quiescent"], true);
    let next = f
        .engine
        .all("Invocation")
        .into_iter()
        .find(|entry| entry["state"] == "Dispatching")
        .unwrap();
    assert_eq!(next["sessionReuseRef"], original["id"]);
    let old_report = f.command(
        Principal::Runtime {runtime_id:f.runtime.clone()}, "runtime.report",
        json!({"observationId":id(),"invocationId":original["id"],"bindingGeneration":1,
            "sequence":number(&original,"lastSequence")+1,"kind":"TextDelta",
            "data":{"messageId":original["replyMessageId"],"partId":"stale","chunkIndex":0,"text":"Stale reply"}}), vec![]);
    assert_eq!(old_report.status, "conflict");
    assert_eq!(old_report.failure.unwrap().code, "STALE_DISPATCH");
    let old_tool = f.command(Principal::Invocation {invocation_id:text(&original,"id").into()},
        "coordination.finish", json!({"turnId":original["subject"]["id"],"outcome":"Answered",
        "messageId":original["replyMessageId"],"commandIds":[],"operationIds":[],"explanation":"Stale tool"}), vec![]);
    assert_eq!(old_tool.status, "error");
    assert_eq!(f.engine.all("Work").len(), 1);
}

#[test]
fn cancelling_or_holding_during_coordinator_recovery_cannot_restart_work() {
    for action in ["Hold", "Cancel"] {
        let mut f = Fixture::new();
        let work = f.draft("A later human control wins over session recovery");
        f.start(&work);
        let original = f.coordinator(&work);
        let mut runtime = f.engine.record(&f.runtime, "Runtime").unwrap();
        runtime["status"] = json!("Disconnected");
        f.engine.put(runtime);
        assert_eq!(continue_work(&mut f, &work, false).status, "ok");
        let operation = f
            .engine
            .all("Operation")
            .into_iter()
            .find(|entry| entry["effect"]["method"] == "runtime.recover_coordinator")
            .unwrap();
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
        f.engine.complete_effect(text(&operation,"id"),Response::ok(id(),json!({
            "workId":work["id"],"capabilityId":original["capabilityId"],
            "providerSessionId":original["providerSessionId"],"providerConfigurationDigest":"fixture-provider-configuration",
            "cwd":f.root.to_string_lossy()
        }))).unwrap();
        let current = f.engine.record(text(&work, "id"), "Work").unwrap();
        assert_eq!(current["desiredAdvancement"], action);
        assert!(current.get("continuationRecovery").is_none());
        assert_eq!(f.engine.all("Invocation").len(), 1);
    }
}

#[test]
fn coordinator_recovery_cannot_bypass_an_unsettled_worker() {
    let mut f = Fixture::new();
    let work = f.draft("Keep real writer settlement mandatory");
    f.start(&work);
    let coordinator = f.coordinator(&work);
    let mut runtime = f.engine.record(&f.runtime, "Runtime").unwrap();
    runtime["status"] = json!("Disconnected");
    f.engine.put(runtime);
    let worker = f.engine.create(
        "Invocation",
        json!({
            "workId":work["id"],"runtimeId":coordinator["runtimeId"],
            "subject":{"kind":"Task","id":id()},"state":"Running",
            "bindingGeneration":1,"lastSequence":0,"executionIdentity":"worker",
            "limits":{"deadlineUtc":"2000-01-01T00:00:00Z"}
        }),
    );
    assert_eq!(continue_work(&mut f, &work, false).status, "ok");
    assert!(!f
        .engine
        .all("Operation")
        .iter()
        .any(|operation| { operation["effect"]["method"] == "runtime.recover_coordinator" }));
    assert_ne!(
        f.engine.record(text(&worker, "id"), "Invocation").unwrap()["state"],
        "Released"
    );
}

#[test]
fn response_activity_requires_a_live_primary_invocation_not_historical_streaming() {
    let mut f = Fixture::new();
    let work = f.draft("Do not mistake an abandoned reply for live thinking");
    f.start(&work);
    let coordinator = f.coordinator(&work);
    let opened = open(&mut f, &work);
    assert_eq!(
        opened["continuation"]["activeResponses"],
        json!([{
            "messageId":coordinator["replyMessageId"],
            "deadlineUtc":coordinator["limits"]["deadlineUtc"]
        }])
    );
    let message = f
        .engine
        .record(text(&coordinator, "replyMessageId"), "ConversationItem")
        .unwrap();
    assert_eq!(message["status"], "Streaming");
    for state in ["Idle", "Stopping", "Releasing", "Released"] {
        let mut inactive = coordinator.clone();
        inactive["state"] = json!(state);
        f.engine.put(inactive);
        assert_eq!(
            open(&mut f, &work)["continuation"]["activeResponses"],
            json!([])
        );
    }
    let mut expired = coordinator.clone();
    expired["limits"]["deadlineUtc"] = json!("2000-01-01T00:00:00Z");
    f.engine.put(expired);
    let reopened = open(&mut f, &work);
    assert_eq!(reopened["continuation"]["state"], "NeedsRecovery");
    assert_eq!(reopened["continuation"]["activeResponses"], json!([]));
    assert_eq!(
        f.engine
            .record(text(&message, "id"), "ConversationItem")
            .unwrap(),
        message
    );
    f.engine.put(coordinator);
    let mut runtime = f.engine.record(&f.runtime, "Runtime").unwrap();
    runtime["status"] = json!("Disconnected");
    f.engine.put(runtime);
    assert_eq!(
        open(&mut f, &work)["continuation"]["activeResponses"],
        json!([])
    );
}

#[test]
fn opening_is_a_durable_idempotent_view_without_execution_or_work_mutation() {
    let mut f = Fixture::new();
    let work = f.draft("Inspect history without creating another task");
    let mut request = Request::new("work.open", json!({"workId":work["id"]}));
    request.command_id = Some(id());
    let first = f.engine.handle(&Principal::Human, request.clone());
    assert_eq!(first.status, "ok");
    let first = first.data.unwrap();
    let repeated = f.engine.handle(&Principal::Human, request).data.unwrap();
    assert_eq!(first, repeated);
    let another_tab = open(&mut f, &work);
    assert_eq!(first["context"], another_tab["context"]);
    assert_eq!(first["workView"]["work"], work);
    assert_eq!(first["continuation"]["state"], "Ready");
    assert!(f.engine.take_effects().unwrap().is_empty());
    assert!(f.engine.all("Invocation").is_empty());
    assert_eq!(f.engine.all("Work").len(), 1);
    assert_eq!(f.engine.all("Conversation").len(), 1);
    assert_eq!(f.engine.all("ConsoleSession").len(), 1);
    let replacement = f.root.join("restart-owner");
    drop(std::mem::replace(
        &mut f.engine,
        Engine::open(&replacement).unwrap(),
    ));
    f.engine = Engine::open(&f.root).unwrap();
    let restarted = open(&mut f, &work);
    assert_eq!(first["context"], restarted["context"]);
    assert_eq!(f.engine.record(text(&work, "id"), "Work").unwrap(), work);
    assert!(f.engine.take_effects().unwrap().is_empty());
}

#[test]
fn opening_never_drives_preexisting_queued_work() {
    let mut f = Fixture::new();
    let work = f.draft("Opening a historical item is not execution approval");
    f.start(&work);
    let coordinator = f.coordinator(&work);
    finish_coordinator(&mut f, &work);
    let work = f.engine.record(text(&work, "id"), "Work").unwrap();
    f.engine
        .queue_coordination(Some(text(&work, "id")), None, "ExistingTrigger", &work)
        .unwrap();
    let before = f.engine.all("Invocation");
    let operations = f.engine.all("Operation");
    let opened = open(&mut f, &work);
    assert_eq!(f.engine.all("Invocation"), before);
    assert_eq!(f.engine.all("Operation"), operations);
    assert_eq!(opened["workView"]["work"], work);
    assert_eq!(
        f.engine
            .record(text(&coordinator, "id"), "Invocation")
            .unwrap()["state"],
        "Released"
    );
}

#[test]
fn historical_main_chat_is_retained_without_worker_internal_output() {
    let mut f = Fixture::new();
    let mut work = f.draft("Retain the historical task conversation");
    let global_conversation = id();
    let source_reply = f.engine.create(
        "ConversationItem",
        json!({
            "conversationId":global_conversation,"role":"assistant","text":"The proposed work"
        }),
    );
    let source_turn = f.engine.create(
        "CoordinationTurn",
        json!({"replyMessageId":source_reply["id"]}),
    );
    let source = f.engine.create("ConversationItem", json!({
        "conversationId":global_conversation,"role":"human","text":"The original goal","intakeTurnId":source_turn["id"]
    }));
    work["spec"]["sourceMessageIds"] = json!([source["id"]]);
    let work = f.engine.put(work);
    let worker = f.engine.create(
        "Invocation",
        json!({"workId":work["id"],"subject":{"kind":"Task"},"state":"Released"}),
    );
    let coordinator = f.engine.create(
        "Invocation",
        json!({"workId":work["id"],"subject":{"kind":"Coordination"},"state":"Released"}),
    );
    let internal = f.engine.create("ConversationItem", json!({
        "workId":work["id"],"invocationId":worker["id"],"role":"assistant","text":"Worker execution details"
    }));
    let main = f.engine.create("ConversationItem", json!({
        "workId":work["id"],"invocationId":coordinator["id"],"role":"assistant","text":"The work coordinator's answer"
    }));
    let opened = open(&mut f, &work);
    let messages = opened["conversation"]["messages"].as_array().unwrap();
    for expected in [&source, &source_reply, &main] {
        assert!(messages
            .iter()
            .any(|message| message["id"] == expected["id"]));
    }
    assert!(!messages
        .iter()
        .any(|message| message["id"] == internal["id"]));
    assert_eq!(
        f.engine
            .record(text(&source, "id"), "ConversationItem")
            .unwrap()["conversationId"],
        global_conversation
    );
}

#[test]
fn work_chat_routes_messages_and_questions_to_one_primary_session() {
    let mut f = Fixture::new();
    let work = f.draft("A work owns one main conversation");
    f.start(&work);
    let coordinator = f.coordinator(&work);
    let opened = open(&mut f, &work);
    let submitted = submit(&mut f, &opened, "Please explain this work");
    assert_eq!(submitted.status, "ok", "{submitted:?}");
    let message_id = submitted.data.unwrap()["messageId"].clone();
    assert_eq!(f.engine.all("Work").len(), 1);
    let triggers = f
        .engine
        .related("CoordinationTrigger", "scopeId", text(&work, "id"));
    assert!(triggers
        .iter()
        .any(|trigger| trigger["reason"] == "WorkMessage" && trigger["subjectId"] == message_id));
    assert!(f
        .engine
        .related(
            "CoordinationTrigger",
            "scopeId",
            text(&opened["conversation"], "id")
        )
        .is_empty());
    f.report(
        &coordinator,
        "TextDelta",
        json!({"messageId":coordinator["replyMessageId"],
        "partId":"turn-1","chunkIndex":0,"text":"This is the work's answer"}),
    );
    let conversation = f
        .engine
        .record(text(&opened["conversation"], "id"), "Conversation")
        .unwrap();
    assert!(values(&conversation, "messages")
        .iter()
        .any(|message| message["parts"][0]["text"] == "This is the work's answer"));
    let question = f.command(
        Principal::Invocation {
            invocation_id: text(&coordinator, "id").into(),
        },
        "conversation.request_input",
        json!({"turnId":coordinator["subject"]["id"],
            "conversationId":opened["conversation"]["id"],"messageId":message_id,
            "question":"Which report?","responseSchema":{"type":"string"}}),
        vec![],
    );
    assert_eq!(question.status, "needs_input", "{question:?}");
    let question = f.engine.all("IntakeRequest").pop().unwrap();
    assert_eq!(question["workId"], work["id"]);
    assert_eq!(
        open(&mut f, &work)["continuation"]["state"],
        "WaitingForInput"
    );
    let answer = f.command(
        Principal::Human,
        "conversation.answer_input",
        json!({"requestId":question["id"],"action":"Answer","value":"The summary"}),
        vec![question],
    );
    assert_eq!(answer.status, "ok", "{answer:?}");
    f.finish_release(&coordinator);
    let next = f
        .engine
        .related("Invocation", "workId", text(&work, "id"))
        .into_iter()
        .find(|invocation| invocation["state"] == "Dispatching")
        .unwrap();
    assert_eq!(next["sessionReuseRef"], coordinator["id"]);
    assert_eq!(
        next["coordinationInput"]["scope"]["conversationId"],
        opened["conversation"]["id"]
    );
    assert!(
        next["coordinationInput"]["snapshot"]["conversation"]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["id"] == message_id)
    );
    f.start_invocation(&next);
    let latest = f.engine.record(text(&work, "id"), "Work").unwrap();
    assert_eq!(
        latest["primarySession"]["providerSessionId"],
        coordinator["providerSessionId"]
    );
}

#[test]
fn scoped_chat_rejects_cross_work_context_and_terminal_input() {
    let mut f = Fixture::new();
    let work = f.draft("This conversation cannot be retargeted");
    let other = f.draft("Another work");
    let mut opened = open(&mut f, &work);
    opened["context"]["selectedWorkId"] = other["id"].clone();
    let response = submit(&mut f, &opened, "Wrong work");
    assert_eq!(response.failure.unwrap().code, "INVALID_REFERENCE");
    assert!(f.engine.all("ConversationItem").is_empty());
    let mut terminal = work.clone();
    terminal["lifecycle"] = json!("Completed");
    f.engine.put(terminal);
    let opened = open(&mut f, &work);
    assert_eq!(opened["continuation"]["state"], "Completed");
    assert_eq!(
        submit(&mut f, &opened, "Do not revive")
            .failure
            .unwrap()
            .code,
        "BAD_STATE"
    );
    assert_eq!(
        continue_work(&mut f, &work, true).failure.unwrap().code,
        "BAD_STATE"
    );
    assert!(f.engine.all("Invocation").is_empty());
}

#[test]
fn continue_attaches_live_execution_and_repeated_clients_do_not_create_writers() {
    let mut f = Fixture::new();
    let work = f.draft("Attach the live execution");
    f.start(&work);
    f.coordinator(&work);
    let before = f.engine.all("Invocation");
    let current = f.engine.record(text(&work, "id"), "Work").unwrap();
    for _ in 0..3 {
        let response = continue_work(&mut f, &work, false);
        assert_eq!(response.status, "ok", "{response:?}");
        assert_eq!(response.data.unwrap()["continuation"]["state"], "Running");
    }
    assert_eq!(f.engine.all("Invocation"), before);
    assert_eq!(f.engine.record(text(&work, "id"), "Work").unwrap(), current);
    assert_eq!(f.engine.all("Work").len(), 1);
}

#[test]
fn expired_execution_requires_exact_stop_and_release_before_session_load() {
    let mut f = Fixture::new();
    let work = f.draft("Reconcile an expired running provider");
    f.start(&work);
    let mut coordinator = f.coordinator(&work);
    coordinator["limits"]["deadlineUtc"] = json!("2000-01-01T00:00:00Z");
    let coordinator = f.engine.put(coordinator);
    assert_eq!(
        open(&mut f, &work)["continuation"]["state"],
        "NeedsRecovery"
    );
    let response = continue_work(&mut f, &work, false);
    assert_eq!(response.status, "ok", "{response:?}");
    assert_eq!(
        response.data.unwrap()["continuation"]["state"],
        "NeedsRecovery"
    );
    let stopping = f
        .engine
        .record(text(&coordinator, "id"), "Invocation")
        .unwrap();
    assert_eq!(stopping["state"], "Stopping");
    assert_eq!(continue_work(&mut f, &work, false).status, "ok");
    assert_eq!(f.engine.all("Invocation").len(), 1);
    assert_eq!(
        f.engine
            .all("Operation")
            .iter()
            .filter(|operation| operation["effect"]["method"] == "runtime.stop")
            .count(),
        1
    );
    f.report(&coordinator, "Settled", json!({"quiescent":true,
        "executionIdentity":coordinator["executionIdentity"],"completedOperationIds":[stopping["stopOperationId"]]}));
    assert_eq!(
        f.engine.all("Invocation").len(),
        1,
        "settlement alone does not release the writer binding"
    );
    let releasing = f
        .engine
        .record(text(&coordinator, "id"), "Invocation")
        .unwrap();
    f.engine
        .complete_effect(
            text(&releasing, "releaseOperationId"),
            Response::ok(id(), json!({"released":true})),
        )
        .unwrap();
    let next = f
        .engine
        .all("Invocation")
        .into_iter()
        .find(|invocation| invocation["state"] == "Dispatching")
        .unwrap();
    assert_eq!(next["sessionReuseRef"], coordinator["id"]);
    assert!(f
        .engine
        .record(text(&work, "id"), "Work")
        .unwrap()
        .get("continuationRecovery")
        .is_none());
    f.start_invocation(&next);
    assert_eq!(open(&mut f, &work)["continuation"]["state"], "Running");
    assert_eq!(f.engine.all("Work").len(), 1);
}

#[test]
fn unverifiable_history_requires_explicit_reconstruction_and_never_silent_fallback() {
    let mut f = Fixture::new();
    let work = f.draft("Old sessions must not silently become new sessions");
    f.start(&work);
    f.coordinator(&work);
    finish_coordinator(&mut f, &work);
    let mut current = f.engine.record(text(&work, "id"), "Work").unwrap();
    current.as_object_mut().unwrap().remove("primarySession");
    f.engine.put(current);
    assert_eq!(
        open(&mut f, &work)["continuation"]["canRestartSession"],
        true
    );
    assert_eq!(
        continue_work(&mut f, &work, false).failure.unwrap().code,
        "SESSION_RESUME_UNAVAILABLE"
    );
    assert_eq!(f.engine.all("Invocation").len(), 1);
    let response = continue_work(&mut f, &work, true);
    assert_eq!(response.status, "ok", "{response:?}");
    let next = f
        .engine
        .all("Invocation")
        .into_iter()
        .find(|invocation| invocation["state"] == "Dispatching")
        .unwrap();
    assert!(next.get("sessionReuseRef").is_none());
    assert_eq!(f.engine.all("Work").len(), 1);
}

#[test]
fn continue_preserves_approval_manual_ownership_and_exact_versions() {
    let mut f = Fixture::new();
    let work = f.draft("Do not bypass approvals or manual writers");
    assert_eq!(
        continue_work(&mut f, &work, false).failure.unwrap().code,
        "BAD_STATE"
    );
    f.start(&work);
    f.coordinator(&work);
    finish_coordinator(&mut f, &work);
    let current = f.engine.record(text(&work, "id"), "Work").unwrap();
    let stale = f.command(
        Principal::Human,
        "work.continue",
        json!({"workId":work["id"]}),
        vec![work.clone()],
    );
    assert_eq!(stale.failure.unwrap().code, "STALE_VERSION");
    let mut workspace = f
        .engine
        .record(text(&current, "workspaceId"), "Workspace")
        .unwrap();
    workspace["manualHold"] = json!(true);
    f.engine.put(workspace);
    assert_eq!(
        continue_work(&mut f, &work, true).failure.unwrap().code,
        "BAD_STATE"
    );
    assert_eq!(f.engine.all("Invocation").len(), 1);
}

#[test]
fn paused_work_resumes_its_saved_primary_and_not_a_new_session() {
    let mut f = Fixture::new();
    let work = f.draft("Resume the approved paused work");
    f.start(&work);
    let coordinator = f.coordinator(&work);
    finish_coordinator(&mut f, &work);
    let current = f.engine.record(text(&work, "id"), "Work").unwrap();
    let held = f.command(
        Principal::Human,
        "work.control",
        json!({"workId":work["id"],"action":"Hold"}),
        vec![current],
    );
    assert_eq!(held.status, "ok", "{held:?}");
    assert_eq!(open(&mut f, &work)["continuation"]["state"], "Paused");
    let response = continue_work(&mut f, &work, false);
    assert_eq!(response.status, "ok", "{response:?}");
    let next = f
        .engine
        .all("Invocation")
        .into_iter()
        .find(|invocation| invocation["state"] == "Dispatching")
        .unwrap();
    assert_eq!(next["sessionReuseRef"], coordinator["id"]);
}

#[test]
fn cancelling_during_recovery_cannot_reactivate_terminal_work() {
    let mut f = Fixture::new();
    let work = f.draft("Cancellation overrides a pending continuation");
    f.start(&work);
    let mut coordinator = f.coordinator(&work);
    coordinator["limits"]["deadlineUtc"] = json!("2000-01-01T00:00:00Z");
    f.engine.put(coordinator.clone());
    assert_eq!(continue_work(&mut f, &work, true).status, "ok");
    let current = f.engine.record(text(&work, "id"), "Work").unwrap();
    let response = f.command(
        Principal::Human,
        "work.control",
        json!({"workId":work["id"],"action":"Cancel"}),
        vec![current],
    );
    assert_eq!(response.status, "ok", "{response:?}");
    let stopping = f
        .engine
        .record(text(&coordinator, "id"), "Invocation")
        .unwrap();
    f.report(&coordinator, "Settled", json!({"quiescent":true,
        "executionIdentity":coordinator["executionIdentity"],"completedOperationIds":[stopping["stopOperationId"]]}));
    let releasing = f
        .engine
        .record(text(&coordinator, "id"), "Invocation")
        .unwrap();
    f.engine
        .complete_effect(
            text(&releasing, "releaseOperationId"),
            Response::ok(id(), json!({"released":true})),
        )
        .unwrap();
    let current = f.engine.record(text(&work, "id"), "Work").unwrap();
    assert_eq!(current["lifecycle"], "Cancelled");
    assert_eq!(current["desiredAdvancement"], "Cancel");
    assert!(current.get("continuationRecovery").is_none());
    assert_eq!(f.engine.all("Invocation").len(), 1);
    assert_eq!(
        continue_work(&mut f, &work, true).failure.unwrap().code,
        "BAD_STATE"
    );
}

#[test]
fn requested_resume_is_not_accepted_as_a_successful_session_load() {
    let mut f = Fixture::new();
    let work = f.draft("Resume requires actual provider session loading");
    f.start(&work);
    let original = f.coordinator(&work);
    finish_coordinator(&mut f, &work);
    assert_eq!(continue_work(&mut f, &work, false).status, "ok");
    let next = f
        .engine
        .all("Invocation")
        .into_iter()
        .find(|invocation| invocation["state"] == "Dispatching")
        .unwrap();
    let response = f.command(Principal::Runtime { runtime_id:f.runtime.clone() }, "runtime.report",
        json!({"observationId":id(),"invocationId":next["id"],"bindingGeneration":1,"sequence":1,"kind":"Started",
            "data":{"adapterKind":"ACP","executionIdentity":"new-process","providerSessionId":"invented-new-session",
                "providerConfigurationDigest":"fixture-provider-configuration","sessionCwd":f.root.to_string_lossy(),"sessionLoaded":false}}), vec![]);
    assert_eq!(response.failure.unwrap().code, "SESSION_RESUME_UNAVAILABLE");
    assert_eq!(
        f.engine.record(text(&work, "id"), "Work").unwrap()["primarySession"]["invocationId"],
        original["id"]
    );
    f.report(
        &next,
        "TurnEnded",
        json!({"turnNumber":1,"finish":"Error","quiescent":true,
        "errorText":"SESSION_RESUME_UNAVAILABLE: provider cannot load history"}),
    );
    let releasing = f.engine.record(text(&next, "id"), "Invocation").unwrap();
    f.engine
        .complete_effect(
            text(&releasing, "releaseOperationId"),
            Response::ok(id(), json!({"released":true})),
        )
        .unwrap();
    assert_eq!(open(&mut f, &work)["continuation"]["state"], "Unavailable");
}

#[test]
fn reconstruction_is_not_recommended_for_live_or_merely_offline_execution() {
    let mut f = Fixture::new();
    let work = f.draft("Reconstruction is a bounded recovery choice");
    f.start(&work);
    f.coordinator(&work);
    assert_eq!(
        open(&mut f, &work)["continuation"]["canRestartSession"],
        false
    );
    finish_coordinator(&mut f, &work);
    let mut runtime = f.engine.record(&f.runtime, "Runtime").unwrap();
    runtime["status"] = json!("Disconnected");
    f.engine.put(runtime);
    assert_eq!(
        open(&mut f, &work)["continuation"]["canRestartSession"],
        false
    );
    let mut current = f.engine.record(text(&work, "id"), "Work").unwrap();
    current.as_object_mut().unwrap().remove("primarySession");
    f.engine.put(current);
    assert_eq!(
        open(&mut f, &work)["continuation"]["canRestartSession"],
        false
    );
}

#[test]
fn failed_scoped_reconciliation_can_be_retried_without_admitting_another_writer() {
    let mut f = Fixture::new();
    let work = f.draft("Retry the exact invocation's reconciliation");
    f.start(&work);
    let mut coordinator = f.coordinator(&work);
    coordinator["limits"]["deadlineUtc"] = json!("2000-01-01T00:00:00Z");
    f.engine.put(coordinator.clone());
    assert_eq!(continue_work(&mut f, &work, false).status, "ok");
    let first = f
        .engine
        .record(text(&coordinator, "id"), "Invocation")
        .unwrap()["stopOperationId"]
        .clone();
    f.engine
        .complete_effect(
            first.as_str().unwrap(),
            Response::fail(
                id(),
                "OUTCOME_UNKNOWN",
                "Runtime was unavailable; writer settlement is not yet proven",
            ),
        )
        .unwrap();
    let failed = open(&mut f, &work);
    assert_eq!(failed["continuation"]["state"], "NeedsRecovery");
    assert_eq!(failed["continuation"]["activeResponses"], json!([]));
    assert_eq!(failed["continuation"]["canRestartSession"], false);
    assert_eq!(
        failed["continuation"]["recoveryFailure"]["code"],
        "OUTCOME_UNKNOWN"
    );
    assert_eq!(
        failed["continuation"]["recoveryFailure"]["operationId"],
        first
    );
    assert_eq!(
        failed["continuation"]["recoveryFailure"]["message"],
        "Runtime was unavailable; writer settlement is not yet proven"
    );
    assert_eq!(continue_work(&mut f, &work, false).status, "ok");
    assert!(open(&mut f, &work)["continuation"]
        .get("recoveryFailure")
        .is_none());
    let stopping = f
        .engine
        .record(text(&coordinator, "id"), "Invocation")
        .unwrap();
    let second = stopping["stopOperationId"].clone();
    assert_ne!(first, second);
    let operation = f
        .engine
        .record(second.as_str().unwrap(), "Operation")
        .unwrap();
    assert_eq!(
        operation["effect"]["params"]["reconciliation"]["executionIdentity"],
        coordinator["executionIdentity"]
    );
    assert_eq!(
        operation["effect"]["params"]["reconciliation"]["stopOperationIds"],
        json!([first])
    );
    assert_eq!(continue_work(&mut f, &work, false).status, "ok");
    assert_eq!(
        f.engine
            .record(text(&coordinator, "id"), "Invocation")
            .unwrap()["stopOperationId"],
        second
    );
    assert_eq!(f.engine.all("Invocation").len(), 1);
    f.report(
        &coordinator,
        "Settled",
        json!({"quiescent":true,"executionIdentity":coordinator["executionIdentity"],
        "completedOperationIds":[first,second]}),
    );
    let releasing = f
        .engine
        .record(text(&coordinator, "id"), "Invocation")
        .unwrap();
    f.engine
        .complete_effect(
            text(&releasing, "releaseOperationId"),
            Response::ok(id(), json!({"released":true})),
        )
        .unwrap();
    let next = f
        .engine
        .all("Invocation")
        .into_iter()
        .find(|invocation| invocation["state"] == "Dispatching")
        .unwrap();
    assert_eq!(next["sessionReuseRef"], coordinator["id"]);
    assert_eq!(f.engine.all("Work").len(), 1);
}

#[test]
fn interrupted_release_is_reconciled_before_loading_the_primary_session() {
    let mut f = Fixture::new();
    let work = f.draft("A lost release acknowledgment must not create another writer");
    f.start(&work);
    let coordinator = f.coordinator(&work);
    f.report(
        &coordinator,
        "TurnEnded",
        json!({"turnNumber":1,"finish":"Normal","quiescent":true}),
    );
    let original = f
        .engine
        .record(text(&coordinator, "id"), "Invocation")
        .unwrap()["releaseOperationId"]
        .clone();
    f.engine
        .complete_effect(
            original.as_str().unwrap(),
            Response::fail(id(), "OUTCOME_UNKNOWN", "Release acknowledgment was lost"),
        )
        .unwrap();
    assert_eq!(continue_work(&mut f, &work, false).status, "ok");
    assert_eq!(f.engine.all("Invocation").len(), 1);
    let releasing = f
        .engine
        .record(text(&coordinator, "id"), "Invocation")
        .unwrap();
    assert_ne!(releasing["releaseOperationId"], original);
    let operation = f
        .engine
        .record(text(&releasing, "releaseOperationId"), "Operation")
        .unwrap();
    assert_eq!(
        operation["effect"]["params"]["reconcilesOperationIds"],
        json!([original])
    );
    assert_eq!(continue_work(&mut f, &work, false).status, "ok");
    assert_eq!(
        f.engine
            .record(text(&coordinator, "id"), "Invocation")
            .unwrap()["releaseOperationId"],
        operation["id"]
    );
    f.engine
        .complete_effect(
            text(&operation, "id"),
            Response::ok(id(), json!({"released":true})),
        )
        .unwrap();
    let previous = f
        .engine
        .record(original.as_str().unwrap(), "Operation")
        .unwrap();
    assert_eq!(previous["status"], "Succeeded");
    assert_eq!(previous["reconciledByOperationId"], operation["id"]);
    let next = f
        .engine
        .all("Invocation")
        .into_iter()
        .find(|invocation| invocation["state"] == "Dispatching")
        .unwrap();
    assert_eq!(next["sessionReuseRef"], coordinator["id"]);
}

#[test]
fn repeated_recovery_can_probe_a_pending_stop_without_restarting_execution() {
    let mut f = Fixture::new();
    let work = f.draft("An incomplete terminal observation can be reconciled again");
    f.start(&work);
    let mut coordinator = f.coordinator(&work);
    coordinator["limits"]["deadlineUtc"] = json!("2000-01-01T00:00:00Z");
    f.engine.put(coordinator.clone());
    assert_eq!(continue_work(&mut f, &work, false).status, "ok");
    let first = f
        .engine
        .record(text(&coordinator, "id"), "Invocation")
        .unwrap()["stopOperationId"]
        .clone();
    f.engine
        .complete_effect(
            first.as_str().unwrap(),
            Response::pending(
                id(),
                first.as_str().unwrap().into(),
                json!({"invocationId":coordinator["id"]}),
            ),
        )
        .unwrap();
    assert_eq!(continue_work(&mut f, &work, false).status, "ok");
    let second = f
        .engine
        .record(text(&coordinator, "id"), "Invocation")
        .unwrap()["stopOperationId"]
        .clone();
    assert_ne!(first, second);
    assert_eq!(continue_work(&mut f, &work, false).status, "ok");
    assert_eq!(
        f.engine
            .record(text(&coordinator, "id"), "Invocation")
            .unwrap()["stopOperationId"],
        second
    );
    assert_eq!(f.engine.all("Invocation").len(), 1);
    assert_eq!(f.engine.all("Work").len(), 1);
}
