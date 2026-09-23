// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

use super::*;

#[test]
fn executor_claim_proposals_distinguish_reconciliation_hold_and_reconstruction() {
    let mut f = Fixture::new();
    let work = f.draft("Claim proposal states");
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
    let (conversation, message, principal) = action_conversation(
        &mut f,
        "Continue the claim; ask before reconstructing history",
    );
    let current = f.engine.record(text(&work, "id"), "Work").unwrap();
    let request = action_request(
        &conversation,
        &message,
        "work.claim_executor",
        json!({"workId":work["id"]}),
        &[current.clone()],
    );
    assert_eq!(
        f.engine.handle(&principal, request).failure.unwrap().code,
        "BAD_STATE"
    );
    let request = action_request(
        &conversation,
        &message,
        "work.continue",
        json!({"workId":work["id"],"restartSession":true}),
        &[current.clone()],
    );
    let response = f.engine.handle(&principal, request);
    assert_eq!(response.status, "ok", "{response:?}");
    let preview = &response.data.as_ref().unwrap()["proposal"]["preview"]["continuation"];
    assert_eq!(preview["sessionBehavior"], "ReconcileExistingClaim");
    assert_eq!(preview["restartSession"], false);
    assert_eq!(
        f.command(
            Principal::Human,
            "work.control",
            json!({"workId":work["id"],"action":"Hold"}),
            vec![current]
        )
        .status,
        "ok"
    );
    let current = f.engine.record(text(&work, "id"), "Work").unwrap();
    let request = action_request(
        &conversation,
        &message,
        "work.continue",
        json!({"workId":work["id"]}),
        &[current.clone()],
    );
    let response = f.engine.handle(&principal, request);
    assert_eq!(response.status, "ok", "{response:?}");
    assert_eq!(
        response.data.unwrap()["proposal"]["preview"]["continuation"]["sessionBehavior"],
        "AdoptVerifiedActualExecutorAfterSettlement"
    );
    f.finish_release(&coordinator);
    let current = f.engine.record(text(&work, "id"), "Work").unwrap();
    assert_eq!(
        f.command(
            Principal::Human,
            "work.continue",
            json!({"workId":work["id"]}),
            vec![current]
        )
        .status,
        "ok"
    );
    let current = f.engine.record(text(&work, "id"), "Work").unwrap();
    assert_eq!(current["executorClaim"]["status"], "NeedsConsent");
    let request = action_request(
        &conversation,
        &message,
        "work.claim_executor",
        json!({"workId":work["id"]}),
        &[current.clone()],
    );
    assert_eq!(
        f.engine.handle(&principal, request).failure.unwrap().code,
        "EXECUTOR_SESSION_UNAVAILABLE"
    );
    let request = action_request(
        &conversation,
        &message,
        "work.continue",
        json!({"workId":work["id"],"restartSession":true}),
        &[current],
    );
    let response = f.engine.handle(&principal, request);
    assert_eq!(response.status, "ok", "{response:?}");
    let preview = &response.data.as_ref().unwrap()["proposal"]["preview"]["continuation"];
    assert_eq!(preview["sessionBehavior"], "ReconstructAfterSettlement");
    assert_eq!(preview["restartSession"], true);
}

// Component fixtures bind an already-running global invocation. They do not
// demonstrate native UI, adapter dispatch, or live-model behavior.
fn action_conversation(f: &mut Fixture, message_text: &str) -> (Value, Value, Principal) {
    let conversation = f
        .engine
        .create("Conversation", json!({"scope":"Global","messages":[]}));
    let message = action_message(f, &conversation, message_text);
    let principal = action_invocation(f, &conversation);
    (conversation, message, principal)
}

fn action_message(f: &mut Fixture, conversation: &Value, message_text: &str) -> Value {
    let message = f.engine.create(
        "ConversationItem",
        json!({
            "conversationId":conversation["id"],
            "role":"human","status":"Complete","text":message_text,
            "clientMessageId":id(),"attachments":[]
        }),
    );
    f.engine.emit_message("MessageRecorded", &message);
    message
}

fn action_invocation(f: &mut Fixture, conversation: &Value) -> Principal {
    let conversation = f
        .engine
        .record(text(conversation, "id"), "Conversation")
        .unwrap();
    let policy = match f.engine.all("GlobalConversationPolicy").into_iter().next() {
        Some(policy) => policy,
        None => {
            let response = f.command(
                Principal::Human,
                "console.configure",
                json!({
                    "capabilityId":"fixture-agent","workerCapabilityId":"fixture-agent",
                    "checkCapabilityId":"native-check","approvedModelDestination":"fixture-approved-provider",
                    "limits":{"concurrency":4,"executionAttempts":8,"evaluationAttempts":16,
                        "coordinationTurns":12,"contextRounds":3,"executionSeconds":60,"coordinationSeconds":60}
                }),
                vec![],
            );
            assert_eq!(response.status, "ok", "{response:?}");
            response.data.unwrap()
        }
    };
    let projects = if f.project.is_empty() {
        vec![]
    } else {
        vec![f.project.clone()]
    };
    let invocation_id = id();
    let turn = f.engine.create(
        "CoordinationTurn",
        json!({
            "scope":"Global","scopeId":conversation["id"],"conversationId":conversation["id"],
            "invocationId":invocation_id,"state":"Running"
        }),
    );
    let mut snapshot = f.engine.view(&conversation);
    snapshot["preferences"] = f.engine.memory_snapshot(None);
    f.engine.put(json!({
        "id":invocation_id,"kind":"Invocation","scope":"Global","state":"Running",
        "conversationId":conversation["id"],"authorizedProjectIds":projects,
        "globalPolicyId":policy["id"],"globalPolicyVersion":policy["version"],
        "subject":{"kind":"Coordination","id":turn["id"]},
        "coordinationInput":{"scope":{"conversationId":conversation["id"]},
            "snapshot":snapshot}
    }));
    Principal::Invocation { invocation_id }
}

fn action_request(
    conversation: &Value,
    message: &Value,
    method: &str,
    params: Value,
    guards: &[Value],
) -> Request {
    let mut request = Request::new(
        "conversation.propose_action",
        json!({
            "conversationId":conversation["id"],"messageId":message["id"],
            "method":method,"params":params,
            "ifMatch":guards.iter().map(Engine::reference).collect::<Vec<_>>(),
            "summary":"Untrusted explanation, not authorization facts"
        }),
    );
    request.command_id = Some(id());
    request
}

fn propose_control(
    f: &mut Fixture,
    principal: &Principal,
    conversation: &Value,
    message: &Value,
    work: &Value,
    action: &str,
) -> Value {
    let request = action_request(
        conversation,
        message,
        "work.control",
        json!({"workId":work["id"],"action":action}),
        std::slice::from_ref(work),
    );
    let response = f.engine.handle(principal, request);
    assert_eq!(response.status, "ok", "{response:?}");
    response.data.unwrap()["proposal"].clone()
}

fn frozen_action(proposal: &Value) -> Request {
    let envelope = &proposal["request"];
    let mut request = Request::new(text(envelope, "method"), envelope["params"].clone());
    request.command_id = Some(text(envelope, "commandId").into());
    request.if_match = parse(&envelope["ifMatch"]).unwrap();
    request
}

#[test]
fn human_action_component_proposal_never_executes_or_grants_invocation_authority() {
    let mut f = Fixture::new();
    let work = f.draft("Leave the target unchanged until human approval");
    let (conversation, message, principal) = action_conversation(&mut f, "Cancel that draft");
    let proposal = propose_control(&mut f, &principal, &conversation, &message, &work, "Cancel");
    assert_eq!(proposal["status"], "Open");
    assert_ne!(proposal["id"], proposal["request"]["commandId"]);
    assert_eq!(proposal["preview"]["work"]["work"], work);
    assert_eq!(proposal["preview"]["project"]["id"], f.project);
    assert_eq!(f.engine.record(text(&work, "id"), "Work").unwrap(), work);
    assert!(f.engine.take_effects().unwrap().is_empty());

    let response = f.engine.handle(&principal, frozen_action(&proposal));
    assert_eq!(response.failure.unwrap().code, "FORBIDDEN");
    assert_eq!(f.engine.record(text(&work, "id"), "Work").unwrap(), work);
    assert_eq!(
        f.engine
            .record(text(&proposal, "id"), "HumanActionProposal")
            .unwrap()["status"],
        "Open"
    );
}

