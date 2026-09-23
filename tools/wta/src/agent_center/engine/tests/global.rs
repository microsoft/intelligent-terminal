// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

use super::*;

fn approve(f: &mut Fixture) {
    let response = f.command(Principal::Human, "console.configure", json!({
        "capabilityId":"fixture-agent","workerCapabilityId":"fixture-agent",
        "checkCapabilityId":"native-check","approvedModelDestination":"Local scripted test adapter",
        "limits":{"concurrency":1,"executionAttempts":8,"evaluationAttempts":16,
            "coordinationTurns":64,"contextRounds":3,"executionSeconds":60,"coordinationSeconds":60}
    }), vec![]);
    assert_eq!(response.status, "ok", "{response:?}");
}

fn submit(f: &mut Fixture, console: &str, conversation: &str, project: Option<&str>) -> Response {
    let mut context = json!({"scope":"Global","consoleSessionId":console,"contextVersion":1});
    if let Some(project) = project {
        context["projectId"] = json!(project);
    }
    f.command(Principal::Human, "conversation.submit", json!({
        "conversationId":conversation,"clientMessageId":id(),"text":"Hello, help me understand my work.",
        "attachments":[],"context":context
    }), vec![])
}

#[test]
fn ordinary_global_chat_requires_no_execution_project_or_work() {
    let mut f = Fixture::runtime_only();
    approve(&mut f);
    let conversation = id();
    let console = id();
    let response = submit(&mut f, &console, &conversation, None);
    assert_eq!(response.status, "ok", "{response:?}");
    assert!(f.engine.all("Project").is_empty());
    assert!(f.engine.all("Work").is_empty());
    let registered = f.engine.record(&console, "ConsoleSession").unwrap();
    assert_eq!(registered["scope"], "Global");
    assert!(registered.get("projectId").is_none());
    let invocation = f.engine.all("Invocation").pop().unwrap();
    assert_eq!(invocation["scope"], "Global");
    let before_first_token = f.engine.record(&conversation, "Conversation").unwrap();
    assert_eq!(before_first_token["coordinationUsed"], 1);
    let reply = values(&before_first_token, "messages")
        .into_iter()
        .find(|message| message["id"] == invocation["replyMessageId"])
        .expect("Allocated response must be visible before adapter startup");
    assert_eq!(reply["status"], "Streaming");
    assert_eq!(reply["parts"], json!([]));
    let events = f
        .engine
        .events_after(None, &json!({"kind":"Conversation","id":conversation}))
        .unwrap();
    assert!(events.iter().any(|event| {
        event["kind"] == "MessageRecorded"
            && values(&event["changes"][0]["view"], "messages").contains(&reply)
    }));
    assert!(invocation.get("projectId").is_none());
    assert_eq!(
        invocation["coordinationInput"]["snapshot"]["projects"],
        json!([])
    );
    assert_eq!(
        invocation["coordinationInput"]["snapshot"]["works"],
        json!([])
    );
    assert!(!invocation["availableToolNames"]
        .as_array()
        .unwrap()
        .contains(&json!("work_start")));
    assert!(!invocation["availableToolNames"]
        .as_array()
        .unwrap()
        .contains(&json!("plan_apply")));
    assert!(invocation["availableToolNames"]
        .as_array()
        .unwrap()
        .contains(&json!("operation_get")));
    let wire = Engine::wire_invocation(&invocation).unwrap();
    assert_eq!(wire["coordinationInput"]["snapshot"]["scope"], "Global");
    assert!(!serde_json::to_string(&wire)
        .unwrap()
        .contains("Local scripted test adapter"));
    f.start_invocation(&invocation);
    f.report(
        &invocation,
        "TextDelta",
        json!({
            "messageId":invocation["replyMessageId"],"partId":id(),"chunkIndex":0,
            "text":"Hello. What would you like to achieve?"
        }),
    );
    let stored = f.engine.record(&conversation, "Conversation").unwrap();
    assert!(values(&stored, "messages")
        .iter()
        .any(|message| message["id"] == invocation["replyMessageId"]
            && values(message, "parts")
                .iter()
                .any(|part| part["text"] == "Hello. What would you like to achieve?")));
    let finished = f.command(
        Principal::Invocation {
            invocation_id: text(&invocation, "id").into(),
        },
        "coordination.finish",
        json!({
            "turnId":invocation["subject"]["id"],"outcome":"Answered",
            "commandIds":[],"operationIds":[],"messageId":invocation["replyMessageId"],
            "explanation":"Answered without arranging execution"
        }),
        vec![],
    );
    assert_eq!(finished.status, "ok", "{finished:?}");
    f.finish_release(&invocation);
    let completed = f.engine.record(&conversation, "Conversation").unwrap();
    assert!(values(&completed, "messages").iter().any(|message| {
        message["id"] == invocation["replyMessageId"] && message["status"] == "Complete"
    }));
    assert_eq!(
        f.engine
            .record(text(&invocation, "id"), "Invocation")
            .unwrap()["state"],
        "Released"
    );
    assert_eq!(f.engine.all("Invocation").len(), 1);
    assert!(f.engine.all("Project").is_empty());
    assert!(f.engine.all("Work").is_empty());
}

