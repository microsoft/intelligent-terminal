// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

use super::*;

#[test]
fn conversation_snapshots_and_replay_include_only_their_own_intake_questions() {
    let mut f = Fixture::new();
    let mut questions = Vec::new();
    for label in ["first", "second"] {
        let conversation = id();
        let console = id();
        let opened = f.command(
            Principal::Human,
            "console.open",
            json!({"consoleSessionId":console,"projectId":f.project,"conversationId":conversation}),
            vec![],
        );
        assert_eq!(opened.status, "ok", "{opened:?}");
        let submitted = f.command(
            Principal::Human,
            "conversation.submit",
            json!({
                "conversationId":conversation,"clientMessageId":id(),
                "text":format!("Clarify the {label} goal"),"attachments":[],
                "context":{"consoleSessionId":console,"contextVersion":1,"projectId":f.project}
            }),
            vec![],
        );
        assert_eq!(submitted.status, "ok", "{submitted:?}");
        let data = submitted.data.unwrap();
        let invocation = f
            .engine
            .all("Invocation")
            .into_iter()
            .find(|invocation| invocation["subject"]["id"] == data["intakeTurnId"])
            .unwrap();
        f.start_invocation(&invocation);
        let cursor = f.engine.cursor();
        let requested = f.command(
            Principal::Invocation {
                invocation_id: text(&invocation, "id").into(),
            },
            "conversation.request_input",
            json!({
                "turnId":data["intakeTurnId"],"conversationId":conversation,
                "messageId":data["messageId"],"question":format!("Use the {label} location?"),
                "responseSchema":{"type":"boolean"}
            }),
            vec![],
        );
        assert_eq!(requested.status, "needs_input", "{requested:?}");
        let question = f
            .engine
            .record(&requested.input_request.unwrap().id, "IntakeRequest")
            .unwrap();
        questions.push((conversation, cursor, question));
    }

    for (index, (conversation, cursor, question)) in questions.iter().enumerate() {
        let scope = json!({"kind":"Conversation","id":conversation});
        let snapshot = f.engine.snapshot(&scope).unwrap();
        assert_eq!(snapshot["id"], json!(conversation));
        assert!(snapshot["messages"].is_array());
        assert_eq!(snapshot["intakeRequests"], json!([question]));
        let events = f.engine.events_after(Some(cursor), &scope).unwrap();
        let intake = events
            .iter()
            .filter(|event| text(&event["subject"], "kind") == "IntakeRequest")
            .collect::<Vec<_>>();
        assert_eq!(intake.len(), 1);
        assert_eq!(intake[0]["kind"], "IntakeRequested");
        assert_eq!(intake[0]["changes"][0]["view"], *question);

        let (action, params, expected_state, expected_event) = if index == 0 {
            (
                "Answer",
                json!({"requestId":question["id"],"action":"Answer","value":true}),
                "Answered",
                "IntakeAnswered",
            )
        } else {
            (
                "Cancel",
                json!({"requestId":question["id"],"action":"Cancel"}),
                "Cancelled",
                "IntakeCancelled",
            )
        };
        let before_answer = f.engine.cursor();
        let response = f.command(
            Principal::Human,
            "conversation.answer_input",
            params.clone(),
            vec![question.clone()],
        );
        assert_eq!(response.status, "ok", "{action}: {response:?}");
        let after = f.engine.snapshot(&scope).unwrap();
        assert_eq!(after["intakeRequests"][0]["status"], expected_state);
        if action == "Answer" {
            assert_eq!(after["intakeRequests"][0]["answer"], true);
        } else {
            assert!(after["intakeRequests"][0].get("answer").is_none());
        }
        let events = f.engine.events_after(Some(&before_answer), &scope).unwrap();
        assert!(events.iter().any(|event| {
            event["kind"] == expected_event
                && event["subject"]["id"] == question["id"]
                && event["changes"][0]["view"]["status"] == expected_state
        }));
        let repeated = f.command(
            Principal::Human,
            "conversation.answer_input",
            params,
            vec![question.clone()],
        );
        assert_ne!(repeated.status, "ok");
        let foreign = f
            .engine
            .events_after(
                Some(&before_answer),
                &json!({"kind":"Conversation","id":questions[1 - index].0}),
            )
            .unwrap();
        assert!(!foreign.iter().any(|event| {
            text(&event["subject"], "kind") == "IntakeRequest"
                && event["subject"]["id"] == question["id"]
        }));
    }
    assert!(f.engine.all("Work").is_empty());
    assert!(!f
        .engine
        .events_after(None, &json!({"kind":"WorkList"}))
        .unwrap()
        .iter()
        .any(|event| text(&event["subject"], "kind") == "IntakeRequest"));
}