#[test]
fn continuation_proposal_is_frozen_work_scoped_and_human_confirmed() {
    let mut f = Fixture::new();
    let work = f.draft("Continue named work without a scheduler-only resume");
    f.start(&work);
    f.coordinator(&work);
    let work = f.engine.record(text(&work, "id"), "Work").unwrap();
    let (conversation, message, principal) =
        action_conversation(&mut f, "Continue that named work");
    let before = f.engine.related("Invocation", "workId", text(&work, "id"));
    let request = action_request(
        &conversation,
        &message,
        "work.continue",
        json!({"workId":work["id"]}),
        std::slice::from_ref(&work),
    );
    let response = f.engine.handle(&principal, request);
    assert_eq!(response.status, "ok", "{response:?}");
    let proposal = response.data.unwrap()["proposal"].clone();
    assert_eq!(proposal["request"]["method"], "work.continue");
    assert_eq!(
        proposal["request"]["ifMatch"],
        json!([Engine::reference(&work)])
    );
    assert_eq!(
        proposal["preview"]["continuation"]["sessionBehavior"],
        "AttachExistingExecution"
    );
    assert_eq!(proposal["preview"]["continuation"]["restartSession"], false);
    assert_eq!(
        proposal["preview"]["continuation"]["createsIndependentWriter"],
        false
    );
    let work_conversation = f
        .engine
        .related("Conversation", "workId", text(&work, "id"))
        .pop()
        .unwrap();
    let work_chat = f.engine.view(&work_conversation);
    assert!(values(&work_chat, "actionProposals")
        .iter()
        .any(|action| action["id"] == proposal["id"]));
    assert_eq!(
        proposal["conversationId"], conversation["id"],
        "Projection must not rebind the frozen source authority"
    );
    assert!(f
        .engine
        .events_after(
            None,
            &json!({"kind":"Conversation","id":work_conversation["id"]})
        )
        .unwrap()
        .iter()
        .any(|event| event["kind"] == "HumanActionProposed"));
    assert_eq!(
        f.engine.related("Invocation", "workId", text(&work, "id")),
        before
    );
    assert_eq!(
        f.engine
            .handle(&principal, frozen_action(&proposal))
            .failure
            .unwrap()
            .code,
        "FORBIDDEN"
    );
    let response = f.engine.handle(&Principal::Human, frozen_action(&proposal));
    assert_eq!(response.status, "ok", "{response:?}");
    assert_eq!(response.data.unwrap()["workView"]["work"]["id"], work["id"]);
    assert_eq!(
        f.engine.related("Invocation", "workId", text(&work, "id")),
        before
    );
    assert_eq!(f.engine.all("Work").len(), 1);
    assert_eq!(
        f.engine
            .record(text(&proposal, "id"), "HumanActionProposal")
            .unwrap()["status"],
        "Submitted"
    );
}

#[test]
fn continuation_proposal_discloses_explicit_reconstruction_without_silent_fallback() {
    let mut f = Fixture::new();
    let work = f.draft("Reconstruction needs separate disclosed human approval");
    f.start(&work);
    let coordinator = f.coordinator(&work);
    f.finish_release(&coordinator);
    let mut work = f.engine.record(text(&work, "id"), "Work").unwrap();
    work.as_object_mut().unwrap().remove("primarySession");
    let work = f.engine.put(work);
    let (conversation, message, principal) = action_conversation(&mut f, "Continue the old work");
    let default_request = action_request(
        &conversation,
        &message,
        "work.continue",
        json!({"workId":work["id"]}),
        std::slice::from_ref(&work),
    );
    assert_eq!(
        f.engine
            .handle(&principal, default_request)
            .failure
            .unwrap()
            .code,
        "SESSION_RESUME_UNAVAILABLE"
    );
    let request = action_request(
        &conversation,
        &message,
        "work.continue",
        json!({"workId":work["id"],"restartSession":true}),
        std::slice::from_ref(&work),
    );
    let response = f.engine.handle(&principal, request);
    assert_eq!(response.status, "ok", "{response:?}");
    let proposal = response.data.unwrap()["proposal"].clone();
    assert_eq!(proposal["preview"]["continuation"]["restartSession"], true);
    assert_eq!(
        proposal["preview"]["continuation"]["sessionBehavior"],
        "ReconstructAfterSettlement"
    );
    assert_eq!(
        proposal["preview"]["continuation"]["preservesProviderHistory"],
        false
    );
    assert_eq!(
        f.engine
            .related("Invocation", "workId", text(&work, "id"))
            .len(),
        1
    );
    let response = f.engine.handle(&Principal::Human, frozen_action(&proposal));
    assert_eq!(response.status, "ok", "{response:?}");
    let next = f
        .engine
        .related("Invocation", "workId", text(&work, "id"))
        .into_iter()
        .find(|invocation| invocation["state"] == "Dispatching")
        .unwrap();
    assert!(next.get("sessionReuseRef").is_none());
    assert_eq!(f.engine.all("Work").len(), 1);
}

#[test]
fn continuation_proposals_require_exact_work_guards_and_authorized_reads() {
    let mut f = Fixture::new();
    let work = f.draft("Protect the proposed continuation target");
    f.start(&work);
    f.coordinator(&work);
    let work = f.engine.record(text(&work, "id"), "Work").unwrap();
    let (conversation, message, principal) = action_conversation(&mut f, "Continue this work");
    let missing = action_request(
        &conversation,
        &message,
        "work.continue",
        json!({"workId":work["id"]}),
        &[],
    );
    assert_eq!(
        f.engine.handle(&principal, missing).failure.unwrap().code,
        "INVALID_ARGUMENT"
    );
    let project = f
        .engine
        .create("Project", json!({"name":"Uncaptured project"}));
    let mut foreign = work.clone();
    foreign["projectId"] = project["id"].clone();
    let foreign = f.engine.create("Work", foreign);
    let forbidden = action_request(
        &conversation,
        &message,
        "work.continue",
        json!({"workId":foreign["id"]}),
        &[foreign],
    );
    assert_eq!(
        f.engine.handle(&principal, forbidden).failure.unwrap().code,
        "FORBIDDEN"
    );
    assert!(f.engine.all("HumanActionProposal").is_empty());
}

#[test]
fn human_action_component_rejects_wrong_conversation_and_uncaptured_source() {
    let mut f = Fixture::new();
    let work = f.draft("Conversation-bound target");
    let (conversation, message, principal) = action_conversation(&mut f, "Hold");
    let (other, other_message, _) = action_conversation(&mut f, "Another conversation");
    for (conversation, message) in [(&other, &other_message), (&conversation, &other_message)] {
        let request = action_request(
            conversation,
            message,
            "work.control",
            json!({"workId":work["id"],"action":"Hold"}),
            std::slice::from_ref(&work),
        );
        let response = f.engine.handle(&principal, request);
        assert_eq!(response.failure.unwrap().code, "FORBIDDEN");
    }
    let new_message = action_message(&mut f, &conversation, "This was not captured");
    let request = action_request(
        &conversation,
        &new_message,
        "work.control",
        json!({"workId":work["id"],"action":"Hold"}),
        std::slice::from_ref(&work),
    );
    assert_eq!(
        f.engine.handle(&principal, request).failure.unwrap().code,
        "FORBIDDEN"
    );
    assert_eq!(message["role"], "human");
    assert!(f.engine.all("HumanActionProposal").is_empty());
}

#[test]
fn human_action_component_rejects_assistant_source_and_late_old_human_intent() {
    let mut f = Fixture::new();
    let work = f.draft("Source provenance");
    let (conversation, message, principal) = action_conversation(&mut f, "Hold A");
    action_message(&mut f, &conversation, "Actually discuss B instead");
    let request = action_request(
        &conversation,
        &message,
        "work.control",
        json!({"workId":work["id"],"action":"Hold"}),
        std::slice::from_ref(&work),
    );
    assert_eq!(
        f.engine.handle(&principal, request).failure.unwrap().code,
        "STALE_VERSION"
    );
    let assistant = f.engine.create(
        "ConversationItem",
        json!({"conversationId":conversation["id"],"role":"assistant","text":"Approve myself"}),
    );
    f.engine.emit_message("MessageRecorded", &assistant);
    let principal = action_invocation(&mut f, &conversation);
    let request = action_request(
        &conversation,
        &assistant,
        "work.control",
        json!({"workId":work["id"],"action":"Hold"}),
        std::slice::from_ref(&work),
    );
    assert_eq!(
        f.engine.handle(&principal, request).failure.unwrap().code,
        "FORBIDDEN"
    );
}

#[test]
fn human_action_component_requires_exact_current_mutable_subjects() {
    let mut f = Fixture::new();
    let work = f.draft("Exact guard");
    let other = f.draft("Not the mutable subject");
    let project = f.engine.record(&f.project, "Project").unwrap();
    let (conversation, message, principal) = action_conversation(&mut f, "Hold");
    for guards in [
        vec![],
        vec![other],
        vec![work.clone(), project],
        vec![work.clone(), work.clone()],
    ] {
        let request = action_request(
            &conversation,
            &message,
            "work.control",
            json!({"workId":work["id"],"action":"Hold"}),
            &guards,
        );
        assert_eq!(
            f.engine.handle(&principal, request).failure.unwrap().code,
            "INVALID_ARGUMENT"
        );
    }
    let mut stale = work.clone();
    stale["version"] = json!(number(&work, "version") + 1);
    let request = action_request(
        &conversation,
        &message,
        "work.control",
        json!({"workId":work["id"],"action":"Hold"}),
        &[stale],
    );
    assert_eq!(
        f.engine.handle(&principal, request).failure.unwrap().code,
        "STALE_VERSION"
    );
    let mut outer_guard = action_request(
        &conversation,
        &message,
        "work.control",
        json!({"workId":work["id"],"action":"Hold"}),
        std::slice::from_ref(&work),
    );
    outer_guard.if_match = vec![Engine::reference(&work)];
    assert_eq!(
        f.engine
            .handle(&principal, outer_guard)
            .failure
            .unwrap()
            .code,
        "INVALID_ARGUMENT"
    );
}