#[test]
fn new_global_input_yields_only_its_conversation_and_preserves_running_work() {
    let mut f = Fixture::new();
    approve(&mut f);
    let work = f.draft("Build the scene asynchronously");
    f.start(&work);
    let coordinator = f.coordinator(&work);
    let worker = f.plan(&work, &coordinator, false);
    f.start_invocation(&worker);
    let worker_before = f.engine.record(text(&worker, "id"), "Invocation").unwrap();
    let work_before = f.engine.record(text(&work, "id"), "Work").unwrap();

    let other_conversation = id();
    submit(&mut f, &id(), &other_conversation, None);
    let other = f
        .engine
        .related("Invocation", "conversationId", &other_conversation)[0]
        .clone();
    f.start_invocation(&other);
    let other_before = f.engine.record(text(&other, "id"), "Invocation").unwrap();
    let conversation = id();
    let console = id();
    submit(&mut f, &console, &conversation, None);
    let old = f
        .engine
        .related("Invocation", "conversationId", &conversation)[0]
        .clone();
    f.start_invocation(&old);
    let next = submit(&mut f, &console, &conversation, None);
    assert_eq!(next.status, "ok");
    let stopping = f.engine.record(text(&old, "id"), "Invocation").unwrap();
    assert_eq!(stopping["state"], "Stopping");
    assert_eq!(stopping["stopReason"], "NewConversationInput");
    assert_eq!(
        f.engine
            .related("Invocation", "conversationId", &conversation)
            .len(),
        1
    );
    let stop_id = text(&stopping, "stopOperationId").to_owned();

    // Further input coalesces while the exact old execution settles.
    let latest = submit(&mut f, &console, &conversation, None);
    assert_eq!(latest.status, "ok");
    assert_eq!(
        f.engine.record(text(&old, "id"), "Invocation").unwrap()["stopOperationId"],
        stop_id
    );
    f.report(
        &old,
        "TurnEnded",
        json!({"turnNumber":1,"finish":"Cancelled","quiescent":true}),
    );
    f.report(
        &old,
        "Settled",
        json!({
            "quiescent":true,"executionIdentity":format!("process-{}",text(&old,"id")),
            "completedOperationIds":[stop_id]
        }),
    );
    let release = f
        .engine
        .all("Operation")
        .into_iter()
        .find(|operation| {
            operation["effect"]["method"] == "runtime.release"
                && operation["effect"]["params"]["invocationId"] == old["id"]
        })
        .unwrap();
    f.engine
        .complete_effect(
            text(&release, "id"),
            Response::ok("", json!({"released":true})),
        )
        .unwrap();
    let new = f
        .engine
        .related("Invocation", "conversationId", &conversation)
        .into_iter()
        .find(|invocation| invocation["id"] != old["id"])
        .unwrap();
    assert_eq!(new["state"], "Dispatching");
    assert!(values(&new["coordinationInput"]["snapshot"], "messages")
        .iter()
        .any(|message| message["id"] == latest.data.as_ref().unwrap()["messageId"]));
    assert_eq!(
        f.engine
            .record(text(&old["subject"], "id"), "CoordinationTurn")
            .unwrap()["state"],
        "Superseded"
    );
    assert!(f
        .engine
        .all("AttentionItem")
        .iter()
        .all(|item| item["subjectId"] != old["subject"]["id"]));
    assert_eq!(
        f.engine.record(text(&worker, "id"), "Invocation").unwrap(),
        worker_before
    );
    assert_eq!(
        f.engine.record(text(&work, "id"), "Work").unwrap(),
        work_before
    );
    assert_eq!(
        f.engine.record(text(&other, "id"), "Invocation").unwrap(),
        other_before
    );
}

