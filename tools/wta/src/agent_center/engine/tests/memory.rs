// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

use super::*;

fn store(f: &mut Fixture, content: &str, project: Option<&str>) -> Value {
    let mut input = json!({"key":"response.style","scope":"User","content":content});
    if let Some(project) = project {
        input["scope"] = json!("Project");
        input["projectId"] = json!(project);
    }
    let response = f.command(Principal::Human, "memory.store", input, vec![]);
    assert_eq!(response.status, "ok", "{response:?}");
    response.data.unwrap()
}

fn list(f: &mut Fixture, principal: &Principal, params: Value) -> Response {
    f.engine
        .handle(principal, Request::new("memory.list", params))
}

fn intake(f: &mut Fixture, content: &str, global: bool) -> (Value, String) {
    let conversation = id();
    let mut context = json!({
        "consoleSessionId":id(),"contextVersion":1,"projectId":f.project
    });
    if global {
        let response = f.command(Principal::Human, "console.configure", json!({
            "capabilityId":"fixture-agent","workerCapabilityId":"fixture-agent",
            "checkCapabilityId":"native-check","approvedModelDestination":"Local test",
            "limits":{"concurrency":1,"executionAttempts":8,"evaluationAttempts":16,
                "coordinationTurns":64,"contextRounds":3,"executionSeconds":60,"coordinationSeconds":60}
        }), vec![]);
        assert_eq!(response.status, "ok", "{response:?}");
        context["scope"] = json!("Global");
    }
    let response = f.command(
        Principal::Human,
        "conversation.submit",
        json!({
            "conversationId":conversation,"clientMessageId":id(),"text":content,
            "attachments":[],"context":context
        }),
        vec![],
    );
    assert_eq!(response.status, "ok", "{response:?}");
    let message = response.data.unwrap()["messageId"]
        .as_str()
        .unwrap()
        .to_owned();
    let invocation = f
        .engine
        .all("Invocation")
        .into_iter()
        .find(|invocation| {
            invocation["coordinationInput"]["scope"]["conversationId"] == conversation
        })
        .unwrap();
    f.start_invocation(&invocation);
    (invocation, message)
}

fn principal(invocation: &Value) -> Principal {
    Principal::Invocation {
        invocation_id: text(invocation, "id").into(),
    }
}

#[test]
fn memory_snapshot_keeps_code_root_provenance_when_other_preferences_fill_the_budget() {
    let mut f = Fixture::new();
    for index in 0..32 {
        f.engine.create(
            "Preference",
            json!({
                "key":format!("preference.{index}"),"scope":"User",
                "status":"Active","content":"a".repeat(512)
            }),
        );
    }
    let root = f.engine.create(
        "Preference",
        json!({
            "key":"workspace.code_root","scope":"User","status":"Active",
            "content":format!("Code root: \"{}\"", f.root.display())
        }),
    );
    let snapshot = f.engine.memory_snapshot(None);
    assert_eq!(snapshot["truncated"], true);
    assert_eq!(snapshot["items"][0], root);
    assert!(snapshot["items"].to_string().len() <= 8192);
}

#[test]
fn memory_persists_restarts_and_preserves_scopes() {
    let mut f = Fixture::new();
    let user = store(&mut f, "Prefer concise answers.", None);
    let project = f.project.clone();
    let local = store(&mut f, "Explain design tradeoffs here.", Some(&project));
    assert_eq!(
        list(&mut f, &Principal::Human, json!({})).data.unwrap()["items"],
        json!([user.clone()])
    );
    let scoped = list(&mut f, &Principal::Human, json!({"projectId":project}))
        .data
        .unwrap();
    assert_eq!(scoped["items"].as_array().unwrap().len(), 2);
    assert!(scoped["items"].as_array().unwrap().contains(&local));
    let replacement = Engine::open(&f.root.join("temporary-owner")).unwrap();
    drop(std::mem::replace(&mut f.engine, replacement));
    f.engine = Engine::open(&f.root).unwrap();
    assert_eq!(
        list(&mut f, &Principal::Human, json!({})).data.unwrap()["items"],
        json!([user])
    );
    let version: i64 = f
        .engine
        .db
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 1, "Preferences need no SQL schema migration");
}