#[test]
fn human_action_component_rejects_edited_envelope_and_superseded_intent() {
    let mut f = Fixture::new();
    let work = f.draft("Frozen target");
    let (conversation, message, principal) = action_conversation(&mut f, "Review cancellation");
    let first = propose_control(&mut f, &principal, &conversation, &message, &work, "Cancel");
    let original = frozen_action(&first);
    let mut changed = original.clone();
    changed.params["action"] = json!("Hold");
    assert_eq!(
        f.engine
            .validate_human_action(&Principal::Human, &changed)
            .unwrap_err()
            .failure
            .unwrap()
            .code,
        "INVALID_ARGUMENT"
    );
    let mut changed_method = original.clone();
    changed_method.method = "project.configure".into();
    assert_eq!(
        f.engine
            .validate_human_action(&Principal::Human, &changed_method)
            .unwrap_err()
            .failure
            .unwrap()
            .code,
        "INVALID_ARGUMENT"
    );
    let mut changed_guards = original.clone();
    changed_guards.if_match.clear();
    assert_eq!(
        f.engine
            .validate_human_action(&Principal::Human, &changed_guards)
            .unwrap_err()
            .failure
            .unwrap()
            .code,
        "INVALID_ARGUMENT"
    );
    let second = propose_control(&mut f, &principal, &conversation, &message, &work, "Hold");
    assert_eq!(second["status"], "Open");
    assert_eq!(
        f.engine
            .record(text(&first, "id"), "HumanActionProposal")
            .unwrap()["status"],
        "Superseded"
    );
    assert_eq!(
        f.engine
            .handle(&Principal::Human, original)
            .failure
            .unwrap()
            .code,
        "BAD_STATE"
    );
    assert_eq!(f.engine.record(text(&work, "id"), "Work").unwrap(), work);
}

#[test]
fn human_action_component_target_a_survives_new_message_and_target_b_proposal() {
    let mut f = Fixture::new();
    let a = f.draft("Work A");
    let b = f.draft("Work B");
    let (conversation, message, principal) = action_conversation(&mut f, "Hold A");
    let proposal_a = propose_control(&mut f, &principal, &conversation, &message, &a, "Hold");
    let message_b = action_message(&mut f, &conversation, "Cancel B");
    let principal_b = action_invocation(&mut f, &conversation);
    let proposal_b = propose_control(
        &mut f,
        &principal_b,
        &conversation,
        &message_b,
        &b,
        "Cancel",
    );
    assert_eq!(
        f.engine
            .record(text(&proposal_a, "id"), "HumanActionProposal")
            .unwrap()["status"],
        "Open"
    );
    assert_eq!(proposal_b["workId"], b["id"]);
    let Principal::Invocation { invocation_id } = principal else {
        unreachable!();
    };
    let mut completed = f.engine.record(&invocation_id, "Invocation").unwrap();
    completed["state"] = json!("Released");
    f.engine.put(completed);
    let response = f
        .engine
        .handle(&Principal::Human, frozen_action(&proposal_a));
    assert_eq!(response.status, "ok", "{response:?}");
    assert_eq!(f.engine.record(text(&b, "id"), "Work").unwrap(), b);
}

#[test]
fn human_action_component_exact_human_retry_uses_original_receipt() {
    let mut f = Fixture::new();
    let work = f.draft("Idempotent hold");
    let (conversation, message, principal) = action_conversation(&mut f, "Hold it");
    let proposal = propose_control(&mut f, &principal, &conversation, &message, &work, "Hold");
    let request = frozen_action(&proposal);
    let first = f.engine.handle(&Principal::Human, request.clone());
    assert_eq!(first.status, "ok", "{first:?}");
    let record = f
        .engine
        .record(text(&proposal, "id"), "HumanActionProposal")
        .unwrap();
    assert_eq!(record["status"], "Submitted");
    let target = f.engine.record(text(&work, "id"), "Work").unwrap();
    let cursor = f.engine.cursor();
    let second = f.engine.handle(&Principal::Human, request);
    assert_eq!(encode(&first).unwrap(), encode(&second).unwrap());
    assert_eq!(f.engine.cursor(), cursor);
    assert_eq!(f.engine.record(text(&work, "id"), "Work").unwrap(), target);
    assert_eq!(
        f.engine
            .record(text(&proposal, "id"), "HumanActionProposal")
            .unwrap(),
        record
    );
    assert_eq!(
        f.engine
            .related("CoordinationTrigger", "subjectId", text(&proposal, "id"))
            .iter()
            .filter(|trigger| text(trigger, "reason") == "HumanActionSubmitted")
            .count(),
        1
    );
}

#[test]
fn human_action_component_start_records_pending_not_completion() {
    let mut f = Fixture::new();
    let work = f.draft("A pending start is not finished work");
    let grant = f.command(
        Principal::Human,
        "grant.preview",
        json!({"workId":work["id"],"specRevision":1,"policyRevision":1}),
        vec![work.clone()],
    );
    assert_eq!(grant.status, "ok", "{grant:?}");
    let grant = grant.data.unwrap()["proposal"].clone();
    let (conversation, message, principal) = action_conversation(&mut f, "Start it");
    let request = action_request(
        &conversation,
        &message,
        "work.start",
        json!({
            "workId":work["id"],"specRevision":1,"projectPolicyRevision":1,
            "grantProposalId":grant["id"]
        }),
        std::slice::from_ref(&work),
    );
    let response = f.engine.handle(&principal, request);
    assert_eq!(response.status, "ok", "{response:?}");
    let proposal = response.data.unwrap()["proposal"].clone();
    assert_eq!(proposal["preview"]["grant"]["data"]["proposal"], grant);
    assert_eq!(
        f.engine.record(text(&work, "id"), "Work").unwrap()["lifecycle"],
        "Draft"
    );
    assert!(f.engine.take_effects().unwrap().is_empty());
    let response = f.engine.handle(&Principal::Human, frozen_action(&proposal));
    assert_eq!(response.status, "pending", "{response:?}");
    let proposal = f
        .engine
        .record(text(&proposal, "id"), "HumanActionProposal")
        .unwrap();
    assert_eq!(proposal["status"], "Submitted");
    assert_eq!(proposal["submission"]["status"], "pending");
    assert_eq!(
        proposal["submission"]["operationId"],
        json!(response.operation_id)
    );
    assert_eq!(
        f.engine.record(text(&work, "id"), "Work").unwrap()["lifecycle"],
        "Active"
    );
    assert!(f.engine.all("Acceptance").is_empty());
}

#[test]
fn human_action_component_ignores_unrelated_commands_and_blocks_arbitrary_methods() {
    let mut f = Fixture::new();
    let work = f.draft("Unrelated human commands stay available");
    let (conversation, message, principal) = action_conversation(&mut f, "Discuss");
    for method in [
        "runtime.register",
        "runtime.invoke",
        "result.submit",
        "run_command",
    ] {
        let request = action_request(&conversation, &message, method, json!({}), &[]);
        assert_eq!(
            f.engine.handle(&principal, request).failure.unwrap().code,
            "METHOD_UNSUPPORTED"
        );
    }
    let mut unrelated = Request::new("work.control", json!({"workId":work["id"],"action":"Hold"}));
    unrelated.command_id = Some(id());
    unrelated.if_match = vec![Engine::reference(&work)];
    assert!(f
        .engine
        .validate_human_action(&Principal::Human, &unrelated)
        .is_ok());
    assert!(f
        .engine
        .record_human_action(&unrelated, &Response::ok("", json!({})))
        .is_ok());
    assert!(f.engine.all("HumanActionProposal").is_empty());
}

#[test]
fn human_action_component_requires_captured_project_authority() {
    let mut f = Fixture::new();
    let work = f.draft("Project access is captured at dispatch");
    let (conversation, message, principal) = action_conversation(&mut f, "Hold it");
    let Principal::Invocation { invocation_id } = &principal else {
        unreachable!();
    };
    let mut invocation = f.engine.record(invocation_id, "Invocation").unwrap();
    invocation["authorizedProjectIds"] = json!([]);
    f.engine.put(invocation);
    let request = action_request(
        &conversation,
        &message,
        "work.control",
        json!({"workId":work["id"],"action":"Hold"}),
        std::slice::from_ref(&work),
    );
    assert_eq!(
        f.engine.handle(&principal, request).failure.unwrap().code,
        "FORBIDDEN"
    );
    assert!(f.engine.all("HumanActionProposal").is_empty());
}