#[test]
fn new_global_input_during_startup_does_not_reactivate_obsolete_turn() {
    let mut f = Fixture::runtime_only();
    approve(&mut f);
    let console = id();
    let conversation = id();
    submit(&mut f, &console, &conversation, None);
    let old = f.engine.all("Invocation").pop().unwrap();
    submit(&mut f, &console, &conversation, None);
    let waiting_for_dispatch = f.engine.record(text(&old, "id"), "Invocation").unwrap();
    assert!(waiting_for_dispatch.get("supersededByInput").is_some());
    assert!(waiting_for_dispatch.get("stopOperationId").is_none());
    f.start_invocation(&old);
    let current = f.engine.record(text(&old, "id"), "Invocation").unwrap();
    assert_eq!(current["state"], "Stopping");
    assert!(current.get("executionIdentity").is_some());
    let actor = Principal::Invocation {
        invocation_id: text(&old, "id").into(),
    };
    assert!(f.engine.global_coordinator(&actor).is_err());
}

#[test]
fn intake_receipt_can_finish_waiting_and_next_turn_can_resolve_with_advertised_guard() {
    let mut f = Fixture::runtime_only();
    approve(&mut f);
    let console = id();
    let conversation = id();
    let submitted = submit(&mut f, &console, &conversation, None);
    let old = f.engine.all("Invocation").pop().unwrap();
    f.start_invocation(&old);
    let actor = Principal::Invocation {
        invocation_id: text(&old, "id").into(),
    };
    let question = f.command(
        actor.clone(),
        "conversation.request_input",
        json!({
            "turnId":old["subject"]["id"],"conversationId":conversation,
            "messageId":submitted.data.unwrap()["messageId"],
            "question":"Which existing directory?","responseSchema":{"type":"string"}
        }),
        vec![],
    );
    assert_eq!(question.status, "needs_input", "{question:?}");
    assert_eq!(question.subjects.len(), 1);
    let reference = &question.subjects[0];
    assert_eq!(reference.kind, "IntakeRequest");
    let finished = f.command(
        actor,
        "coordination.finish",
        json!({
            "turnId":old["subject"]["id"],"outcome":"WaitingOnRecordedSubject",
            "commandIds":[],"operationIds":[],"waitingSubject":reference,
            "explanation":"Waiting for the directory, not holding this turn"
        }),
        vec![],
    );
    assert_eq!(finished.status, "ok", "{finished:?}");
    f.finish_release(&old);
    let answer = submit(&mut f, &console, &conversation, None);
    let next = f
        .engine
        .all("Invocation")
        .into_iter()
        .find(|invocation| invocation["id"] != old["id"])
        .unwrap();
    f.start_invocation(&next);
    let mut request = Request::new(
        "conversation.resolve_input",
        json!({
            "requestId":reference.id,"messageId":answer.data.unwrap()["messageId"],
            "value":f.root.to_string_lossy()
        }),
    );
    request.command_id = Some(id());
    request.if_match = question.subjects.clone();
    let response = f.engine.handle(
        &Principal::Invocation {
            invocation_id: text(&next, "id").into(),
        },
        request,
    );
    assert_eq!(response.status, "ok", "{response:?}");
    assert_eq!(response.data.unwrap()["state"], "Answered");
    assert!(f.engine.all("Work").is_empty());
}

#[test]
fn global_response_activity_ends_on_failure_before_first_token() {
    let mut f = Fixture::runtime_only();
    approve(&mut f);
    let conversation = id();
    assert_eq!(submit(&mut f, &id(), &conversation, None).status, "ok");
    let invocation = f.engine.all("Invocation").pop().unwrap();
    f.report(
        &invocation,
        "TurnEnded",
        json!({"turnNumber":1,"finish":"Error","quiescent":true}),
    );
    let stored = f.engine.record(&conversation, "Conversation").unwrap();
    assert!(values(&stored, "messages").iter().any(|message| {
        message["id"] == invocation["replyMessageId"] && message["status"] == "Interrupted"
    }));
    let events = f
        .engine
        .events_after(None, &json!({"kind":"Conversation","id":conversation}))
        .unwrap();
    assert!(events.iter().any(|event| {
        event["kind"] == "MessageCompleted"
            && values(&event["changes"][0]["view"], "messages")
                .iter()
                .any(|message| message["status"] == "Interrupted")
    }));
}

#[test]
fn global_chat_does_not_infer_model_destination_approval() {
    let mut f = Fixture::runtime_only();
    let response = submit(&mut f, &id(), &id(), None);
    let failure = response.failure.unwrap();
    assert_eq!(failure.code, "ASSISTANT_UNAVAILABLE");
    assert!(!failure.message.contains("/project"));
    assert!(f.engine.all("Invocation").is_empty());
    assert!(f.engine.all("Project").is_empty());
    assert!(f.engine.all("ConversationItem").is_empty());
}