#[test]
fn memory_store_is_idempotent_and_updates_require_exact_version() {
    let mut f = Fixture::new();
    let mut request = Request::new(
        "memory.store",
        json!({
            "key":"response.style","scope":"User","content":"Keep replies brief."
        }),
    );
    request.command_id = Some(id());
    let first = f
        .engine
        .handle(&Principal::Human, request.clone())
        .data
        .unwrap();
    assert_eq!(
        f.engine
            .handle(&Principal::Human, request.clone())
            .data
            .unwrap(),
        first
    );
    assert_eq!(f.engine.all("Preference").len(), 1);
    request.command_id = Some(id());
    assert_eq!(
        f.engine
            .handle(&Principal::Human, request.clone())
            .failure
            .unwrap()
            .code,
        "INVALID_ARGUMENT"
    );
    request.command_id = Some(id());
    request.if_match = vec![Engine::reference(&first)];
    request.params["content"] = json!("Include rationale for tradeoffs.");
    let updated = f
        .engine
        .handle(&Principal::Human, request.clone())
        .data
        .unwrap();
    assert_eq!(updated["version"], 2);
    request.command_id = Some(id());
    assert_eq!(
        f.engine
            .handle(&Principal::Human, request)
            .failure
            .unwrap()
            .code,
        "STALE_VERSION"
    );
    assert_eq!(
        f.engine.record(text(&first, "id"), "Preference").unwrap(),
        updated
    );
}

#[test]
fn memory_capture_is_bound_to_latest_human_intake_and_active_coordinator() {
    for global in [false, true] {
        let mut f = Fixture::new();
        let (invocation, message) =
            intake(&mut f, "Prefer concise replies across my work.", global);
        let input = json!({
            "key":"response.style","scope":"User","content":"Prefer concise replies.",
            "sourceMessageId":message
        });
        let caller = principal(&invocation);
        let response = f.command(caller.clone(), "memory.store", input.clone(), vec![]);
        assert_eq!(response.status, "ok", "{response:?}");
        let record = response.data.unwrap();
        assert_eq!(record["sourceMessageId"], message);
        let mut invalid = input.clone();
        invalid["sourceMessageId"] = invocation["replyMessageId"].clone();
        assert_eq!(
            f.command(
                caller.clone(),
                "memory.store",
                invalid,
                vec![record.clone()]
            )
            .failure
            .unwrap()
            .code,
            "FORBIDDEN"
        );
        let mut invalid = input.clone();
        invalid.as_object_mut().unwrap().remove("sourceMessageId");
        assert_eq!(
            f.command(
                caller.clone(),
                "memory.store",
                invalid,
                vec![record.clone()]
            )
            .failure
            .unwrap()
            .code,
            "INVALID_ARGUMENT"
        );
        let (other, foreign_source) = intake(&mut f, "Always use full explanations.", false);
        let mut invalid = input.clone();
        invalid["sourceMessageId"] = json!(foreign_source);
        assert_eq!(
            f.command(
                caller.clone(),
                "memory.store",
                invalid,
                vec![record.clone()]
            )
            .failure
            .unwrap()
            .code,
            "FORBIDDEN"
        );
        assert_eq!(list(&mut f, &principal(&other), json!({})).status, "ok");
        let mut ended = f
            .engine
            .record(text(&invocation, "id"), "Invocation")
            .unwrap();
        ended["state"] = json!("Released");
        f.engine.put(ended);
        assert_eq!(
            f.command(caller.clone(), "memory.store", input, vec![record])
                .failure
                .unwrap()
                .code,
            "FORBIDDEN"
        );
        assert_eq!(
            list(&mut f, &caller, json!({})).failure.unwrap().code,
            "FORBIDDEN"
        );
    }
}

#[test]
fn memory_forget_fences_old_invocations_and_removes_future_context() {
    let mut f = Fixture::new();
    let record = store(&mut f, "Prefer brief replies.", None);
    let (invocation, message) = intake(&mut f, "I prefer concise responses.", false);
    let forgotten = f.command(
        Principal::Human,
        "memory.forget",
        json!({
            "preferenceId":record["id"]
        }),
        vec![record.clone()],
    );
    assert_eq!(forgotten.status, "ok", "{forgotten:?}");
    let forgotten = forgotten.data.unwrap();
    assert!(forgotten.get("content").is_none());
    assert_eq!(f.engine.memory_snapshot(None)["items"], json!([]));
    let result = f.command(
        principal(&invocation),
        "memory.store",
        json!({
            "key":"different.key","scope":"User","content":"Prefer brief replies.",
            "sourceMessageId":message
        }),
        vec![],
    );
    assert_eq!(
        result.failure.unwrap().code,
        "STALE_VERSION",
        "Renaming must not evade the forget fence"
    );
    let (fresh, source) = intake(&mut f, "Remember again: I prefer concise responses.", false);
    assert_eq!(
        fresh["coordinationInput"]["snapshot"]["preferences"]["items"],
        json!([])
    );
    assert_eq!(
        f.command(
            principal(&fresh),
            "memory.store",
            json!({
                "key":"response.style","scope":"User","content":"Prefer brief replies.",
                "sourceMessageId":source
            }),
            vec![forgotten]
        )
        .status,
        "ok"
    );
}