#[test]
fn human_action_component_failure_does_not_claim_submission() {
    let mut f = Fixture::new();
    let work = f.draft("Failed commands remain unsubmitted");
    let (conversation, message, principal) = action_conversation(&mut f, "Hold");
    let proposal = propose_control(&mut f, &principal, &conversation, &message, &work, "Hold");
    let request = frozen_action(&proposal);
    f.engine
        .record_human_action(
            &request,
            &Response::fail("", "EXECUTION_FAILED", "No successful mutation"),
        )
        .unwrap();
    assert_eq!(
        f.engine
            .record(text(&proposal, "id"), "HumanActionProposal")
            .unwrap(),
        proposal
    );
    assert!(f
        .engine
        .record_human_action(&request, &Response::pending("", id(), json!({})))
        .is_err());
    assert_eq!(
        f.engine
            .record(text(&proposal, "id"), "HumanActionProposal")
            .unwrap()["status"],
        "Open"
    );
}

fn proposed_project_params(f: &Fixture) -> Value {
    json!({
        "name":"Human supplied project","root":f.root.to_string_lossy(),
        "coordinatorCapabilityId":"fixture-agent","workerCapabilityId":"fixture-agent",
        "checkCapabilityId":"native-check",
        "limits":{"concurrency":4,"executionAttempts":8,"evaluationAttempts":16,
            "coordinationTurns":12,"contextRounds":3,"executionSeconds":60,"coordinationSeconds":60}
    })
}

fn saved_project_root(f: &mut Fixture) -> Value {
    let response = f.command(
        Principal::Human,
        "memory.store",
        json!({"key":"workspace.code_root","scope":"User",
            "content":format!("My code repositories are stored under {}.", f.root.display())}),
        vec![],
    );
    assert_eq!(response.status, "ok", "{response:?}");
    response.data.unwrap()
}

fn new_project_params(f: &Fixture, preference: &Value) -> Value {
    let mut params = proposed_project_params(f);
    params["root"] = json!(f.root.join("3d-human-website"));
    params["createDirectory"] = json!(true);
    params["rootPreference"] = json!(Engine::reference(preference));
    params
}

#[test]
fn project_creation_uses_saved_root_but_waits_for_approval_and_receipt() {
    let mut f = Fixture::new();
    let preference = saved_project_root(&mut f);
    let (conversation, message, principal) =
        action_conversation(&mut f, "Start a new project for a 3D human website");
    let params = new_project_params(&f, &preference);
    let target = f.root.join("3d-human-website");
    let response = f.engine.handle(
        &principal,
        action_request(&conversation, &message, "project.configure", params, &[]),
    );
    assert_eq!(response.status, "ok", "{response:?}");
    let proposal = response.data.unwrap()["proposal"].clone();
    assert!(!target.exists());
    assert!(f.engine.take_effects().unwrap().is_empty());
    let request = frozen_action(&proposal);
    assert_eq!(
        f.engine
            .handle(&principal, request.clone())
            .failure
            .unwrap()
            .code,
        "FORBIDDEN"
    );
    let Principal::Invocation { invocation_id } = &principal else {
        unreachable!()
    };
    let mut invocation = f.engine.record(invocation_id, "Invocation").unwrap();
    invocation["state"] = json!("Released");
    f.engine.put(invocation);
    let response = f.engine.handle(&Principal::Human, request.clone());
    assert_eq!(response.status, "pending", "{response:?}");
    let operation_id = response.operation_id.as_deref().unwrap();
    let project_id = text(response.data.as_ref().unwrap(), "projectId");
    assert!(f.engine.record(project_id, "Project").is_err());
    assert!(!target.exists());
    assert_eq!(
        f.engine
            .related("Invocation", "conversationId", text(&conversation, "id"))
            .len(),
        1,
        "A pending directory operation must not start a model with stale project access"
    );
    assert!(f.engine.all("CoordinationTrigger").iter().all(|trigger| {
        trigger["conversationId"] != conversation["id"]
            || trigger["reason"] != "HumanActionSubmitted"
    }));
    let replay = f.engine.handle(&Principal::Human, request);
    assert_eq!(encode(&replay).unwrap(), encode(&response).unwrap());
    let effects = f.engine.take_effects().unwrap();
    assert_eq!(effects.len(), 1);
    let effect = &effects[0];
    assert_eq!(effect.method, "project.create");
    assert_eq!(effect.id, operation_id);
    let read = f.engine.handle(
        &Principal::Human,
        Request::new("operation.get", json!({"operationId":operation_id})),
    );
    assert_eq!(read.status, "ok", "{read:?}");

    std::fs::create_dir(&target).unwrap();
    let receipt = Response::ok(
        "",
        json!({
            "projectId":project_id,"root":effect.params["project"]["root"],"commitId":"a".repeat(40)
        }),
    );
    f.engine
        .complete_effect(operation_id, receipt.clone())
        .unwrap();
    let registered = f.engine.record(project_id, "Project").unwrap();
    assert_eq!(registered["root"], effect.params["project"]["root"]);
    let submitted = f
        .engine
        .record(text(&proposal, "id"), "HumanActionProposal")
        .unwrap();
    assert_eq!(submitted["submission"]["status"], "pending");
    assert_eq!(submitted["completion"]["status"], "Succeeded");
    assert_eq!(submitted["completion"]["result"]["projectId"], project_id);
    let triggers = f.engine.all("CoordinationTrigger");
    assert!(triggers
        .iter()
        .any(|trigger| trigger["reason"] == "HumanActionCompleted"
            && trigger["conversationId"] == conversation["id"]));
    f.engine.complete_effect(operation_id, receipt).unwrap();
    assert_eq!(f.engine.record(project_id, "Project").unwrap(), registered);
    assert_eq!(f.engine.all("CoordinationTrigger"), triggers);

    assert_eq!(
        f.engine
            .related("Invocation", "conversationId", text(&conversation, "id"))
            .len(),
        2,
        "The terminal creation receipt starts exactly one follow-up"
    );
    let next = f
        .engine
        .all("Invocation")
        .into_iter()
        .find(|invocation| {
            text(invocation, "id") != invocation_id
                && invocation["conversationId"] == conversation["id"]
        })
        .expect("completion must dispatch a fresh global turn");
    assert!(values(&next, "authorizedProjectIds").contains(&json!(project_id)));
    assert!(values(&next["coordinationInput"]["snapshot"], "projects")
        .iter()
        .any(|project| project["id"] == project_id));
    assert_eq!(values(&next["coordinationInput"], "triggerEvents").len(), 1);
    assert_eq!(
        next["coordinationInput"]["triggerEvents"][0]["kind"],
        "HumanActionCompleted"
    );
    assert!(f.engine.all("Work").is_empty());
    assert!(f.engine.all("Task").is_empty());
}

#[test]
fn project_creation_approval_rollback_has_no_filesystem_effect_or_reserved_project() {
    let mut f = Fixture::new();
    let preference = saved_project_root(&mut f);
    let (conversation, message, principal) = action_conversation(&mut f, "Create a new website");
    let params = new_project_params(&f, &preference);
    let response = f.engine.handle(
        &principal,
        action_request(&conversation, &message, "project.configure", params, &[]),
    );
    assert_eq!(response.status, "ok", "{response:?}");
    let proposal = response.data.unwrap()["proposal"].clone();
    let records = f.engine.records.clone();
    f.engine.db.execute_batch(
        "CREATE TEMP TRIGGER fail_project_creation BEFORE INSERT ON effects
         WHEN NEW.method='project.create' BEGIN SELECT RAISE(ABORT,'Injected creation persistence failure'); END;"
    ).unwrap();
    let response = f.engine.handle(&Principal::Human, frozen_action(&proposal));
    assert!(response.failure.is_some(), "{response:?}");
    assert_eq!(f.engine.records, records);
    assert!(!f.root.join("3d-human-website").exists());
    assert!(f.engine.take_effects().unwrap().is_empty());
    f.engine
        .db
        .execute_batch("DROP TRIGGER fail_project_creation")
        .unwrap();
    let response = f.engine.handle(&Principal::Human, frozen_action(&proposal));
    assert_eq!(response.status, "pending", "{response:?}");
    assert!(!f.root.join("3d-human-website").exists());
}