#[test]
fn project_hints_never_replace_the_global_console_binding() {
    let mut f = Fixture::new();
    approve(&mut f);
    let first_project = f.project.clone();
    let original = f.engine.record(&first_project, "Project").unwrap();
    let second = f.engine.create(
        "Project",
        json!({
            "name":"Another configured project","root":original["root"],"policyRevision":1,
            "coordinatorCapabilityId":"fixture-agent","workerCapabilityId":"fixture-agent",
            "checkCapabilityId":"native-check","limits":original["limits"]
        }),
    );
    let console = id();
    let conversation = id();
    let first = submit(&mut f, &console, &conversation, Some(&first_project));
    assert_eq!(first.status, "ok", "{first:?}");
    let second = submit(&mut f, &console, &conversation, Some(text(&second, "id")));
    assert_eq!(second.status, "ok", "{second:?}");
    assert_eq!(f.engine.all("ConsoleSession").len(), 1);
    assert_eq!(f.engine.all("Conversation").len(), 1);
    let returned = submit(&mut f, &console, &conversation, None);
    assert_eq!(returned.status, "ok", "{returned:?}");
    let rejected = submit(&mut f, &id(), &conversation, None);
    assert_eq!(rejected.failure.unwrap().code, "INVALID_REFERENCE");
    let legacy_rebind = f.command(
        Principal::Human,
        "console.open",
        json!({
            "consoleSessionId":console,"conversationId":conversation,"projectId":first_project
        }),
        vec![],
    );
    assert_eq!(legacy_rebind.failure.unwrap().code, "INVALID_REFERENCE");
}

#[test]
fn global_reads_executor_summary_without_accessing_or_prompting_its_session() {
    let mut f = Fixture::new();
    let work = super::executor::draft(&mut f);
    f.start(&work);
    let executor = super::executor::actor(&mut f, &work);
    f.start_invocation(&executor);
    f.report(
        &executor,
        "TextDelta",
        json!({
            "messageId":executor["replyMessageId"],"partId":"answer","chunkIndex":0,
            "text":"Implemented the fix; local checks passed. Human review is still required."
        }),
    );
    super::executor::finish(&mut f, &executor);
    approve(&mut f);
    submit(&mut f, &id(), &id(), None);
    let coordinator = f
        .engine
        .all("Invocation")
        .into_iter()
        .find(|v| v["scope"] == "Global")
        .unwrap();
    f.start_invocation(&coordinator);
    let principal = Principal::Invocation {
        invocation_id: text(&coordinator, "id").into(),
    };
    let before = f.engine.records.clone();
    let response = f.engine.handle(
        &principal,
        Request::new("work.get", json!({"workId":work["id"]})),
    );
    assert_eq!(response.status, "ok", "{response:?}");
    assert_eq!(
        response.data.unwrap()["executionSummary"]["recentResponses"][0]["text"],
        "Implemented the fix; local checks passed. Human review is still required."
    );
    assert_eq!(f.engine.records, before);
    let private = f
        .engine
        .record(
            text(&executor["executorInput"], "conversationId"),
            "Conversation",
        )
        .unwrap();
    assert!(f.engine.authorized_read(&principal, &private).is_err());
}

#[test]
fn global_reads_are_captured_project_access_not_other_windows_chat() {
    let mut f = Fixture::new();
    approve(&mut f);
    let project = f.project.clone();
    let work = f.draft("Existing approved work");
    let conversation = id();
    assert_eq!(submit(&mut f, &id(), &conversation, None).status, "ok");
    let invocation = f.engine.all("Invocation").pop().unwrap();
    f.start_invocation(&invocation);
    let actor = Principal::Invocation {
        invocation_id: text(&invocation, "id").into(),
    };
    assert!(f.engine.authorized_read(&actor, &work).is_ok());
    let private_conversation = id();
    assert_eq!(
        f.command(
            Principal::Human,
            "console.open",
            json!({
                "consoleSessionId":id(),"conversationId":private_conversation,"projectId":project
            }),
            vec![]
        )
        .status,
        "ok"
    );
    let private = f
        .engine
        .record(&private_conversation, "Conversation")
        .unwrap();
    assert_eq!(
        f.engine
            .authorized_read(&actor, &private)
            .unwrap_err()
            .failure
            .unwrap()
            .code,
        "FORBIDDEN"
    );
    let later_project = f.engine.create("Project", json!({"name":"Not captured"}));
    assert!(f.engine.authorized_read(&actor, &later_project).is_err());
    let control = f.command(
        actor.clone(),
        "work.control",
        json!({
            "workId":work["id"],"action":"Cancel"
        }),
        vec![work.clone()],
    );
    assert_eq!(control.failure.unwrap().code, "FORBIDDEN");
    assert!(f
        .engine
        .coordinator(&actor, Some(text(&work, "id")))
        .is_err());
    let current = f.engine.record(text(&work, "id"), "Work").unwrap();
    assert_eq!(current["lifecycle"], "Draft");
}