#[test]
fn memory_can_forget_multiple_preferences_but_not_store_again_in_the_same_turn() {
    let mut f = Fixture::new();
    let first = store(&mut f, "Prefer brief replies.", None);
    let project = f.project.clone();
    let second = store(&mut f, "Prefer design explanations here.", Some(&project));
    let (invocation, message) = intake(&mut f, "Forget both response preferences.", false);
    for record in [first, second] {
        let response = f.command(
            principal(&invocation),
            "memory.forget",
            json!({
                "preferenceId":record["id"],"sourceMessageId":message
            }),
            vec![record],
        );
        assert_eq!(response.status, "ok", "{response:?}");
    }
    assert_eq!(f.engine.memory_snapshot(Some(&project))["items"], json!([]));
    assert_eq!(
        f.command(
            principal(&invocation),
            "memory.store",
            json!({
                "key":"different.key","scope":"User","content":"Keep replies short.",
                "sourceMessageId":message
            }),
            vec![]
        )
        .failure
        .unwrap()
        .code,
        "STALE_VERSION"
    );
}

#[test]
fn memory_newer_human_input_invalidates_old_capture_and_non_intake_cannot_harvest_history() {
    let mut f = Fixture::new();
    let (invocation, message) = intake(&mut f, "Prefer concise replies.", false);
    let mut caller = f
        .engine
        .record(text(&invocation, "id"), "Invocation")
        .unwrap();
    caller["coordinationInput"]["triggerEvents"] = json!([]);
    f.engine.put(caller);
    let input = json!({
        "key":"response.style","scope":"User","content":"Prefer concise replies.",
        "sourceMessageId":message
    });
    assert_eq!(
        f.command(
            principal(&invocation),
            "memory.store",
            input.clone(),
            vec![]
        )
        .failure
        .unwrap()
        .code,
        "FORBIDDEN"
    );
    let mut caller = f
        .engine
        .record(text(&invocation, "id"), "Invocation")
        .unwrap();
    caller["coordinationInput"]["triggerEvents"] =
        invocation["coordinationInput"]["triggerEvents"].clone();
    f.engine.put(caller);
    let source = f.engine.record(&message, "ConversationItem").unwrap();
    let response = f.command(Principal::Human, "conversation.submit", json!({
        "conversationId":source["conversationId"],"clientMessageId":id(),
        "text":"Actually, never save that preference.","attachments":[],"context":source["context"]
    }), vec![]);
    assert_eq!(response.status, "ok", "{response:?}");
    assert_eq!(
        f.command(principal(&invocation), "memory.store", input, vec![])
            .failure
            .unwrap()
            .code,
        "FORBIDDEN"
    );
    assert!(f.engine.all("Preference").is_empty());
}

#[test]
fn memory_project_access_is_checked_for_reads_writes_and_forgetting() {
    let mut f = Fixture::new();
    let (invocation, source) = intake(&mut f, "Prefer short replies.", true);
    let original = f.engine.record(&f.project, "Project").unwrap();
    let other = f.engine.create(
        "Project",
        json!({
            "name":"Not in captured authorization","root":original["root"],
            "coordinatorCapabilityId":"fixture-agent"
        }),
    );
    let record = store(&mut f, "Use a different style.", Some(text(&other, "id")));
    for global in [true, false] {
        let (caller, source) = if global {
            (principal(&invocation), source.clone())
        } else {
            let (local, source) = intake(&mut f, "Prefer short replies.", false);
            (principal(&local), source)
        };
        assert_eq!(
            list(&mut f, &caller, json!({"projectId":other["id"]}))
                .failure
                .unwrap()
                .code,
            "FORBIDDEN"
        );
        assert_eq!(
            f.command(
                caller.clone(),
                "memory.store",
                json!({
                    "key":"response.style","scope":"Project","projectId":other["id"],
                    "content":"Overwrite another project.","sourceMessageId":source
                }),
                vec![record.clone()]
            )
            .failure
            .unwrap()
            .code,
            "FORBIDDEN"
        );
        assert_eq!(
            f.command(
                caller,
                "memory.forget",
                json!({
                    "preferenceId":record["id"],"sourceMessageId":source
                }),
                vec![record.clone()]
            )
            .failure
            .unwrap()
            .code,
            "FORBIDDEN"
        );
    }
    assert_eq!(
        list(&mut f, &Principal::Runtime { runtime_id: id() }, json!({}))
            .failure
            .unwrap()
            .code,
        "FORBIDDEN"
    );
}

#[test]
fn memory_failed_receipt_rolls_back_forgetting_and_its_fence() {
    let mut f = Fixture::new();
    let record = store(&mut f, "Prefer concise responses.", None);
    let snapshot = f.engine.memory_snapshot(None);
    f.engine
        .db
        .execute_batch(
            "CREATE TRIGGER reject_memory_receipt BEFORE INSERT ON commands
         BEGIN SELECT RAISE(FAIL,'forced memory receipt failure'); END;",
        )
        .unwrap();
    let response = f.command(
        Principal::Human,
        "memory.forget",
        json!({
            "preferenceId":record["id"]
        }),
        vec![record],
    );
    assert_eq!(response.failure.unwrap().code, "EXECUTION_FAILED");
    assert_eq!(f.engine.memory_snapshot(None), snapshot);
    f.engine
        .db
        .execute_batch("DROP TRIGGER reject_memory_receipt")
        .unwrap();
}