#[test]
fn project_creation_rechecks_preference_and_collision_at_confirmation() {
    for change in ["replace", "forget", "collision"] {
        let mut f = Fixture::new();
        let preference = saved_project_root(&mut f);
        let (conversation, message, principal) =
            action_conversation(&mut f, "Start a new 3D website");
        let params = new_project_params(&f, &preference);
        let response = f.engine.handle(
            &principal,
            action_request(&conversation, &message, "project.configure", params, &[]),
        );
        assert_eq!(response.status, "ok", "{response:?}");
        let proposal = response.data.unwrap()["proposal"].clone();
        let target = f.root.join("3d-human-website");
        match change {
            "replace" => {
                let mut updated = preference.clone();
                updated["content"] = json!("Use another code root instead.");
                f.engine.put(updated);
            }
            "forget" => {
                let response = f.command(
                    Principal::Human,
                    "memory.forget",
                    json!({"preferenceId":preference["id"]}),
                    vec![preference.clone()],
                );
                assert_eq!(response.status, "ok", "{response:?}");
            }
            _ => {
                std::fs::create_dir(&target).unwrap();
                std::fs::write(target.join("keep.txt"), "Preserve existing content").unwrap();
            }
        }
        let response = f.engine.handle(&Principal::Human, frozen_action(&proposal));
        assert!(response.failure.is_some(), "{change}: {response:?}");
        assert!(f.engine.take_effects().unwrap().is_empty());
        assert_eq!(f.engine.all("Project").len(), 1);
        if change == "collision" {
            assert_eq!(
                std::fs::read_to_string(target.join("keep.txt")).unwrap(),
                "Preserve existing content"
            );
        } else {
            assert!(!target.exists());
        }
    }
}

#[test]
fn project_creation_preference_must_be_captured_current_code_root_and_direct_parent() {
    for invalid in [
        "uncaptured",
        "stale",
        "wrong-key",
        "project-scope",
        "parent",
        "not-creating",
    ] {
        let mut f = Fixture::new();
        let mut preference = saved_project_root(&mut f);
        if invalid == "wrong-key" {
            preference["key"] = json!("workspace.active_project");
            preference = f.engine.put(preference);
        } else if invalid == "project-scope" {
            preference["scope"] = json!("Project");
            preference["projectId"] = json!(f.project);
            preference = f.engine.put(preference);
        }
        let (conversation, message, principal) = action_conversation(&mut f, "Start a new website");
        let mut params = new_project_params(&f, &preference);
        if invalid == "uncaptured" {
            let Principal::Invocation { invocation_id } = &principal else {
                unreachable!()
            };
            let mut invocation = f.engine.record(invocation_id, "Invocation").unwrap();
            invocation["coordinationInput"]["snapshot"]["preferences"]["items"] = json!([]);
            f.engine.put(invocation);
        } else if invalid == "stale" {
            preference["content"] = json!("The root changed.");
            f.engine.put(preference);
        } else if invalid == "parent" {
            std::fs::create_dir(f.root.join("elsewhere")).unwrap();
            params["root"] = json!(f.root.join("elsewhere").join("3d-human-website"));
        } else if invalid == "not-creating" {
            params["root"] = json!(f.root.to_string_lossy());
            params["createDirectory"] = json!(false);
        }
        let response = f.engine.handle(
            &principal,
            action_request(&conversation, &message, "project.configure", params, &[]),
        );
        assert!(response.failure.is_some(), "{invalid}: {response:?}");
        assert!(f.engine.all("HumanActionProposal").is_empty());
        assert!(f.engine.take_effects().unwrap().is_empty());
    }
}

#[test]
fn project_creation_accepts_explicit_human_path_without_memory() {
    let mut f = Fixture::new();
    let target = f.root.join("explicit-new-project");
    let (conversation, message, principal) = action_conversation(
        &mut f,
        &format!("Create my new project in \"{}\"", target.display()),
    );
    let mut params = proposed_project_params(&f);
    params["root"] = json!(target);
    params["createDirectory"] = json!(true);
    let response = f.engine.handle(
        &principal,
        action_request(&conversation, &message, "project.configure", params, &[]),
    );
    assert_eq!(response.status, "ok", "{response:?}");
    assert!(!target.exists());
}

#[test]
fn project_creation_failure_is_durable_and_wakes_the_original_conversation() {
    for code in ["EXECUTION_FAILED", "OUTCOME_UNKNOWN"] {
        let mut f = Fixture::new();
        let preference = saved_project_root(&mut f);
        let (conversation, message, principal) =
            action_conversation(&mut f, "Create a new website");
        let params = new_project_params(&f, &preference);
        let response = f.engine.handle(
            &principal,
            action_request(&conversation, &message, "project.configure", params, &[]),
        );
        assert_eq!(response.status, "ok", "{response:?}");
        let proposal = response.data.unwrap()["proposal"].clone();
        let Principal::Invocation { invocation_id } = &principal else {
            unreachable!()
        };
        let mut invocation = f.engine.record(invocation_id, "Invocation").unwrap();
        invocation["state"] = json!("Released");
        f.engine.put(invocation);
        let response = f.engine.handle(&Principal::Human, frozen_action(&proposal));
        assert_eq!(response.status, "pending", "{response:?}");
        let operation_id = response.operation_id.as_deref().unwrap();
        let pending = f.engine.take_effects().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].method, "project.create");
        let target = f.root.join("3d-human-website");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("partial.txt"), "Do not delete").unwrap();
        f.engine
            .complete_effect(
                operation_id,
                Response::fail("", code, "Recorded creation failure"),
            )
            .unwrap();
        assert!(f
            .engine
            .record(
                text(response.data.as_ref().unwrap(), "projectId"),
                "Project"
            )
            .is_err());
        let operation = f.engine.record(operation_id, "Operation").unwrap();
        assert_eq!(
            operation["status"],
            if code == "OUTCOME_UNKNOWN" {
                "RepairRequired"
            } else {
                "Failed"
            }
        );
        assert_eq!(operation["failure"]["code"], code);
        assert_eq!(
            std::fs::read_to_string(target.join("partial.txt")).unwrap(),
            "Do not delete"
        );
        let submitted = f
            .engine
            .record(text(&proposal, "id"), "HumanActionProposal")
            .unwrap();
        assert_eq!(submitted["completion"]["failure"]["code"], code);
        assert!(f
            .engine
            .all("CoordinationTrigger")
            .iter()
            .any(|trigger| trigger["reason"] == "HumanActionCompleted"
                && trigger["conversationId"] == conversation["id"]));
        let effects = f.engine.take_effects().unwrap();
        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0].method, "runtime.invoke");
        let input = &effects[0].params["invocation"]["coordinationInput"];
        assert_eq!(input["triggerEvents"][0]["kind"], "HumanActionCompleted");
        assert_eq!(input["scope"]["conversationId"], conversation["id"]);
        assert!(f.engine.all("Work").is_empty());
    }
}

#[test]
fn human_action_component_configures_first_project_only_after_human_confirmation() {
    let mut f = Fixture::new();
    f.engine
        .db
        .execute("DELETE FROM records WHERE id=?1", [&f.project])
        .unwrap();
    f.engine.records.remove(&f.project);
    f.project.clear();
    let root_message = format!("Use the existing directory \"{}\"", f.root.display());
    let (conversation, _, _) = action_conversation(&mut f, &root_message);
    let message = action_message(&mut f, &conversation, "Configure that project");
    let principal = action_invocation(&mut f, &conversation);
    let params = proposed_project_params(&f);
    let request = action_request(&conversation, &message, "project.configure", params, &[]);
    let response = f.engine.handle(&principal, request);
    assert_eq!(response.status, "ok", "{response:?}");
    let proposal = response.data.unwrap()["proposal"].clone();
    assert_eq!(proposal["status"], "Open");
    assert_eq!(
        proposal["preview"]["approvedModelDestination"],
        "fixture-approved-provider"
    );
    assert_eq!(
        proposal["preview"]["project"]["name"],
        "Human supplied project"
    );
    assert!(proposal.get("workId").is_none());
    assert!(f.engine.all("Project").is_empty());
    assert!(f.engine.take_effects().unwrap().is_empty());
    let frozen = frozen_action(&proposal);
    let denied = f.engine.handle(&principal, frozen.clone());
    assert_eq!(denied.failure.unwrap().code, "FORBIDDEN");
    assert!(f.engine.all("Project").is_empty());
    let response = f.engine.handle(&Principal::Human, frozen);
    assert_eq!(response.status, "ok", "{response:?}");
    assert_eq!(f.engine.all("Project").len(), 1);
    let submitted = f
        .engine
        .record(text(&proposal, "id"), "HumanActionProposal")
        .unwrap();
    assert_eq!(submitted["status"], "Submitted");
    assert_eq!(submitted["submission"]["status"], "ok");
    assert_eq!(
        submitted["submission"]["data"]["projectId"],
        response.data.unwrap()["projectId"]
    );
}