fn inspect(f: &mut Fixture, principal: Principal, params: Value) -> Response {
    f.engine
        .handle(&principal, Request::new("workspace.inspect", params))
}

#[test]
fn inspection_before_a_candidate_exposes_workspace_without_transferring_authority() {
    let mut f = Fixture::new();
    let work = f.draft("Inspect before the first submission");
    f.start(&work);
    let current = f.engine.record(text(&work, "id"), "Work").unwrap();
    let workspace = f
        .engine
        .record(text(&current, "workspaceId"), "Workspace")
        .unwrap();
    let cursor = f.engine.cursor();
    let response = inspect(&mut f, Principal::Human, json!({"workId":work["id"]}));
    assert_eq!(response.status, "ok", "{response:?}");
    let data = response.data.unwrap();
    assert_eq!(data["workspace"], workspace);
    assert_eq!(data["destination"]["localRoot"], workspace["localRoot"]);
    assert_eq!(data["artifacts"], json!([]));
    assert_eq!(data["results"], json!([]));
    assert!(data.get("candidateId").is_none());
    assert_eq!(f.engine.cursor(), cursor);
    assert_eq!(
        f.engine
            .record(text(&workspace, "id"), "Workspace")
            .unwrap(),
        workspace
    );
    assert_eq!(f.engine.record(text(&work, "id"), "Work").unwrap(), current);
}

#[test]
fn inspection_shows_exact_submitted_output_before_delivery_and_keeps_candidate_path() {
    let mut f = Fixture::new();
    let work = f.draft("Inspect intermediate report");
    f.start(&work);
    let coordinator = f.coordinator(&work);
    let worker = f.plan(&work, &coordinator, false);
    f.start_invocation(&worker);
    f.acknowledge(&worker, None);
    let artifact = f.artifact(&work);
    let result = f.submit(&worker, &artifact, None);

    let response = inspect(&mut f, Principal::Human, json!({"workId":work["id"]}));
    assert_eq!(response.status, "ok", "{response:?}");
    let data = response.data.unwrap();
    assert!(data.get("candidateId").is_none());
    assert_eq!(data["artifacts"], json!([artifact]));
    assert_eq!(data["results"][0]["resultId"], result["id"]);
    assert_eq!(data["results"][0]["disposition"], "Submitted");

    f.finish_release(&worker);
    let current = f.engine.record(text(&work, "id"), "Work").unwrap();
    let candidate = f
        .engine
        .record(text(&current, "currentCandidateId"), "DeliveryCandidate")
        .unwrap();
    let response = inspect(
        &mut f,
        Principal::Human,
        json!({"workId":work["id"],"candidateId":candidate["id"]}),
    );
    assert_eq!(response.status, "ok", "{response:?}");
    let data = response.data.unwrap();
    assert_eq!(data["candidateId"], candidate["id"]);
    assert_eq!(data["artifacts"], candidate["artifacts"]);
    assert_eq!(data["destination"], candidate["destination"]);

    let other = f.draft("Another candidate owner");
    f.start(&other);
    let response = inspect(
        &mut f,
        Principal::Human,
        json!({"workId":other["id"],"candidateId":candidate["id"]}),
    );
    assert_eq!(response.failure.unwrap().code, "INVALID_REFERENCE");
}

#[test]
fn inspection_rejects_missing_workspace_explicit_bad_candidate_and_cross_work_binding() {
    let mut f = Fixture::new();
    let first = f.draft("First work");
    let second = f.draft("Second work");
    let response = inspect(&mut f, Principal::Human, json!({"workId":first["id"]}));
    assert_eq!(response.failure.unwrap().code, "BAD_STATE");
    f.start(&first);
    f.start(&second);
    let coordinator = f.coordinator(&first);
    let response = inspect(
        &mut f,
        Principal::Invocation {
            invocation_id: text(&coordinator, "id").into(),
        },
        json!({"workId":second["id"]}),
    );
    assert_eq!(response.failure.unwrap().code, "FORBIDDEN");
    let response = inspect(
        &mut f,
        Principal::Human,
        json!({"workId":first["id"],"candidateId":id()}),
    );
    assert_ne!(response.status, "ok");
    let response = inspect(
        &mut f,
        Principal::Human,
        json!({"workId":first["id"],"candidateId":""}),
    );
    assert_ne!(response.status, "ok");
}