#[test]
fn memory_snapshots_cover_global_intake_and_work_without_worker_access() {
    let mut f = Fixture::new();
    let user = store(&mut f, "Be concise.", None);
    let project = f.project.clone();
    let project_preference = store(
        &mut f,
        "Explain decisions for this project.",
        Some(&project),
    );
    for global in [false, true] {
        let (invocation, _) = intake(&mut f, "Help me.", global);
        let items = &invocation["coordinationInput"]["snapshot"]["preferences"]["items"];
        assert!(items.as_array().unwrap().contains(&user));
        assert!(items.as_array().unwrap().contains(&project_preference));
        for tool in ["memory_list", "memory_store", "memory_forget"] {
            assert!(values(&invocation, "availableToolNames").contains(&json!(tool)));
        }
    }
    let work = f.draft("Exercise coordinator preference context");
    f.start(&work);
    let coordinator = f.coordinator(&work);
    assert!(values(
        &coordinator["coordinationInput"]["snapshot"]["preferences"],
        "items"
    )
    .contains(&user));
    assert!(values(&coordinator, "availableToolNames").contains(&json!("memory_list")));
    assert!(!values(&coordinator, "availableToolNames").contains(&json!("memory_store")));
    assert_eq!(
        list(
            &mut f,
            &principal(&coordinator),
            json!({"projectId":project})
        )
        .status,
        "ok"
    );
    let worker = f.plan(&work, &coordinator, false);
    assert!(!values(&worker, "availableToolNames").contains(&json!("memory_list")));
    assert_eq!(
        list(&mut f, &principal(&worker), json!({}))
            .failure
            .unwrap()
            .code,
        "FORBIDDEN"
    );
    assert_eq!(
        f.command(
            principal(&worker),
            "memory.forget",
            json!({
                "preferenceId":user["id"]
            }),
            vec![user]
        )
        .failure
        .unwrap()
        .code,
        "FORBIDDEN"
    );
}

#[test]
fn memory_snapshot_budget_and_list_pagination_are_exact() {
    let mut f = Fixture::new();
    for index in 0..30 {
        let response = f.command(
            Principal::Human,
            "memory.store",
            json!({
                "key":format!("workflow.{index}"),"scope":"User",
                "content":"\"\\\n".repeat(170)
            }),
            vec![],
        );
        assert_eq!(response.status, "ok", "{response:?}");
    }
    let snapshot = f.engine.memory_snapshot(None);
    assert_eq!(snapshot["truncated"], true);
    assert!(snapshot["items"].to_string().len() <= 8192);
    let mut params = json!({"limit":7});
    let mut all = Vec::new();
    loop {
        let page = list(&mut f, &Principal::Human, params.clone())
            .data
            .unwrap();
        all.extend(values(&page, "items"));
        if let Some(next) = page.get("nextAfterId") {
            params["afterId"] = next.clone();
        } else {
            break;
        }
    }
    assert_eq!(all.len(), 30);
    assert_eq!(
        all.iter()
            .map(|item| text(item, "id"))
            .collect::<BTreeSet<_>>()
            .len(),
        30
    );
    for params in [
        json!({"limit":0}),
        json!({"limit":101}),
        json!({"afterId":"bad"}),
        json!({"unknown":true}),
    ] {
        assert_eq!(
            list(&mut f, &Principal::Human, params)
                .failure
                .unwrap()
                .code,
            "INVALID_ARGUMENT"
        );
    }
}

#[test]
fn memory_rejects_invalid_shapes_and_unknown_fields() {
    let mut f = Fixture::new();
    for params in [
        json!({"key":"UPPER","scope":"User","content":"Keep it brief."}),
        json!({"key":"style","scope":"User","content":""}),
        json!({"key":"style","scope":"User","content":"x".repeat(513)}),
        json!({"key":"style","scope":"Project","content":"Keep it brief."}),
        json!({"key":"style","scope":"User","content":"Keep it brief.","projectId":f.project}),
        json!({"key":"style","scope":"User","content":"Keep it brief.","sourceMessageId":null}),
        json!({"key":"style","scope":"User","content":"Keep it brief.","extra":true}),
    ] {
        assert_eq!(
            f.command(Principal::Human, "memory.store", params, vec![])
                .failure
                .unwrap()
                .code,
            "INVALID_ARGUMENT"
        );
    }
    assert!(f.engine.all("Preference").is_empty());
}