#[test]
fn human_action_component_project_proposal_cannot_expand_approved_policy() {
    let mut f = Fixture::new();
    let root_message = format!("Configure \"{}\"", f.root.display());
    let (conversation, message, principal) = action_conversation(&mut f, &root_message);
    let base = proposed_project_params(&f);
    let mut worker = base.clone();
    worker["workerCapabilityId"] = json!("unapproved-worker");
    let mut capabilities = base.clone();
    capabilities["capabilityIds"] = json!(["unapproved-capability"]);
    let mut environment = base.clone();
    environment["environmentRefs"] = json!(["unapproved-environment"]);
    let mut limit = base.clone();
    limit["limits"]["executionAttempts"] = json!(9);
    for params in [worker, capabilities, environment, limit] {
        let request = action_request(&conversation, &message, "project.configure", params, &[]);
        assert_eq!(
            f.engine.handle(&principal, request).failure.unwrap().code,
            "FORBIDDEN"
        );
    }
    let mut unknown = base.clone();
    unknown["approvedModelDestination"] = json!("unapproved-provider");
    let request = action_request(&conversation, &message, "project.configure", unknown, &[]);
    assert_eq!(
        f.engine.handle(&principal, request).failure.unwrap().code,
        "INVALID_ARGUMENT"
    );
    let project = f.engine.record(&f.project, "Project").unwrap();
    let request = action_request(
        &conversation,
        &message,
        "project.configure",
        base,
        &[project],
    );
    assert_eq!(
        f.engine.handle(&principal, request).failure.unwrap().code,
        "INVALID_ARGUMENT"
    );
    assert!(f.engine.all("HumanActionProposal").is_empty());
    assert_eq!(f.engine.all("Project").len(), 1);
}

#[test]
fn human_action_component_project_root_requires_literal_human_provenance() {
    let mut f = Fixture::new();
    let root_message = format!("Use \"{}-different-directory\"", f.root.display());
    let (conversation, message, principal) = action_conversation(&mut f, &root_message);
    let params = proposed_project_params(&f);
    let request = action_request(
        &conversation,
        &message,
        "project.configure",
        params.clone(),
        &[],
    );
    assert_eq!(
        f.engine.handle(&principal, request).failure.unwrap().code,
        "FORBIDDEN"
    );
    let assistant = f.engine.create(
        "ConversationItem",
        json!({"conversationId":conversation["id"],"role":"assistant",
            "text":format!("Configure \"{}\"", f.root.display())}),
    );
    f.engine.emit_message("MessageRecorded", &assistant);
    let principal = action_invocation(&mut f, &conversation);
    let request = action_request(
        &conversation,
        &message,
        "project.configure",
        params.clone(),
        &[],
    );
    assert_eq!(
        f.engine.handle(&principal, request).failure.unwrap().code,
        "FORBIDDEN"
    );
    let root_message = format!("Configure \"{}\"", f.root.display());
    let message = action_message(&mut f, &conversation, &root_message);
    let principal = action_invocation(&mut f, &conversation);
    let mut relative = params.clone();
    relative["root"] = json!(".");
    let request = action_request(&conversation, &message, "project.configure", relative, &[]);
    assert_eq!(
        f.engine.handle(&principal, request).failure.unwrap().code,
        "INVALID_ARGUMENT"
    );
    let mut missing = params;
    missing["root"] = json!(f.root.join("not-created").to_string_lossy());
    let request = action_request(&conversation, &message, "project.configure", missing, &[]);
    assert_eq!(
        f.engine.handle(&principal, request).failure.unwrap().code,
        "INVALID_ARGUMENT"
    );
    assert!(!f.root.join("not-created").exists());
}

#[test]
fn human_action_component_policy_preview_change_invalidates_frozen_configuration() {
    let mut f = Fixture::new();
    let root_message = format!("Configure \"{}\"", f.root.display());
    let (conversation, message, principal) = action_conversation(&mut f, &root_message);
    let request = action_request(
        &conversation,
        &message,
        "project.configure",
        proposed_project_params(&f),
        &[],
    );
    let response = f.engine.handle(&principal, request);
    assert_eq!(response.status, "ok", "{response:?}");
    let proposal = response.data.unwrap()["proposal"].clone();
    let mut policy = f.engine.global_policy().unwrap();
    policy["approvedModelDestination"] = json!("changed-provider");
    f.engine.put(policy);
    let response = f.engine.handle(&Principal::Human, frozen_action(&proposal));
    assert_eq!(response.failure.unwrap().code, "STALE_VERSION");
    assert_eq!(f.engine.all("Project").len(), 1);
    assert_eq!(
        f.engine
            .record(text(&proposal, "id"), "HumanActionProposal")
            .unwrap()["status"],
        "Open"
    );
}

fn action_change(f: &mut Fixture) -> (Value, Value, Value) {
    let draft = f.draft("Change only after confirming a replacement brief");
    f.start(&draft);
    let proposal = f.propose_change(&draft, false);
    let work = f.engine.record(text(&draft, "id"), "Work").unwrap();
    let grant = f.command(
        Principal::Human,
        "grant.preview",
        json!({"workId":work["id"],"specRevision":work["currentSpecRevision"],"policyRevision":1}),
        vec![work.clone()],
    );
    assert_eq!(grant.status, "ok", "{grant:?}");
    (work, proposal, grant.data.unwrap()["proposal"].clone())
}

#[test]
fn human_action_component_specification_change_is_frozen_without_execution() {
    let mut f = Fixture::new();
    let (work, change, grant) = action_change(&mut f);
    let (conversation, message, principal) =
        action_conversation(&mut f, "Apply the revised report brief");
    let request = action_request(
        &conversation,
        &message,
        "work.apply_change",
        json!({"proposalId":change["id"],"grantProposalId":grant["id"]}),
        &[work.clone(), change.clone()],
    );
    let operations = f.engine.all("Operation");
    let execution_grants = f.engine.all("ExecutionGrant");
    let response = f.engine.handle(&principal, request);
    assert_eq!(response.status, "ok", "{response:?}");
    let proposal = response.data.unwrap()["proposal"].clone();
    assert_eq!(proposal["preview"]["changeProposal"], change);
    assert_eq!(proposal["preview"]["grant"]["data"]["proposal"], grant);
    assert_eq!(f.engine.record(text(&work, "id"), "Work").unwrap(), work);
    assert_eq!(
        f.engine
            .record(text(&change, "id"), "ChangeProposal")
            .unwrap(),
        change
    );
    assert_eq!(f.engine.all("Operation"), operations);
    assert_eq!(f.engine.all("ExecutionGrant"), execution_grants);
    let denied = f.engine.handle(&principal, frozen_action(&proposal));
    assert_eq!(denied.failure.unwrap().code, "FORBIDDEN");
    let response = f.engine.handle(&Principal::Human, frozen_action(&proposal));
    assert_eq!(response.status, "pending", "{response:?}");
    let current = f.engine.record(text(&work, "id"), "Work").unwrap();
    assert_eq!(current["currentSpecRevision"], 2);
    assert_eq!(current["requiresReplan"], true);
    let proposal = f
        .engine
        .record(text(&proposal, "id"), "HumanActionProposal")
        .unwrap();
    assert_eq!(proposal["status"], "Submitted");
    assert_eq!(proposal["submission"]["status"], "pending");
}

#[test]
fn human_action_component_specification_change_rejects_guards_and_authority_expansion() {
    let mut f = Fixture::new();
    let (work, change, grant) = action_change(&mut f);
    let (conversation, message, principal) = action_conversation(&mut f, "Review the change");
    let params = json!({"proposalId":change["id"],"grantProposalId":grant["id"]});
    for guards in [
        vec![work.clone()],
        vec![change.clone()],
        vec![work.clone(), change.clone(), grant.clone()],
    ] {
        let request = action_request(
            &conversation,
            &message,
            "work.apply_change",
            params.clone(),
            &guards,
        );
        assert_eq!(
            f.engine.handle(&principal, request).failure.unwrap().code,
            "INVALID_ARGUMENT"
        );
    }
    let mut expanded = grant;
    expanded["limits"]["executionAttempts"] = json!(99);
    f.engine.put(expanded);
    let request = action_request(
        &conversation,
        &message,
        "work.apply_change",
        params,
        &[work.clone(), change],
    );
    assert_eq!(
        f.engine.handle(&principal, request).failure.unwrap().code,
        "FORBIDDEN"
    );
    assert_eq!(f.engine.record(text(&work, "id"), "Work").unwrap(), work);
    assert!(f.engine.all("HumanActionProposal").is_empty());
}

fn action_delivery(f: &mut Fixture) -> (Value, Value, Value, Value) {
    let work = f.draft("Deliver the inspected report");
    f.start(&work);
    let coordinator = f.coordinator(&work);
    let worker = f.plan(&work, &coordinator, false);
    f.start_invocation(&worker);
    f.acknowledge(&worker, None);
    let artifact = f.artifact(&work);
    let result = f.submit(&worker, &artifact, None);
    f.finish_release(&worker);
    let work = f.engine.record(text(&work, "id"), "Work").unwrap();
    let candidate = f
        .engine
        .record(text(&work, "currentCandidateId"), "DeliveryCandidate")
        .unwrap();
    let result = f.engine.record(text(&result, "id"), "TaskResult").unwrap();
    (work, candidate, result, artifact)
}