#[test]
fn a_global_actor_can_only_draft_in_captured_project_access() {
    let mut f = Fixture::new();
    approve(&mut f);
    let conversation = id();
    let response = submit(&mut f, &id(), &conversation, None);
    let message = response.data.unwrap()["messageId"].clone();
    let invocation = f.engine.all("Invocation").pop().unwrap();
    f.start_invocation(&invocation);
    let actor = Principal::Invocation {
        invocation_id: text(&invocation, "id").into(),
    };
    let mut draft = json!({
        "projectId":f.project,"goal":"A captured global goal","scope":["reports"],"exclusions":[],
        "criteria":[{"id":"report","description":"Readable report","evidenceRule":"artifact:report"}],
        "context":[],"delivery":{"kind":"Report"},"sourceMessageIds":[message]
    });
    let accepted = f.command(actor.clone(), "work.create_draft", draft.clone(), vec![]);
    assert_eq!(accepted.status, "ok", "{accepted:?}");
    let later = f.engine.create("Project", json!({"name":"Later approval"}));
    draft["projectId"] = later["id"].clone();
    let rejected = f.command(actor, "work.create_draft", draft, vec![]);
    assert_eq!(rejected.failure.unwrap().code, "FORBIDDEN");
    assert_eq!(f.engine.all("Work").len(), 1);
}

#[test]
fn global_snapshot_does_not_send_work_to_an_unapproved_project_destination() {
    let mut f = Fixture::new();
    approve(&mut f);
    let project = f.engine.create("Project", json!({
        "name":"Different destination","capabilityIds":["different-assistant"],
        "coordinatorCapabilityId":"different-assistant","workerCapabilityId":"different-assistant"
    }));
    let hidden = f.engine.create("Work", json!({
        "projectId":project["id"],"spec":{"goal":"Private other-destination goal"},"lifecycle":"Draft"
    }));
    assert_eq!(submit(&mut f, &id(), &id(), None).status, "ok");
    let invocation = f.engine.all("Invocation").pop().unwrap();
    assert!(!values(&invocation, "authorizedProjectIds").contains(&project["id"]));
    assert!(!serde_json::to_string(&invocation["coordinationInput"])
        .unwrap()
        .contains("Private other-destination goal"));
    f.start_invocation(&invocation);
    let actor = Principal::Invocation {
        invocation_id: text(&invocation, "id").into(),
    };
    assert!(f.engine.authorized_read(&actor, &hidden).is_err());
}

#[test]
fn global_change_proposals_accept_own_source_but_not_another_windows_chat() {
    let mut f = Fixture::new();
    approve(&mut f);
    let work = f.draft("Existing work for revision");
    f.start(&work);
    let work = f.engine.record(text(&work, "id"), "Work").unwrap();
    let response = submit(&mut f, &id(), &id(), None);
    let own_message = response.data.unwrap()["messageId"].clone();
    let invocation = f
        .engine
        .all("Invocation")
        .into_iter()
        .find(|invocation| invocation["scope"] == "Global")
        .unwrap();
    f.start_invocation(&invocation);
    let actor = Principal::Invocation {
        invocation_id: text(&invocation, "id").into(),
    };
    let mut replacement = work["spec"].clone();
    replacement["revision"] = work["currentSpecRevision"].clone();
    replacement["sourceMessageIds"] = json!([own_message]);
    let accepted = f.command(actor.clone(), "work.propose_change", json!({
        "workId":work["id"],"replacementSpec":replacement,"affectedTaskIds":[],"reason":"Explain the captured refinement"
    }), vec![work.clone()]);
    assert_eq!(accepted.status, "ok", "{accepted:?}");
    let other = submit(&mut f, &id(), &id(), None);
    replacement["sourceMessageIds"] = json!([other.data.unwrap()["messageId"]]);
    let rejected = f.command(actor, "work.propose_change", json!({
        "workId":work["id"],"replacementSpec":replacement,"affectedTaskIds":[],"reason":"Must not borrow private chat"
    }), vec![work]);
    assert_eq!(rejected.failure.unwrap().code, "FORBIDDEN");
}