#[test]
fn human_action_component_delivery_acceptance_does_not_accept_before_human_confirmation() {
    let mut f = Fixture::new();
    let (work, candidate, result, _) = action_delivery(&mut f);
    let (conversation, message, principal) =
        action_conversation(&mut f, "Prepare to accept this delivery");
    let request = action_request(
        &conversation,
        &message,
        "delivery.accept",
        json!({"candidateId":candidate["id"]}),
        &[work.clone(), candidate.clone()],
    );
    let operations = f.engine.all("Operation");
    let response = f.engine.handle(&principal, request);
    assert_eq!(response.status, "ok", "{response:?}");
    let proposal = response.data.unwrap()["proposal"].clone();
    assert_eq!(proposal["preview"]["candidate"], candidate);
    assert_eq!(proposal["preview"]["result"]["result"], result);
    assert_eq!(proposal["preview"]["destination"], candidate["destination"]);
    assert!(!proposal["preview"]["artifacts"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(f.engine.record(text(&work, "id"), "Work").unwrap(), work);
    assert_eq!(
        f.engine
            .record(text(&candidate, "id"), "DeliveryCandidate")
            .unwrap(),
        candidate
    );
    assert!(f.engine.all("Acceptance").is_empty());
    assert_eq!(f.engine.all("Operation"), operations);
    let denied = f.engine.handle(&principal, frozen_action(&proposal));
    assert_eq!(denied.failure.unwrap().code, "FORBIDDEN");
    // Read the actual candidate and verify its guarded identity before the engine
    // confirmation. Native inspection/PendingConfirmation remains a UI test.
    let inspected = f.engine.handle(
        &Principal::Human,
        Request::new("delivery.get", json!({"candidateId":candidate["id"]})),
    );
    assert_eq!(inspected.status, "ok", "{inspected:?}");
    assert_eq!(inspected.data.unwrap(), candidate);
    let request = frozen_action(&proposal);
    assert_eq!(inspected.subjects, request.if_match);
    let response = f.engine.handle(&Principal::Human, request);
    assert_eq!(response.status, "ok", "{response:?}");
    assert_eq!(response.data.as_ref().unwrap()["phase"], "Completed");
    assert_eq!(f.engine.all("Acceptance").len(), 1);
    let proposal = f
        .engine
        .record(text(&proposal, "id"), "HumanActionProposal")
        .unwrap();
    assert_eq!(proposal["status"], "Submitted");
    assert_eq!(proposal["submission"]["data"]["phase"], "Completed");
}

#[test]
fn human_action_component_delivery_revision_does_not_reject_or_schedule_before_confirmation() {
    let mut f = Fixture::new();
    let (work, candidate, result, artifact) = action_delivery(&mut f);
    let (conversation, message, principal) =
        action_conversation(&mut f, "Revise the report explanation, then hold");
    let request = action_request(
        &conversation,
        &message,
        "delivery.request_changes",
        json!({
            "candidateId":candidate["id"],
            "findings":[{"criterionId":"report","requestedChange":"Explain the report boundary"}],
            "preserveArtifacts":[artifact],"advance":false
        }),
        &[work.clone(), candidate.clone()],
    );
    let operations = f.engine.all("Operation");
    let response = f.engine.handle(&principal, request);
    assert_eq!(response.status, "ok", "{response:?}");
    let proposal = response.data.unwrap()["proposal"].clone();
    assert_eq!(proposal["preview"]["requestedChanges"]["advance"], false);
    assert_eq!(f.engine.record(text(&work, "id"), "Work").unwrap(), work);
    assert_eq!(
        f.engine
            .record(text(&candidate, "id"), "DeliveryCandidate")
            .unwrap(),
        candidate
    );
    assert_eq!(
        f.engine.record(text(&result, "id"), "TaskResult").unwrap(),
        result
    );
    assert!(f.engine.all("ReworkInstruction").is_empty());
    assert_eq!(f.engine.all("Operation"), operations);
    let response = f.engine.handle(&Principal::Human, frozen_action(&proposal));
    assert_eq!(response.status, "ok", "{response:?}");
    assert_eq!(
        f.engine
            .record(text(&candidate, "id"), "DeliveryCandidate")
            .unwrap()["status"],
        "Rejected"
    );
    assert_eq!(
        f.engine.record(text(&result, "id"), "TaskResult").unwrap()["disposition"],
        "ChangesRequested"
    );
    assert_eq!(
        f.engine.record(text(&work, "id"), "Work").unwrap()["desiredAdvancement"],
        "Hold"
    );
    assert_eq!(f.engine.all("ReworkInstruction").len(), 1);
    assert!(f.engine.all("Acceptance").is_empty());
}

#[test]
fn human_action_component_delivery_preserves_exact_guards_and_criterion_bounds() {
    let mut f = Fixture::new();
    let (work, candidate, _, _) = action_delivery(&mut f);
    let (conversation, message, principal) = action_conversation(&mut f, "Review this delivery");
    for guards in [vec![work.clone()], vec![candidate.clone()]] {
        let request = action_request(
            &conversation,
            &message,
            "delivery.accept",
            json!({"candidateId":candidate["id"]}),
            &guards,
        );
        assert_eq!(
            f.engine.handle(&principal, request).failure.unwrap().code,
            "INVALID_ARGUMENT"
        );
    }
    for findings in [
        json!([]),
        json!([{"criterionId":"new-goal","requestedChange":"Expand the approved goal"}]),
        json!([{"criterionId":"report","requestedChange":" "}]),
    ] {
        let request = action_request(
            &conversation,
            &message,
            "delivery.request_changes",
            json!({"candidateId":candidate["id"],"findings":findings,
                "preserveArtifacts":[],"advance":true}),
            &[work.clone(), candidate.clone()],
        );
        assert_eq!(
            f.engine.handle(&principal, request).failure.unwrap().code,
            "INVALID_ARGUMENT"
        );
    }
    assert!(f.engine.all("HumanActionProposal").is_empty());
    assert!(f.engine.all("ReworkInstruction").is_empty());
    assert!(f.engine.all("Acceptance").is_empty());
}

#[test]
fn human_action_component_rejects_delivery_evidence_changed_after_proposal() {
    let mut f = Fixture::new();
    let (work, candidate, result, _) = action_delivery(&mut f);
    let (conversation, message, principal) = action_conversation(&mut f, "Inspect acceptance");
    let request = action_request(
        &conversation,
        &message,
        "delivery.accept",
        json!({"candidateId":candidate["id"]}),
        &[work.clone(), candidate],
    );
    let response = f.engine.handle(&principal, request);
    assert_eq!(response.status, "ok", "{response:?}");
    let proposal = response.data.unwrap()["proposal"].clone();
    let mut result = result;
    result["disposition"] = json!("Superseded");
    f.engine.put(result);
    let response = f.engine.handle(&Principal::Human, frozen_action(&proposal));
    assert_eq!(response.failure.unwrap().code, "BAD_STATE");
    assert!(f.engine.all("Acceptance").is_empty());
    assert_eq!(f.engine.record(text(&work, "id"), "Work").unwrap(), work);
    assert_eq!(
        f.engine
            .record(text(&proposal, "id"), "HumanActionProposal")
            .unwrap()["status"],
        "Open"
    );
}

fn action_question(f: &mut Fixture, conversation: &Value, source: &Value, schema: Value) -> Value {
    f.engine.create(
        "IntakeRequest",
        json!({
            "conversationId":conversation["id"],"messageId":source["id"],"turnId":id(),
            "question":"Which report details should be used?","responseSchema":schema,"status":"Open"
        }),
    )
}

fn resolve_input_request(question: &Value, message: &Value, value: Value) -> Request {
    let mut request = Request::new(
        "conversation.resolve_input",
        json!({"requestId":question["id"],"messageId":message["id"],"value":value}),
    );
    request.command_id = Some(id());
    request.if_match = vec![Engine::reference(question)];
    request
}

#[test]
fn human_action_component_global_clarification_uses_human_message_without_requeueing() {
    let mut f = Fixture::new();
    let (conversation, source, _) = action_conversation(&mut f, "Prepare a report");
    let question = action_question(
        &mut f,
        &conversation,
        &source,
        json!({"type":"string","minLength":1}),
    );
    let message = action_message(&mut f, &conversation, "A concise summary, please");
    let principal = action_invocation(&mut f, &conversation);
    let request = resolve_input_request(&question, &message, json!("Concise summary"));
    let invocations = f.engine.all("Invocation");
    let triggers = f.engine.all("CoordinationTrigger");
    let projects = f.engine.all("Project");
    let response = f.engine.handle(&principal, request.clone());
    assert_eq!(response.status, "ok", "{response:?}");
    let answered = f
        .engine
        .record(text(&question, "id"), "IntakeRequest")
        .unwrap();
    assert_eq!(answered["status"], "Answered");
    assert_eq!(answered["answer"], "Concise summary");
    assert_eq!(answered["answerMessageId"], message["id"]);
    assert_eq!(answered["answerSource"], "HumanMessageInterpretation");
    assert_eq!(f.engine.all("CoordinationTrigger"), triggers);
    assert_eq!(f.engine.all("Invocation"), invocations);
    assert_eq!(f.engine.all("Project"), projects);
    assert!(f.engine.all("Work").is_empty());
    assert!(f.engine.take_effects().unwrap().is_empty());
    let retry = f.engine.handle(&principal, request);
    assert_eq!(encode(&retry).unwrap(), encode(&response).unwrap());
    let replay = resolve_input_request(&answered, &message, json!("Replayed answer"));
    assert_eq!(
        f.engine.handle(&principal, replay).failure.unwrap().code,
        "STALE_VERSION"
    );
}

#[test]
fn human_action_component_global_clarification_rejects_uncaptured_foreign_and_old_sources() {
    let mut f = Fixture::new();
    let (conversation, source, original) = action_conversation(&mut f, "Prepare a report");
    let question = action_question(&mut f, &conversation, &source, json!({"type":"string"}));
    let message = action_message(&mut f, &conversation, "Short report");
    let principal = action_invocation(&mut f, &conversation);
    let old = resolve_input_request(&question, &source, json!("Old source"));
    assert_eq!(
        f.engine.handle(&principal, old).failure.unwrap().code,
        "STALE_VERSION"
    );
    let uncaptured_question = resolve_input_request(&question, &message, json!("Short"));
    assert_eq!(
        f.engine
            .handle(&original, uncaptured_question)
            .failure
            .unwrap()
            .code,
        "FORBIDDEN"
    );
    let (other, other_source, _) = action_conversation(&mut f, "Another conversation");
    let foreign_question = action_question(&mut f, &other, &other_source, json!({"type":"string"}));
    let foreign = resolve_input_request(&foreign_question, &message, json!("Foreign question"));
    assert_eq!(
        f.engine.handle(&principal, foreign).failure.unwrap().code,
        "FORBIDDEN"
    );
    let foreign = resolve_input_request(&question, &other_source, json!("Foreign answer"));
    assert_eq!(
        f.engine.handle(&principal, foreign).failure.unwrap().code,
        "FORBIDDEN"
    );
    let future = action_message(&mut f, &conversation, "Not in the captured turn");
    let future = resolve_input_request(&question, &future, json!("Future"));
    assert_eq!(
        f.engine.handle(&principal, future).failure.unwrap().code,
        "FORBIDDEN"
    );
    assert_eq!(
        f.engine
            .record(text(&question, "id"), "IntakeRequest")
            .unwrap(),
        question
    );
}

#[test]
fn human_action_component_global_clarification_cannot_reuse_its_question_source() {
    let mut f = Fixture::new();
    let (conversation, source, _) = action_conversation(&mut f, "Prepare a report");
    let question = action_question(&mut f, &conversation, &source, json!({"type":"string"}));
    let principal = action_invocation(&mut f, &conversation);
    let request = resolve_input_request(&question, &source, json!("No new human answer"));
    assert_eq!(
        f.engine.handle(&principal, request).failure.unwrap().code,
        "FORBIDDEN"
    );
    assert_eq!(
        f.engine
            .record(text(&question, "id"), "IntakeRequest")
            .unwrap()["status"],
        "Open"
    );
}

#[test]
fn human_action_component_global_clarification_preserves_schema_guards_and_intake_boundary() {
    let mut f = Fixture::new();
    let (conversation, source, _) = action_conversation(&mut f, "Prepare a report");
    let question = action_question(
        &mut f,
        &conversation,
        &source,
        json!({"type":"string","enum":["Short","Detailed"]}),
    );
    let message = action_message(&mut f, &conversation, "Short, please");
    let principal = action_invocation(&mut f, &conversation);
    for value in [json!(true), json!("Outside the schema")] {
        let request = resolve_input_request(&question, &message, value);
        assert_eq!(
            f.engine.handle(&principal, request).failure.unwrap().code,
            "INVALID_ARGUMENT"
        );
    }
    let mut missing_guard = resolve_input_request(&question, &message, json!("Short"));
    missing_guard.if_match.clear();
    assert_eq!(
        f.engine
            .handle(&principal, missing_guard)
            .failure
            .unwrap()
            .code,
        "INVALID_ARGUMENT"
    );
    let mut extra_guard = resolve_input_request(&question, &message, json!("Short"));
    extra_guard.if_match.push(Engine::reference(&conversation));
    assert_eq!(
        f.engine
            .handle(&principal, extra_guard)
            .failure
            .unwrap()
            .code,
        "INVALID_ARGUMENT"
    );
    let human = resolve_input_request(&question, &message, json!("Short"));
    assert_eq!(
        f.engine
            .handle(&Principal::Human, human)
            .failure
            .unwrap()
            .code,
        "FORBIDDEN"
    );
    let decision = f.engine.create(
        "DecisionRequest",
        json!({"conversationId":conversation["id"],"status":"Open"}),
    );
    let request = resolve_input_request(&decision, &message, json!("Short"));
    assert_eq!(
        f.engine.handle(&principal, request).failure.unwrap().code,
        "INVALID_REFERENCE"
    );
    assert_eq!(
        f.engine
            .record(text(&question, "id"), "IntakeRequest")
            .unwrap(),
        question
    );
}

#[test]
fn human_action_component_explicit_typed_intake_answer_can_ground_project_directory() {
    let mut f = Fixture::new();
    let (conversation, source, _) = action_conversation(&mut f, "Configure a new project");
    let question = action_question(
        &mut f,
        &conversation,
        &source,
        json!({"type":"object","properties":{"directory":{"type":"string"}},
            "required":["directory"],"additionalProperties":false}),
    );
    let response = f.command(
        Principal::Human,
        "conversation.answer_input",
        json!({"requestId":question["id"],"action":"Answer",
            "value":{"directory":f.root.to_string_lossy()}}),
        vec![question],
    );
    assert_eq!(response.status, "ok", "{response:?}");
    let principal = action_invocation(&mut f, &conversation);
    let request = action_request(
        &conversation,
        &source,
        "project.configure",
        proposed_project_params(&f),
        &[],
    );
    let response = f.engine.handle(&principal, request);
    assert_eq!(response.status, "ok", "{response:?}");
    assert_eq!(f.engine.all("Project").len(), 1);
    assert_eq!(response.data.unwrap()["proposal"]["status"], "Open");
}

#[test]
fn human_action_component_interpreted_directory_answer_is_not_new_human_authority() {
    let mut f = Fixture::new();
    let (conversation, source, _) = action_conversation(&mut f, "Configure a project");
    let question = action_question(&mut f, &conversation, &source, json!({"type":"string"}));
    let message = action_message(&mut f, &conversation, "The usual place");
    let principal = action_invocation(&mut f, &conversation);
    let request = resolve_input_request(&question, &message, json!(f.root.to_string_lossy()));
    let response = f.engine.handle(&principal, request);
    assert_eq!(response.status, "ok", "{response:?}");
    let principal = action_invocation(&mut f, &conversation);
    let request = action_request(
        &conversation,
        &message,
        "project.configure",
        proposed_project_params(&f),
        &[],
    );
    assert_eq!(
        f.engine.handle(&principal, request).failure.unwrap().code,
        "FORBIDDEN"
    );
    let root_message = format!("Use \"{}\"", f.root.display());
    let grounded = action_message(&mut f, &conversation, &root_message);
    let principal = action_invocation(&mut f, &conversation);
    let request = action_request(
        &conversation,
        &grounded,
        "project.configure",
        proposed_project_params(&f),
        &[],
    );
    let response = f.engine.handle(&principal, request);
    assert_eq!(response.status, "ok", "{response:?}");
    assert_eq!(f.engine.all("Project").len(), 1);
}

#[test]
fn human_action_component_server_intent_annotations_do_not_invalidate_human_source() {
    let mut f = Fixture::new();
    let work = f.draft("Preserve immutable human provenance");
    let (conversation, source, principal) = action_conversation(&mut f, "Hold this draft");
    let mut annotated = source.clone();
    annotated["intentIds"] = json!([id()]);
    let annotated = f.engine.put(annotated);
    f.engine.emit_message("MessageRecorded", &annotated);
    let proposal = propose_control(&mut f, &principal, &conversation, &source, &work, "Hold");
    assert_eq!(proposal["status"], "Open");
    let mut tampered = annotated;
    tampered["text"] = json!("Cancel it instead");
    let tampered = f.engine.put(tampered);
    f.engine.emit_message("MessageRecorded", &tampered);
    let request = action_request(
        &conversation,
        &source,
        "work.control",
        json!({"workId":work["id"],"action":"Cancel"}),
        &[work],
    );
    assert_eq!(
        f.engine.handle(&principal, request).failure.unwrap().code,
        "FORBIDDEN"
    );
}
