// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

use super::*;

#[path = "tests/actions.rs"]
mod actions;
#[path = "tests/continuation.rs"]
mod continuation;
#[path = "tests/executor.rs"]
mod executor;
#[path = "tests/global.rs"]
mod global;
#[path = "tests/inspection_regressions.rs"]
mod inspection_regressions;
#[path = "tests/memory.rs"]
mod memory;

struct Fixture {
    engine: Engine,
    root: TestRoot,
    project: String,
    runtime: String,
}

struct TestRoot(std::path::PathBuf);

impl std::ops::Deref for TestRoot {
    type Target = std::path::PathBuf;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        fn writable(path: &Path) -> std::io::Result<()> {
            for entry in std::fs::read_dir(path)? {
                let entry = entry?;
                if entry.file_type()?.is_dir() {
                    writable(&entry.path())?;
                } else {
                    let mut permissions = entry.metadata()?.permissions();
                    #[allow(clippy::permissions_set_readonly_false)]
                    permissions.set_readonly(false);
                    std::fs::set_permissions(entry.path(), permissions)?;
                }
            }
            Ok(())
        }
        if let Err(error) = writable(&self.0) {
            eprintln!("Could not reset capture permissions for test cleanup: {error}");
        }
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            eprintln!(
                "Could not remove engine test directory {}: {error}",
                self.0.display()
            );
        }
    }
}

#[test]
fn streaming_observations_do_not_rewrite_unrelated_history() {
    let mut f = Fixture::new();
    let work = f.draft("Stream without rewriting historical records");
    f.start(&work);
    let invocation = f.coordinator(&work);
    f.engine.db.execute_batch("BEGIN IMMEDIATE").unwrap();
    for _ in 0..2000 {
        f.engine.create("RuntimeObservation", json!({
            "invocationId":id(),"body":{"kind":"TextDelta","data":{"text":"retained historical text"}}
        }));
    }
    f.engine.persist().unwrap();
    f.engine.db.execute_batch("COMMIT").unwrap();
    let before = f.engine.db.total_changes();
    f.report(
        &invocation,
        "TextDelta",
        json!({
            "messageId":invocation["replyMessageId"],"partId":"turn-1","chunkIndex":0,
            "text":"A new response"
        }),
    );
    let writes = f.engine.db.total_changes() - before;
    assert!(
        writes < 20,
        "A text chunk wrote {writes} rows; unchanged history must not be rewritten"
    );
    let stored: String = f
        .engine
        .db
        .query_row(
            "SELECT body FROM records WHERE id=?1",
            [text(&invocation, "replyMessageId")],
            |row| row.get(0),
        )
        .unwrap();
    let stored: Value = serde_json::from_str(&stored).unwrap();
    assert_eq!(stored["parts"][0]["text"], "A new response");
    assert_eq!(f.engine.all("RuntimeObservation").len(), 2002);
}

#[test]
fn bootstrap_requires_registered_capabilities_and_fresh_runtime_after_restart() {
    let mut f = Fixture::new();
    let params = json!({
        "name":"Unconfigured provider","root":f.root.to_string_lossy(),
        "coordinatorCapabilityId":"missing-provider","workerCapabilityId":"fixture-agent",
        "checkCapabilityId":"native-check",
        "limits":{"concurrency":1,"executionAttempts":2,"evaluationAttempts":2,
            "coordinationTurns":2,"contextRounds":1,"executionSeconds":60,"coordinationSeconds":60}
    });
    let response = f.command(Principal::Human, "project.configure", params, vec![]);
    assert_eq!(response.failure.unwrap().code, "CAPABILITY_UNAVAILABLE");
    assert_eq!(f.engine.all("Project").len(), 1);
    assert!(f.engine.take_effects().unwrap().is_empty());
    let replacement_root = f.root.join("replace-bootstrap-owner");
    let old = std::mem::replace(&mut f.engine, Engine::open(&replacement_root).unwrap());
    drop(old);
    f.engine = Engine::open(&f.root).unwrap();
    assert!(f
        .engine
        .runtime_for("fixture-agent", "Coordinate")
        .is_none());
    assert_eq!(
        f.engine.record(&f.runtime, "Runtime").unwrap()["status"],
        "Disconnected"
    );
    let fresh_runtime = id();
    let response = f.command(Principal::Runtime {runtime_id:fresh_runtime.clone()}, "runtime.register",
        json!({"runtimeInstanceId":fresh_runtime,"protocolVersions":[1],"capabilities":[
            {"id":"fixture-agent","kinds":["Coordinate"],"supportsContinuation":true,"supportsScopedStop":true}
        ]}), vec![]);
    assert_eq!(response.status, "ok", "{response:?}");
    assert_eq!(
        f.engine.runtime_for("fixture-agent", "Coordinate").unwrap()["id"],
        fresh_runtime
    );
    assert!(f.engine.take_effects().unwrap().is_empty());
}

#[test]
fn bound_reads_omit_command_ids_and_use_shared_classification() {
    let mut f = Fixture::new();
    let work = f.draft("Read the assigned task and immutable artifacts");
    f.start(&work);
    let coordinator = f.coordinator(&work);
    let worker = f.plan(&work, &coordinator, false);
    let artifact = f.artifact(&work);
    let principal = Principal::Invocation {
        invocation_id: text(&worker, "id").into(),
    };
    for (method, params) in [
        ("task.get", json!({"taskId":worker["dispatch"]["taskId"]})),
        ("artifact.get", json!({"artifactId":artifact["artifactId"]})),
    ] {
        assert!(is_read_method(method));
        let mut request = Request::new(method, params);
        let response = f.engine.handle(&principal, request.clone());
        assert_eq!(response.status, "ok", "{response:?}");
        request.command_id = Some(id());
        let response = f.engine.handle(&principal, request);
        assert_eq!(response.failure.unwrap().code, "INVALID_ARGUMENT");
    }
    for method in [
        "result.submit",
        "task.acknowledge",
        "artifact.capture",
        "unknown.get",
    ] {
        assert!(!is_read_method(method));
    }
}

#[test]
fn failed_receipt_persistence_restores_committed_cursor_and_records() {
    let mut f = Fixture::new();
    let cursor = f.engine.cursor();
    let position = f.engine.position;
    f.engine
        .db
        .execute_batch(
            "CREATE TRIGGER reject_receipts BEFORE INSERT ON commands
         BEGIN SELECT RAISE(FAIL,'forced receipt persistence failure'); END;",
        )
        .unwrap();
    let response = f.command(
        Principal::Human,
        "work.create_draft",
        json!({
            "projectId":f.project,"goal":"Must not become partially committed",
            "scope":["reports"],"exclusions":[],
            "criteria":[{"id":"report","description":"Report","evidenceRule":"artifact:report"}],
            "context":[],"delivery":{"kind":"Report"},"sourceMessageIds":[]
        }),
        vec![],
    );
    assert_eq!(response.failure.unwrap().code, "EXECUTION_FAILED");
    assert_eq!(f.engine.cursor(), cursor);
    assert!(f.engine.all("Work").is_empty());
    let stored: i64 = f
        .engine
        .db
        .query_row("SELECT COALESCE(MAX(sequence),0) FROM events", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(u64::try_from(stored).unwrap(), position);
    f.engine
        .db
        .execute_batch("DROP TRIGGER reject_receipts")
        .unwrap();
    f.draft("A later successful command advances the committed cursor");
    assert!(f.engine.position > position);
    assert_eq!(f.engine.all("Work").len(), 1);
}

impl Fixture {
    fn runtime_only() -> Self {
        let root = std::env::current_dir()
            .unwrap()
            .join("target")
            .join("agent-center-engine-tests")
            .join(id());
        std::fs::create_dir_all(&root).unwrap();
        let engine = Engine::open(&root).unwrap();
        let mut fixture = Self {
            engine,
            root: TestRoot(root),
            project: String::new(),
            runtime: id(),
        };
        let response=fixture.command(Principal::Runtime{runtime_id:fixture.runtime.clone()},"runtime.register",json!({
            "runtimeInstanceId":fixture.runtime,"protocolVersions":[1],
            "capabilities":[
                {"id":"fixture-agent","kinds":["ProduceResult","ReviewResult","Coordinate","ExecuteWork"],"supportsContinuation":true,"supportsScopedStop":true},
                {"id":"native-check","kinds":["EvaluateGate"],"supportsContinuation":false,"supportsScopedStop":true}
            ]
        }),vec![]);
        assert_eq!(response.status, "ok", "{response:?}");
        fixture
    }

    fn new() -> Self {
        let mut fixture = Self::runtime_only();
        let response=fixture.command(Principal::Human,"project.configure",json!({
            "name":"Fixture","root":fixture.root.to_string_lossy(),"coordinatorCapabilityId":"fixture-agent",
            "workerCapabilityId":"fixture-agent","checkCapabilityId":"native-check",
            "limits":{"concurrency":4,"executionAttempts":8,"evaluationAttempts":16,"coordinationTurns":12,
                "contextRounds":3,"executionSeconds":60,"coordinationSeconds":60}
        }),vec![]);
        assert_eq!(response.status, "ok", "{response:?}");
        fixture.project = response.data.unwrap()["projectId"].as_str().unwrap().into();
        fixture
    }

    fn command(
        &mut self,
        principal: Principal,
        method: &str,
        params: Value,
        subjects: Vec<Value>,
    ) -> Response {
        let mut request = Request::new(method, params);
        request.command_id = Some(id());
        request.if_match = subjects.iter().map(Engine::reference).collect();
        self.engine.handle(&principal, request)
    }

    fn draft(&mut self, goal: &str) -> Value {
        let response=self.command(Principal::Human,"work.create_draft",json!({
            "executionMode":"LegacyTasks","projectId":self.project,"goal":goal,"scope":["reports"],"exclusions":[],
            "criteria":[{"id":"report","description":"Readable report","evidenceRule":"artifact:report"}],
            "context":[],"delivery":{"kind":"Report"},"sourceMessageIds":[]
        }),vec![]);
        assert_eq!(response.status, "ok", "{response:?}");
        self.engine
            .record(response.data.unwrap()["workId"].as_str().unwrap(), "Work")
            .unwrap()
    }

    fn start(&mut self, work: &Value) {
        let response = self.command(
            Principal::Human,
            "grant.preview",
            json!({"workId":work["id"],"specRevision":1,"policyRevision":1}),
            vec![work.clone()],
        );
        assert_eq!(response.status, "ok", "{response:?}");
        let proposal = response.data.unwrap()["grantProposalId"].clone();
        let response=self.command(Principal::Human,"work.start",json!({"workId":work["id"],"specRevision":1,"projectPolicyRevision":1,"grantProposalId":proposal}),vec![work.clone()]);
        assert_eq!(response.status, "pending", "{response:?}");
        self.complete_nonruntime();
    }

    fn complete_nonruntime(&mut self) {
        let operations = self.engine.all("Operation");
        for operation in operations {
            if text(&operation, "status") != "Pending" {
                continue;
            }
            let method = text(&operation["effect"], "method");
            if method == "workspace.provision" {
                self.engine.complete_effect(text(&operation,"id"),Response::ok(id(),json!({
                    "workspaceId":operation["effect"]["params"]["workspaceId"],"localRoot":self.root.to_string_lossy()
                }))).unwrap();
            }
        }
    }

    fn coordinator(&mut self, work: &Value) -> Value {
        let invocation = self
            .engine
            .related("Invocation", "workId", text(work, "id"))
            .into_iter()
            .find(|v| {
                text(&v["subject"], "kind") == "Coordination" && text(v, "state") == "Dispatching"
            })
            .unwrap();
        self.start_invocation(&invocation);
        self.engine
            .record(text(&invocation, "id"), "Invocation")
            .unwrap()
    }

    fn start_invocation(&mut self, invocation: &Value) {
        let operation = self
            .engine
            .all("Operation")
            .into_iter()
            .find(|o| {
                text(&o["effect"], "method") == "runtime.invoke"
                    && o["effect"]["params"]["invocation"]["id"] == invocation["id"]
            })
            .unwrap();
        self.engine
            .complete_effect(
                text(&operation, "id"),
                Response::ok(
                    id(),
                    json!({"invocationId":invocation["id"],"disposition":"Recorded"}),
                ),
            )
            .unwrap();
        let command = text(&invocation["dispatch"], "kind") == "EvaluateGate";
        let mut data = json!({"adapterKind":if command{"Command"}else{"ACP"},"executionIdentity":format!("process-{}",text(invocation,"id"))});
        if !command {
            data["providerSessionId"] = json!(format!("session-{}", text(invocation, "id")));
            data["providerConfigurationDigest"] = json!("fixture-provider-configuration");
            data["sessionCwd"] = json!(self.root.to_string_lossy());
            data["sessionLoaded"] = json!(false);
            if let Some(reference) = invocation.get("sessionReuseRef").and_then(Value::as_str) {
                let previous = self.engine.record(reference, "Invocation").unwrap();
                data["providerSessionId"] = previous["providerSessionId"].clone();
                data["sessionLoaded"] = json!(true);
            }
        }
        self.report(invocation, "Started", data);
    }

    fn report(&mut self, invocation: &Value, kind: &str, data: Value) -> Response {
        let current = self
            .engine
            .record(text(invocation, "id"), "Invocation")
            .unwrap();
        let response=self.command(Principal::Runtime{runtime_id:self.runtime.clone()},"runtime.report",json!({
            "observationId":id(),"invocationId":invocation["id"],"bindingGeneration":1,"sequence":number(&current,"lastSequence")+1,
            "kind":kind,"data":data
        }),vec![]);
        assert_eq!(response.status, "ok", "{response:?}");
        response
    }

    fn finish_release(&mut self, invocation: &Value) {
        let current = self
            .engine
            .record(text(invocation, "id"), "Invocation")
            .unwrap();
        self.report(invocation,"TurnEnded",json!({"turnNumber":number(&current,"lastTurnNumber")+1,"finish":"Normal","quiescent":true}));
        let operation = self
            .engine
            .all("Operation")
            .into_iter()
            .find(|o| {
                text(&o["effect"], "method") == "runtime.release"
                    && o["effect"]["params"]["invocationId"] == invocation["id"]
            })
            .unwrap();
        self.engine
            .complete_effect(
                text(&operation, "id"),
                Response::ok(id(), json!({"released":true})),
            )
            .unwrap();
    }

    fn contract(&self, work: &Value, key: &str, gated: bool) -> Value {
        json!({"clientKey":key,"role":"Integration","objective":"Produce a readable report","scope":["reports"],"exclusions":[],
            "inputSlots":[],"outputs":[{"slot":"report","kind":"Report","required":true}],
            "criteria":[{"id":"report","description":"Readable report","requiredEvidence":if gated{vec!["artifact:report","check"]}else{vec!["artifact:report"]}}],
            "gateDefinitions":if gated{vec![json!({"id":"check","revision":1,"criterionIds":["report"],"kind":"Command","required":true,
                "recipe":{"executable":"fixture.exe","args":[],"cwdRelative":".","environmentRef":"local-default","timeoutSeconds":30,"evidenceParserId":"process-exit-v1"}})]}else{vec![]},
            "reviewPolicy":{"revision":1,"required":false,"rule":"AllRequiredGatesThenReview"},
            "capabilityId":"fixture-agent","requiredForDelivery":true,
            "resourceRequirements":{"workspaceId":work["workspaceId"],"mode":"ExclusiveWrite"}})
    }

    fn plan(&mut self, work: &Value, coordinator: &Value, gated: bool) -> Value {
        let current = self.engine.record(text(work, "id"), "Work").unwrap();
        let mut contract = self.contract(work, "report", gated);
        if current["requiresReplan"] == true {
            let previous = self
                .engine
                .related("Task", "workId", text(work, "id"))
                .into_iter()
                .find(|task| text(task, "clientKey") == "report")
                .unwrap();
            contract = previous["contract"].clone();
            contract["existingTaskId"] = previous["id"].clone();
            contract["objective"] = current["spec"]["goal"].clone();
            contract["scope"] = current["spec"]["scope"].clone();
            contract["exclusions"] = current["spec"]["exclusions"].clone();
            contract["criteria"] = json!(values(&current["spec"],"criteria").iter().map(|criterion|json!({
                "id":criterion["id"],"description":criterion["description"],"requiredEvidence":[text(criterion,"evidenceRule").strip_prefix("command:").unwrap_or(text(criterion,"evidenceRule"))]
            })).collect::<Vec<_>>());
        }
        self.plan_contract(work, coordinator, contract)
    }

    fn plan_contract(&mut self, work: &Value, coordinator: &Value, contract: Value) -> Value {
        let principal = Principal::Invocation {
            invocation_id: text(coordinator, "id").into(),
        };
        let current = self.engine.record(text(work, "id"), "Work").unwrap();
        let mut request = Request::new(
            "plan.propose",
            json!({"workId":work["id"],"basedOnPlanRevision":current["currentPlanRevision"],"tasks":[contract],"edges":[],"integrationTaskKey":"report","reason":"Deliver approved report"}),
        );
        request.command_id = Some(id());
        request.if_match = vec![Engine::reference(&current)];
        let response = self.engine.handle(&principal, request);
        assert_eq!(response.status, "ok", "{response:?}");
        let proposal = self
            .engine
            .record(
                response.data.unwrap()["proposalId"].as_str().unwrap(),
                "PlanProposal",
            )
            .unwrap();
        let current = self.engine.record(text(work, "id"), "Work").unwrap();
        let command_id = id();
        let mut request = Request::new("plan.apply", json!({"proposalId":proposal["id"]}));
        request.command_id = Some(command_id.clone());
        request.if_match = vec![Engine::reference(&current), Engine::reference(&proposal)];
        let response = self.engine.handle(&principal, request);
        assert_eq!(response.status, "ok", "{response:?}");
        let response=self.command(principal,"coordination.finish",json!({"turnId":coordinator["subject"]["id"],"outcome":"ActionsRecorded","commandIds":[command_id],"operationIds":[],"explanation":"Applied the plan"}),vec![]);
        assert_eq!(response.status, "ok", "{response:?}");
        self.finish_release(coordinator);
        self.engine
            .related("Invocation", "workId", text(work, "id"))
            .into_iter()
            .find(|v| {
                text(&v["dispatch"], "kind") == "ProduceResult" && text(v, "state") == "Dispatching"
            })
            .unwrap()
    }

    fn propose_change(&mut self, work: &Value, gated: bool) -> Value {
        let current = self.engine.record(text(work, "id"), "Work").unwrap();
        let mut replacement = current["spec"].clone();
        replacement["revision"] = current["currentSpecRevision"].clone();
        replacement["goal"] = json!("Revised delivery with exact evidence");
        replacement["criteria"][0]["description"] = json!("Revised readable report");
        replacement["criteria"][0]["evidenceRule"] = json!(if gated {
            "command:check"
        } else {
            "artifact:report"
        });
        let response = self.command(
            Principal::Human,
            "work.propose_change",
            json!({
                "workId":work["id"],"replacementSpec":replacement,"affectedTaskIds":[],
                "reason":"Human requested a materially revised brief"
            }),
            vec![current],
        );
        assert_eq!(response.status, "ok", "{response:?}");
        self.engine
            .record(
                text(response.data.as_ref().unwrap(), "proposalId"),
                "ChangeProposal",
            )
            .unwrap()
    }

    fn apply_change(&mut self, work: &Value, proposal: &Value) -> Response {
        let current = self.engine.record(text(work, "id"), "Work").unwrap();
        let preview = self.command(
            Principal::Human,
            "grant.preview",
            json!({
                "workId":work["id"],"specRevision":current["currentSpecRevision"],"policyRevision":1
            }),
            vec![current.clone()],
        );
        assert_eq!(preview.status, "ok", "{preview:?}");
        let response = self.command(Principal::Human,"work.apply_change",json!({
            "proposalId":proposal["id"],"grantProposalId":preview.data.unwrap()["grantProposalId"]
        }),vec![current,proposal.clone()]);
        assert_eq!(response.status, "pending", "{response:?}");
        response
    }

    fn finish_idle_coordinator(&mut self, work: &Value) {
        let coordinator = self.coordinator(work);
        let response = self.command(Principal::Invocation{invocation_id:text(&coordinator,"id").into()},
            "coordination.finish",json!({"turnId":coordinator["subject"]["id"],"outcome":"NoActionNeeded",
                "commandIds":[],"operationIds":[],"explanation":"Recorded controllers already own the remaining bounded work"}),vec![]);
        assert_eq!(response.status, "ok", "{response:?}");
        self.finish_release(&coordinator);
    }

    fn acknowledge(&mut self, invocation: &Value, continuation: Option<&str>) {
        let mut params = json!({"dispatchId":invocation["dispatch"]["id"],"taskRevision":invocation["dispatch"]["taskRevision"],"disposition":"Accepted"});
        if let Some(id) = continuation {
            params["continuationId"] = json!(id);
        }
        let response = self.command(
            Principal::Invocation {
                invocation_id: text(invocation, "id").into(),
            },
            "task.acknowledge",
            params,
            vec![],
        );
        assert_eq!(response.status, "ok", "{response:?}");
    }

    fn artifact(&mut self, work: &Value) -> Value {
        let file = self.root.join("report.txt");
        std::fs::write(&file, b"actual fixture report\n").unwrap();
        let captured = super::super::runtime::artifacts::capture(
            &self.root,
            &self.root,
            &json!({"kind":"File","relativePath":file.file_name().unwrap().to_string_lossy()}),
        )
        .unwrap();
        let operation = self.engine.effect(
            "artifact.capture",
            json!({"workspaceId":work["workspaceId"],"workId":work["id"]}),
            Some(text(work, "id")),
        );
        self.engine.db.execute_batch("BEGIN IMMEDIATE").unwrap();
        self.engine.persist().unwrap();
        self.engine.db.execute_batch("COMMIT").unwrap();
        self.engine
            .complete_effect(
                text(&operation, "id"),
                Response::ok(id(), json!({"artifacts":[captured]})),
            )
            .unwrap();
        self.engine
            .record(text(&operation, "id"), "Operation")
            .unwrap()["result"]["artifacts"][0]
            .clone()
    }

    fn submit(&mut self, invocation: &Value, artifact: &Value, supersedes: Option<&str>) -> Value {
        let mut params = json!({"dispatchId":invocation["dispatch"]["id"],"taskRevision":invocation["dispatch"]["taskRevision"],
            "inputManifestDigest":invocation["dispatch"]["inputManifestDigest"],
            "outputs":[{"slot":"report","artifact":artifact}],
            "criterionEvidence":[{"criterionId":"report","evidence":[artifact],"claim":"Readable report"}],
            "summary":"Report and evidence","knownGaps":[]});
        if let Some(result) = supersedes {
            params["supersedesResultId"] = json!(result);
        }
        let response = self.command(
            Principal::Invocation {
                invocation_id: text(invocation, "id").into(),
            },
            "result.submit",
            params,
            vec![],
        );
        assert_eq!(response.status, "ok", "{response:?}");
        self.engine
            .record(
                response.data.unwrap()["resultId"].as_str().unwrap(),
                "TaskResult",
            )
            .unwrap()
    }

    fn gate(&mut self, work: &Value, passed: bool) -> Value {
        let invocation = self
            .engine
            .related("Invocation", "workId", text(work, "id"))
            .into_iter()
            .find(|v| {
                text(&v["dispatch"], "kind") == "EvaluateGate" && text(v, "state") == "Dispatching"
            })
            .unwrap();
        self.start_invocation(&invocation);
        self.acknowledge(&invocation, None);
        let evidence = self.artifact(work);
        let dispatch = &invocation["dispatch"];
        let response=self.command(Principal::Invocation{invocation_id:text(&invocation,"id").into()},"gate.submit",json!({
            "evaluationUnitId":dispatch["evaluationUnitId"],"dispatchId":dispatch["id"],"taskRevision":dispatch["taskRevision"],
            "subjectResultId":dispatch["subjectResultId"],"evaluationRound":dispatch["evaluationRound"],
            "gateDefinitionId":"check","gateDefinitionRevision":1,"inputManifestDigest":dispatch["inputManifestDigest"],
            "outcome":if passed{"Passed"}else{"Failed"},"evidence":[evidence],"explanation":if passed{"Exit 0"}else{"Boundary assertion failed: expected 1001 rows, observed 1000"}
        }),vec![]);
        assert_eq!(response.status, "ok", "{response:?}");
        self.finish_release(&invocation);
        invocation
    }
}

#[test]
fn approval_delivery_and_two_work_identity() {
    let mut f = Fixture::new();
    let a = f.draft("Report A");
    let b = f.draft("Report B");
    assert!(f.engine.all("Invocation").is_empty());
    f.start(&a);
    f.start(&b);
    let coordinator = f.coordinator(&a);
    let worker = f.plan(&a, &coordinator, false);
    f.start_invocation(&worker);
    f.acknowledge(&worker, None);
    let artifact = f.artifact(&a);
    let result = f.submit(&worker, &artifact, None);
    assert_eq!(text(&result, "disposition"), "Submitted");
    assert!(f.engine.all("DeliveryCandidate").is_empty());
    f.finish_release(&worker);
    let current = f.engine.record(text(&a, "id"), "Work").unwrap();
    let candidate = f
        .engine
        .record(text(&current, "currentCandidateId"), "DeliveryCandidate")
        .unwrap();
    let view = f.engine.handle(
        &Principal::Human,
        Request::new("work.get", json!({"workId":a["id"]})),
    );
    assert_eq!(view.data.unwrap()["spec"]["revision"], 1);
    let deliveries = f.engine.handle(
        &Principal::Human,
        Request::new("delivery.list", json!({"workId":a["id"],"limit":100})),
    );
    assert_eq!(deliveries.data.unwrap()["items"][0]["id"], candidate["id"]);
    let delivery = f.engine.handle(
        &Principal::Human,
        Request::new("delivery.get", json!({"candidateId":candidate["id"]})),
    );
    assert_eq!(delivery.subjects.len(), 2);
    assert!(delivery
        .subjects
        .iter()
        .any(|subject| subject.kind == "Work" && subject.id == text(&a, "id")));
    assert!(
        delivery
            .subjects
            .iter()
            .any(|subject| subject.kind == "DeliveryCandidate"
                && subject.id == text(&candidate, "id"))
    );
    assert_eq!(delivery.data.unwrap()["id"], candidate["id"]);
    let response = f.command(
        Principal::Human,
        "delivery.accept",
        json!({"candidateId":candidate["id"]}),
        vec![current, candidate],
    );
    assert_eq!(response.status, "ok", "{response:?}");
    assert_eq!(response.data.unwrap()["phase"], "Completed");
    assert_eq!(
        text(
            &f.engine.record(text(&b, "id"), "Work").unwrap(),
            "lifecycle"
        ),
        "Active"
    );
    let events = f
        .engine
        .events_after(None, &json!({"kind":"Work","id":a["id"]}))
        .unwrap();
    assert!(events.iter().all(|event| event["workId"] == a["id"]));
    assert!(events.iter().any(|event| event["kind"] == "WorkCompleted"));
}

#[test]
fn specification_change_settles_old_writer_and_replans_without_resetting_grant() {
    let mut f = Fixture::new();
    let work = f.draft("Change while execution is active");
    let other = f.draft("Independent work");
    f.start(&work);
    let coordinator = f.coordinator(&work);
    let worker = f.plan(&work, &coordinator, false);
    f.start_invocation(&worker);
    f.acknowledge(&worker, None);
    f.start(&other);
    let other_before = f.engine.record(text(&other, "id"), "Work").unwrap();
    let before = f.engine.record(text(&work, "id"), "Work").unwrap();
    let old_grant = f
        .engine
        .record(text(&before, "currentGrantId"), "ExecutionGrant")
        .unwrap();
    let proposal = f.propose_change(&work, false);
    assert_eq!(
        f.engine.record(text(&work, "id"), "Work").unwrap()["currentSpecRevision"],
        1
    );
    let response = f.apply_change(&work, &proposal);
    let operation_id = response.operation_id.unwrap();
    assert_eq!(
        f.engine.record(&operation_id, "Operation").unwrap()["status"],
        "Running"
    );
    let revised = f.engine.record(text(&work, "id"), "Work").unwrap();
    assert_eq!(revised["currentSpecRevision"], 2);
    assert_eq!(
        revised["usage"]["executionAttempts"],
        before["usage"]["executionAttempts"]
    );
    let grant = f
        .engine
        .record(text(&revised, "currentGrantId"), "ExecutionGrant")
        .unwrap();
    assert_eq!(grant["limits"], old_grant["limits"]);
    assert_eq!(
        grant["allowedCapabilities"],
        old_grant["allowedCapabilities"]
    );
    let stale = f.command(
        Principal::Invocation {
            invocation_id: text(&worker, "id").into(),
        },
        "task.acknowledge",
        json!({"dispatchId":worker["dispatch"]["id"],"taskRevision":1,"disposition":"Accepted"}),
        vec![],
    );
    assert_eq!(stale.failure.unwrap().code, "STALE_DISPATCH");
    assert_eq!(
        f.engine
            .related("Invocation", "workId", text(&work, "id"))
            .len(),
        2
    );
    let stopping = f.engine.record(text(&worker, "id"), "Invocation").unwrap();
    let stop_id = text(&stopping, "stopOperationId").to_owned();
    f.engine
        .complete_effect(
            &stop_id,
            Response::pending(id(), stop_id.clone(), json!({})),
        )
        .unwrap();
    f.report(
        &worker,
        "Settled",
        json!({"quiescent":true,"executionIdentity":stopping["executionIdentity"],
        "completedOperationIds":[stop_id]}),
    );
    assert_eq!(
        f.engine.record(&operation_id, "Operation").unwrap()["status"],
        "Running"
    );
    let releasing = f.engine.record(text(&worker, "id"), "Invocation").unwrap();
    f.engine
        .complete_effect(
            text(&releasing, "releaseOperationId"),
            Response::ok(id(), json!({"released":true})),
        )
        .unwrap();
    assert_eq!(
        f.engine.record(&operation_id, "Operation").unwrap()["status"],
        "Succeeded"
    );
    let coordinator = f.coordinator(&work);
    let next = f.plan(&work, &coordinator, false);
    assert!(
        number(&next["dispatch"], "taskRevision") > number(&worker["dispatch"], "taskRevision")
    );
    f.start_invocation(&next);
    f.acknowledge(&next, None);
    let artifact = f.artifact(&work);
    f.submit(&next, &artifact, None);
    f.finish_release(&next);
    let current = f.engine.record(text(&work, "id"), "Work").unwrap();
    let candidate = f
        .engine
        .record(text(&current, "currentCandidateId"), "DeliveryCandidate")
        .unwrap();
    let accepted = f.command(
        Principal::Human,
        "delivery.accept",
        json!({"candidateId":candidate["id"]}),
        vec![current, candidate],
    );
    assert_eq!(accepted.data.unwrap()["phase"], "Completed");
    assert_eq!(
        f.engine.record(text(&other, "id"), "Work").unwrap(),
        other_before
    );
    assert_eq!(
        f.engine
            .related("WorkSpec", "workId", text(&work, "id"))
            .len(),
        2
    );
}

#[test]
fn specification_changes_require_human_exact_grant_and_deduplicate_application() {
    let mut f = Fixture::new();
    let work = f.draft("Exact change authority");
    f.start(&work);
    let coordinator = f.coordinator(&work);
    let current = f.engine.record(text(&work, "id"), "Work").unwrap();
    let mut replacement = current["spec"].clone();
    replacement["revision"] = json!(1);
    replacement["projectId"] = json!(id());
    let rejected = f.command(Principal::Human,"work.propose_change",json!({
        "workId":work["id"],"replacementSpec":replacement,"affectedTaskIds":[],"reason":"Unapproved project migration"
    }),vec![current.clone()]);
    assert_eq!(rejected.failure.unwrap().code, "FORBIDDEN");
    replacement["projectId"] = work["projectId"].clone();
    replacement["revision"] = json!(2);
    let rejected = f.command(Principal::Human,"work.propose_change",json!({
        "workId":work["id"],"replacementSpec":replacement,"affectedTaskIds":[],"reason":"Stale brief"
    }),vec![current]);
    assert_eq!(rejected.failure.unwrap().code, "STALE_VERSION");
    let proposal = f.propose_change(&work, false);
    let current = f.engine.record(text(&work, "id"), "Work").unwrap();
    let params = json!({"proposalId":proposal["id"],"grantProposalId":id()});
    let rejected = f.command(
        Principal::Invocation {
            invocation_id: text(&coordinator, "id").into(),
        },
        "work.apply_change",
        params,
        vec![current.clone(), proposal.clone()],
    );
    assert_eq!(rejected.failure.unwrap().code, "FORBIDDEN");
    let other = f.draft("Other grant");
    let wrong_preview = f.command(
        Principal::Human,
        "grant.preview",
        json!({"workId":other["id"],"specRevision":1,"policyRevision":1}),
        vec![other],
    );
    let rejected = f.command(Principal::Human,"work.apply_change",json!({
        "proposalId":proposal["id"],"grantProposalId":wrong_preview.data.unwrap()["grantProposalId"]
    }),vec![current.clone(),proposal.clone()]);
    assert_eq!(rejected.failure.unwrap().code, "STALE_VERSION");
    let preview = f.command(
        Principal::Human,
        "grant.preview",
        json!({"workId":work["id"],"specRevision":1,"policyRevision":1}),
        vec![current.clone()],
    );
    let data = preview.data.unwrap();
    assert_eq!(data["proposal"]["remaining"]["coordinationTurns"], 11);
    let mut request = Request::new(
        "work.apply_change",
        json!({
            "proposalId":proposal["id"],"grantProposalId":data["grantProposalId"]
        }),
    );
    request.command_id = Some(id());
    request.if_match = vec![Engine::reference(&current), Engine::reference(&proposal)];
    let first = f.engine.handle(&Principal::Human, request.clone());
    assert_eq!(first.status, "pending", "{first:?}");
    request.request_id = id();
    let replay = f.engine.handle(&Principal::Human, request);
    assert_eq!(replay.operation_id, first.operation_id);
    assert_eq!(replay.status, "pending");
    assert_eq!(
        f.engine.record(text(&work, "id"), "Work").unwrap()["currentSpecRevision"],
        2
    );
    assert_eq!(
        f.engine
            .related("WorkSpec", "workId", text(&work, "id"))
            .len(),
        2
    );
}

#[test]
fn manual_takeover_waits_for_settlement_and_binding_release() {
    let mut f = Fixture::new();
    let work = f.draft("Take over an active writer");
    f.start(&work);
    let coordinator = f.coordinator(&work);
    let worker = f.plan(&work, &coordinator, false);
    f.start_invocation(&worker);
    f.acknowledge(&worker, None);
    let workspace = f
        .engine
        .record(text(&work, "workspaceId"), "Workspace")
        .unwrap();
    let response = f.command(
        Principal::Human,
        "workspace.takeover",
        json!({"workspaceId":workspace["id"]}),
        vec![workspace.clone()],
    );
    let operation_id = response.operation_id.unwrap();
    assert_eq!(
        f.engine.record(&operation_id, "Operation").unwrap()["status"],
        "Running"
    );
    assert_ne!(
        f.engine
            .record(text(&workspace, "id"), "Workspace")
            .unwrap()["writer"],
        "Human"
    );
    let stopping = f.engine.record(text(&worker, "id"), "Invocation").unwrap();
    let stop_id = text(&stopping, "stopOperationId").to_owned();
    f.engine
        .complete_effect(
            &stop_id,
            Response::pending(id(), stop_id.clone(), json!({})),
        )
        .unwrap();
    f.report(
        &worker,
        "Settled",
        json!({"quiescent":true,"executionIdentity":stopping["executionIdentity"],
        "completedOperationIds":[stop_id]}),
    );
    assert_eq!(
        f.engine.record(&operation_id, "Operation").unwrap()["status"],
        "Running"
    );
    let releasing = f.engine.record(text(&worker, "id"), "Invocation").unwrap();
    f.engine
        .complete_effect(
            text(&releasing, "releaseOperationId"),
            Response::ok(id(), json!({"released":true})),
        )
        .unwrap();
    assert_eq!(
        f.engine.record(&operation_id, "Operation").unwrap()["status"],
        "Succeeded"
    );
    assert_eq!(
        f.engine
            .record(text(&workspace, "id"), "Workspace")
            .unwrap()["writer"],
        "Human"
    );
    assert!(!f
        .engine
        .related("AttentionItem", "workId", text(&work, "id"))
        .iter()
        .any(
            |item| text(item, "status") == "Open" && text(item, "reason") == "PROTOCOL_INCOMPLETE"
        ));
}

#[test]
fn specification_change_invalidates_candidate_and_cannot_weaken_required_gates() {
    let mut f = Fixture::new();
    let work = f.draft("Revise a reviewed candidate");
    f.start(&work);
    let coordinator = f.coordinator(&work);
    let worker = f.plan(&work, &coordinator, true);
    f.start_invocation(&worker);
    f.acknowledge(&worker, None);
    let artifact = f.artifact(&work);
    let result = f.submit(&worker, &artifact, None);
    f.finish_release(&worker);
    f.gate(&work, true);
    let before = f.engine.record(text(&work, "id"), "Work").unwrap();
    let candidate = f
        .engine
        .record(text(&before, "currentCandidateId"), "DeliveryCandidate")
        .unwrap();
    let proposal = f.propose_change(&work, true);
    let mut stale = proposal.clone();
    stale["version"] = json!(number(&stale, "version") + 1);
    let rejected = f.command(
        Principal::Human,
        "work.apply_change",
        json!({"proposalId":proposal["id"],"grantProposalId":id()}),
        vec![before.clone(), stale],
    );
    assert_eq!(rejected.failure.unwrap().code, "STALE_VERSION");
    f.apply_change(&work, &proposal);
    let current = f.engine.record(text(&work, "id"), "Work").unwrap();
    assert!(current.get("currentCandidateId").is_none());
    assert_eq!(
        f.engine
            .record(text(&candidate, "id"), "DeliveryCandidate")
            .unwrap()["status"],
        "Superseded"
    );
    assert_eq!(
        f.engine.record(text(&result, "id"), "TaskResult").unwrap()["disposition"],
        "Superseded"
    );
    let old_candidate = f
        .engine
        .record(text(&candidate, "id"), "DeliveryCandidate")
        .unwrap();
    let refused = f.command(
        Principal::Human,
        "delivery.accept",
        json!({"candidateId":candidate["id"]}),
        vec![current.clone(), old_candidate],
    );
    assert_ne!(refused.status, "ok");
    let coordinator = f.coordinator(&work);
    let task = f.engine.record(text(&result, "taskId"), "Task").unwrap();
    let mut weakened = task["contract"].clone();
    weakened["existingTaskId"] = task["id"].clone();
    weakened["gateDefinitions"] = json!([]);
    weakened["criteria"][0]["requiredEvidence"] = json!(["artifact:report"]);
    let current = f.engine.record(text(&work, "id"), "Work").unwrap();
    let refused = f.command(Principal::Invocation{invocation_id:text(&coordinator,"id").into()},"plan.propose",
        json!({"workId":work["id"],"basedOnPlanRevision":current["currentPlanRevision"],"tasks":[weakened],"edges":[],"integrationTaskKey":"report","reason":"Must not drop the required gate"}),
        vec![current]);
    assert_eq!(refused.failure.unwrap().code, "FORBIDDEN");
    let next = f.plan(&work, &coordinator, true);
    f.start_invocation(&next);
    f.acknowledge(&next, None);
    let revised = f.artifact(&work);
    f.submit(&next, &revised, None);
    f.finish_release(&next);
    assert!(f
        .engine
        .record(text(&work, "id"), "Work")
        .unwrap()
        .get("currentCandidateId")
        .is_none());
    f.gate(&work, true);
    let current = f.engine.record(text(&work, "id"), "Work").unwrap();
    let next_candidate = f
        .engine
        .record(text(&current, "currentCandidateId"), "DeliveryCandidate")
        .unwrap();
    assert_ne!(next_candidate["id"], candidate["id"]);
    let response = f.command(
        Principal::Human,
        "delivery.accept",
        json!({"candidateId":next_candidate["id"]}),
        vec![current, next_candidate],
    );
    assert_eq!(response.data.unwrap()["phase"], "Completed");
}

#[test]
fn command_dedup_survives_restart_and_rejects_reuse() {
    let mut f = Fixture::new();
    let mut request = Request::new(
        "work.create_draft",
        json!({"projectId":f.project,"goal":"Durable","scope":["reports"],"exclusions":[],
        "criteria":[{"id":"report","description":"Report","evidenceRule":"artifact:report"}],"context":[],"delivery":{"kind":"Report"},"sourceMessageIds":[]}),
    );
    request.command_id = Some(id());
    let original = f.engine.handle(&Principal::Human, request.clone());
    assert_eq!(original.status, "ok");
    let replacement_root = f.root.join("replacement");
    let old = std::mem::replace(&mut f.engine, Engine::open(&replacement_root).unwrap());
    drop(old);
    let reopened = Engine::open(&f.root).unwrap();
    f.engine = reopened;
    request.request_id = id();
    let retried = f.engine.handle(&Principal::Human, request.clone());
    assert_eq!(original.data, retried.data);
    assert_eq!(f.engine.all("Work").len(), 1);
    request.params["goal"] = json!("Different");
    assert_eq!(
        f.engine
            .handle(&Principal::Human, request)
            .failure
            .unwrap()
            .code,
        "COMMAND_ID_REUSED"
    );
}

#[test]
fn exact_versions_unknown_fields_and_actor_spoofing() {
    let mut f = Fixture::new();
    let work = f.draft("Version checks");
    let mut stale = work.clone();
    stale["version"] = json!(1);
    let response = f.command(
        Principal::Human,
        "grant.preview",
        json!({"workId":work["id"],"specRevision":1,"policyRevision":1}),
        vec![stale],
    );
    assert_eq!(response.status, "conflict");
    assert_eq!(response.subjects[0].version, number(&work, "version"));
    let response = f.command(
        Principal::Human,
        "work.control",
        json!({"workId":work["id"],"action":"Hold","actor":"Service"}),
        vec![work.clone()],
    );
    assert_eq!(response.failure.unwrap().code, "INVALID_ARGUMENT");
    let response=f.command(Principal::Invocation{invocation_id:id()},"work.start",json!({"workId":work["id"],"specRevision":1,"projectPolicyRevision":1,"grantProposalId":id()}),vec![work]);
    assert_eq!(response.failure.unwrap().code, "FORBIDDEN");
}

#[test]
fn missing_submission_is_not_success() {
    let mut f = Fixture::new();
    let work = f.draft("Missing result");
    f.start(&work);
    let coordinator = f.coordinator(&work);
    let worker = f.plan(&work, &coordinator, false);
    f.start_invocation(&worker);
    f.acknowledge(&worker, None);
    f.finish_release(&worker);
    let attempt = f
        .engine
        .record(text(&worker["subject"], "id"), "Attempt")
        .unwrap();
    assert_eq!(text(&attempt, "state"), "Failed");
    assert_eq!(text(&attempt, "endReason"), "MissingSubmission");
    assert!(f.engine.all("TaskResult").is_empty());
    assert!(f.engine.all("DeliveryCandidate").is_empty());
    assert!(
        f.engine
            .related("CoordinationTurn", "workId", text(&work, "id"))
            .len()
            >= 2
    );
}

#[test]
fn early_context_answer_waits_for_yield_and_exact_acknowledgment() {
    let mut f = Fixture::new();
    let work = f.draft("Context");
    f.start(&work);
    let coordinator = f.coordinator(&work);
    let worker = f.plan(&work, &coordinator, false);
    f.start_invocation(&worker);
    f.acknowledge(&worker, None);
    let response = f.command(
        Principal::Invocation {
            invocation_id: text(&worker, "id").into(),
        },
        "task.request_context",
        json!({
        "dispatchId":worker["dispatch"]["id"],"taskRevision":1,"question":"Which column order?",
        "target":{"kind":"Coordinator"},"inputs":[],"blocking":true}),
        vec![],
    );
    assert_eq!(response.status, "needs_input", "{response:?}");
    let context = f
        .engine
        .record(&response.input_request.unwrap().id, "ContextRequest")
        .unwrap();
    let response=f.command(Principal::Service,"task.answer_context",json!({"requestId":context["id"],"answer":"Keep the approved order","evidence":[],"compatibility":"ExistingInputs"}),vec![context.clone()]);
    assert_eq!(response.status, "ok", "{response:?}");
    assert!(!f
        .engine
        .all("Operation")
        .iter()
        .any(|o| text(&o["effect"], "method") == "runtime.continue"));
    f.report(
        &worker,
        "TurnEnded",
        json!({"turnNumber":1,"finish":"Normal","quiescent":true}),
    );
    let attempt = f
        .engine
        .record(text(&worker["subject"], "id"), "Attempt")
        .unwrap();
    assert_eq!(text(&attempt, "state"), "WaitingForContext");
    assert!(attempt.get("endReason").is_none());
    let operation = f
        .engine
        .all("Operation")
        .into_iter()
        .find(|o| text(&o["effect"], "method") == "runtime.continue")
        .unwrap();
    let continuation = operation["effect"]["params"]["continuation"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    f.engine
        .complete_effect(
            text(&operation, "id"),
            Response::ok(
                id(),
                json!({"continuationId":continuation,"disposition":"Recorded"}),
            ),
        )
        .unwrap();
    f.acknowledge(&worker, Some(&continuation));
    let artifact = f.artifact(&work);
    f.submit(&worker, &artifact, None);
    f.finish_release(&worker);
    let attempt = f
        .engine
        .record(text(&worker["subject"], "id"), "Attempt")
        .unwrap();
    assert_eq!(text(&attempt, "state"), "Succeeded");
    assert!(attempt.get("waitingRequestId").is_none());
}

#[test]
fn failed_check_structured_rework_and_new_result() {
    let mut f = Fixture::new();
    let work = f.draft("Checked report");
    f.start(&work);
    let coordinator = f.coordinator(&work);
    let worker = f.plan(&work, &coordinator, true);
    f.start_invocation(&worker);
    f.acknowledge(&worker, None);
    let artifact = f.artifact(&work);
    let first = f.submit(&worker, &artifact, None);
    f.finish_release(&worker);
    f.gate(&work, false);
    let first = f.engine.record(text(&first, "id"), "TaskResult").unwrap();
    assert_eq!(text(&first, "disposition"), "ChangesRequested");
    assert!(f.engine.all("DeliveryCandidate").is_empty());
    let rework = f
        .engine
        .record(text(&first, "reworkId"), "ReworkInstruction")
        .unwrap();
    assert_eq!(rework["instruction"]["reason"], "ContractViolation");
    assert_eq!(rework["instruction"]["action"], "ReviseOutput");
    let task = f.engine.record(text(&first, "taskId"), "Task").unwrap();
    let response=f.command(Principal::Service,"task.rework",json!({"taskId":task["id"],"resultId":first["id"],"reworkId":rework["id"],"action":"ReviseOutput"}),vec![task,first.clone()]);
    assert_eq!(response.status, "ok", "{response:?}");
    let next = f
        .engine
        .related("Invocation", "workId", text(&work, "id"))
        .into_iter()
        .find(|v| {
            text(&v["dispatch"], "kind") == "ProduceResult" && text(v, "state") == "Dispatching"
        })
        .unwrap();
    assert_ne!(next["id"], worker["id"]);
    assert_eq!(next["dispatch"]["rework"]["resultId"], first["id"]);
    f.start_invocation(&next);
    f.acknowledge(&next, None);
    let artifact = f.artifact(&work);
    let second = f.submit(&next, &artifact, Some(text(&first, "id")));
    f.finish_release(&next);
    f.gate(&work, true);
    assert_eq!(
        text(
            &f.engine.record(text(&second, "id"), "TaskResult").unwrap(),
            "disposition"
        ),
        "Accepted"
    );
    assert_eq!(f.engine.all("DeliveryCandidate").len(), 1);
}

#[test]
fn stale_input_manifest_and_wrong_terminal_record_are_rejected() {
    let mut f = Fixture::new();
    let work = f.draft("Stale result");
    f.start(&work);
    let coordinator = f.coordinator(&work);
    let worker = f.plan(&work, &coordinator, false);
    f.start_invocation(&worker);
    f.acknowledge(&worker, None);
    let response=f.command(Principal::Invocation{invocation_id:text(&worker,"id").into()},"result.submit",json!({
        "dispatchId":worker["dispatch"]["id"],"taskRevision":1,"inputManifestDigest":format!("sha256:{}","0".repeat(64)),
        "outputs":[],"criterionEvidence":[],"summary":"Pretend completed","knownGaps":[]}),vec![]);
    assert_eq!(response.status, "conflict");
    assert_eq!(response.failure.unwrap().code, "STALE_DISPATCH");
    let response=f.command(Principal::Invocation{invocation_id:text(&worker,"id").into()},"gate.submit",json!({
        "evaluationUnitId":id(),"dispatchId":worker["dispatch"]["id"],"taskRevision":1,"subjectResultId":id(),"evaluationRound":1,
        "gateDefinitionId":"check","gateDefinitionRevision":1,"inputManifestDigest":worker["dispatch"]["inputManifestDigest"],
        "outcome":"Passed","evidence":[],"explanation":"I think tests pass"}),vec![]);
    assert_eq!(response.failure.unwrap().code, "WRONG_TERMINAL_RECORD");
}

#[test]
fn store_exclusive_lock_and_schema_validation() {
    let f = Fixture::new();
    assert!(Engine::open(&f.root).is_err());
    let root = f.root.join("future-schema");
    std::fs::create_dir_all(&root).unwrap();
    let db = Connection::open(root.join("work.db")).unwrap();
    db.execute_batch("PRAGMA user_version=99").unwrap();
    drop(db);
    assert!(Engine::open(&root)
        .err()
        .unwrap()
        .to_string()
        .contains("schema"));
}

#[test]
fn cursor_store_identity_and_unknown_operations() {
    let mut f = Fixture::new();
    let projects = f.engine.handle(
        &Principal::Human,
        Request::new("project.list", json!({"limit":100})),
    );
    assert_eq!(projects.status, "ok");
    assert_eq!(projects.data.unwrap()["items"][0]["id"], f.project);
    let work = f.draft("Events");
    let old = f.engine.cursor();
    f.start(&work);
    let events = f
        .engine
        .events_after(Some(&old), &json!({"kind":"WorkList"}))
        .unwrap();
    assert!(!events.is_empty());
    assert!(f
        .engine
        .events_after(Some("another:123"), &json!({"kind":"WorkList"}))
        .is_err());
    let response = f.command(Principal::Human, "work.publish", json!({}), vec![]);
    assert_eq!(response.status, "unsupported");
    assert_eq!(response.failure.unwrap().code, "METHOD_UNSUPPORTED");
}

#[test]
fn missing_check_gets_evidence_only_rework_and_rejects_stale_round() {
    let mut f = Fixture::new();
    let work = f.draft("Evidence correction");
    f.start(&work);
    let coordinator = f.coordinator(&work);
    let worker = f.plan(&work, &coordinator, true);
    f.start_invocation(&worker);
    f.acknowledge(&worker, None);
    let artifact = f.artifact(&work);
    let result = f.submit(&worker, &artifact, None);
    f.finish_release(&worker);
    let check = f
        .engine
        .related("Invocation", "workId", text(&work, "id"))
        .into_iter()
        .find(|invocation| {
            text(&invocation["dispatch"], "kind") == "EvaluateGate"
                && text(invocation, "state") == "Dispatching"
        })
        .unwrap();
    f.start_invocation(&check);
    f.acknowledge(&check, None);
    f.finish_release(&check);
    let result = f.engine.record(text(&result, "id"), "TaskResult").unwrap();
    assert_eq!(text(&result, "disposition"), "ChangesRequested");
    let rework = f
        .engine
        .record(text(&result, "reworkId"), "ReworkInstruction")
        .unwrap();
    assert_eq!(rework["instruction"]["action"], "CollectEvidence");
    let task = f.engine.record(text(&result, "taskId"), "Task").unwrap();
    let response = f.command(
        Principal::Service,
        "task.rework",
        json!({"taskId":task["id"],"resultId":result["id"],
        "reworkId":rework["id"],"action":"CollectEvidence"}),
        vec![task, result.clone()],
    );
    assert_eq!(response.status, "ok", "{response:?}");
    assert_eq!(f.engine.all("TaskResult").len(), 1);
    assert_eq!(
        f.engine
            .all("Invocation")
            .iter()
            .filter(|invocation| text(&invocation["dispatch"], "kind") == "ProduceResult")
            .count(),
        1
    );
    let second = f
        .engine
        .related("Invocation", "workId", text(&work, "id"))
        .into_iter()
        .find(|invocation| {
            text(&invocation["dispatch"], "kind") == "EvaluateGate"
                && text(invocation, "state") == "Dispatching"
        })
        .unwrap();
    assert_eq!(second["dispatch"]["evaluationRound"], 2);
    f.start_invocation(&second);
    f.acknowledge(&second, None);
    let response = f.command(Principal::Invocation{invocation_id:text(&second,"id").into()},"gate.submit",json!({
        "evaluationUnitId":second["dispatch"]["evaluationUnitId"],"dispatchId":second["dispatch"]["id"],
        "taskRevision":1,"subjectResultId":result["id"],"evaluationRound":1,"gateDefinitionId":"check",
        "gateDefinitionRevision":1,"inputManifestDigest":second["dispatch"]["inputManifestDigest"],
        "outcome":"Passed","evidence":[artifact],"explanation":"Old round cannot decide current verdict"
    }),vec![]);
    assert_eq!(response.status, "conflict");
    assert_eq!(response.failure.unwrap().code, "STALE_EVALUATION");
}

#[test]
fn accepted_dependency_pins_exact_result_without_human_child_acceptance() {
    let mut f = Fixture::new();
    let work = f.draft("Handoff");
    f.start(&work);
    let mut contribution = f.contract(&work, "research", false);
    contribution["role"] = json!("Contribution");
    let mut integration = f.contract(&work, "integration", false);
    integration["inputSlots"] = json!([{"slot":"research","source":{"kind":"Dependency","sourceTaskKey":"research","outputSlot":"report"}}]);
    let current = f.engine.record(text(&work, "id"), "Work").unwrap();
    let response = f.command(Principal::Service,"plan.propose",json!({"workId":work["id"],"tasks":[contribution,integration],
        "edges":[{"sourceTaskKey":"research","outputSlot":"report","consumerTaskKey":"integration","condition":"GatePassed","requiredGateIds":[]}],
        "integrationTaskKey":"integration","reason":"Use accepted evidence in the final report"}),vec![current]);
    assert_eq!(response.status, "ok", "{response:?}");
    let proposal = f
        .engine
        .record(
            response.data.unwrap()["proposalId"].as_str().unwrap(),
            "PlanProposal",
        )
        .unwrap();
    let current = f.engine.record(text(&work, "id"), "Work").unwrap();
    let response = f.command(
        Principal::Service,
        "plan.apply",
        json!({"proposalId":proposal["id"]}),
        vec![current, proposal],
    );
    assert_eq!(response.status, "ok", "{response:?}");
    let worker = f
        .engine
        .related("Invocation", "workId", text(&work, "id"))
        .into_iter()
        .find(|v| text(&v["dispatch"], "kind") == "ProduceResult")
        .unwrap();
    assert_eq!(worker["dispatch"]["role"], "Contribution");
    f.start_invocation(&worker);
    f.acknowledge(&worker, None);
    let artifact = f.artifact(&work);
    let result = f.submit(&worker, &artifact, None);
    assert_eq!(
        f.engine
            .all("Invocation")
            .iter()
            .filter(|v| text(&v["dispatch"], "role") == "Integration")
            .count(),
        0
    );
    f.finish_release(&worker);
    let consumer = f
        .engine
        .related("Invocation", "workId", text(&work, "id"))
        .into_iter()
        .find(|v| text(&v["dispatch"], "role") == "Integration")
        .unwrap();
    assert_eq!(
        consumer["dispatch"]["inputs"][0]["sourceResultId"],
        result["id"]
    );
    assert_eq!(consumer["dispatch"]["inputs"][0]["artifact"], artifact);
    assert!(f.engine.all("Acceptance").is_empty());
}

#[test]
fn final_revision_preserves_history_and_demands_new_candidate() {
    let mut f = Fixture::new();
    let work = f.draft("Final revision");
    f.start(&work);
    let coordinator = f.coordinator(&work);
    let worker = f.plan(&work, &coordinator, false);
    f.start_invocation(&worker);
    f.acknowledge(&worker, None);
    let artifact = f.artifact(&work);
    let first = f.submit(&worker, &artifact, None);
    f.finish_release(&worker);
    let current = f.engine.record(text(&work, "id"), "Work").unwrap();
    let candidate = f
        .engine
        .record(text(&current, "currentCandidateId"), "DeliveryCandidate")
        .unwrap();
    let response = f.command(Principal::Human,"delivery.request_changes",json!({"candidateId":candidate["id"],
        "findings":[{"criterionId":"report","requestedChange":"Explain the boundary cause in the report"}],
        "preserveArtifacts":[artifact],"advance":true}),vec![current,candidate.clone()]);
    assert_eq!(response.status, "ok", "{response:?}");
    assert_eq!(
        text(
            &f.engine
                .record(text(&candidate, "id"), "DeliveryCandidate")
                .unwrap(),
            "status"
        ),
        "Rejected"
    );
    let result = f.engine.record(text(&first, "id"), "TaskResult").unwrap();
    let task = f.engine.record(text(&first, "taskId"), "Task").unwrap();
    let response = f.command(
        Principal::Service,
        "task.rework",
        json!({"taskId":task["id"],"resultId":result["id"],
        "reworkId":result["reworkId"],"action":"ReviseOutput"}),
        vec![task, result],
    );
    assert_eq!(response.status, "ok", "{response:?}");
    let next = f
        .engine
        .related("Invocation", "workId", text(&work, "id"))
        .into_iter()
        .find(|v| {
            text(&v["dispatch"], "kind") == "ProduceResult" && text(v, "state") == "Dispatching"
        })
        .unwrap();
    f.start_invocation(&next);
    f.acknowledge(&next, None);
    let revised = f.artifact(&work);
    f.submit(&next, &revised, Some(text(&first, "id")));
    f.finish_release(&next);
    let current = f.engine.record(text(&work, "id"), "Work").unwrap();
    assert_ne!(current["currentCandidateId"], candidate["id"]);
    assert_eq!(f.engine.all("DeliveryCandidate").len(), 2);
    if f.engine
        .related("Invocation", "workId", text(&work, "id"))
        .iter()
        .any(|invocation| {
            text(&invocation["subject"], "kind") == "Coordination"
                && text(invocation, "state") == "Dispatching"
        })
    {
        f.finish_idle_coordinator(&work);
    }
    let current = f.engine.record(text(&work, "id"), "Work").unwrap();
    let revised_candidate = f
        .engine
        .record(text(&current, "currentCandidateId"), "DeliveryCandidate")
        .unwrap();
    let response = f.command(
        Principal::Human,
        "delivery.accept",
        json!({"candidateId":revised_candidate["id"]}),
        vec![current, revised_candidate],
    );
    assert_eq!(response.data.unwrap()["phase"], "Completed");
}

#[test]
fn immediate_question_and_busy_intake_have_real_queued_turn_identities() {
    let mut f = Fixture::new();
    let conversation = id();
    let console = id();
    let opened = f.command(
        Principal::Human,
        "console.open",
        json!({"consoleSessionId":console,"projectId":f.project,"conversationId":conversation}),
        vec![],
    );
    assert_eq!(opened.status, "ok", "{opened:?}");
    assert_eq!(opened.data.unwrap()["contextVersion"], 1);
    assert!(f.engine.all("Invocation").is_empty());
    let incompatible = f.command(
        Principal::Human,
        "console.open",
        json!({"consoleSessionId":console,"projectId":f.project,"conversationId":id()}),
        vec![],
    );
    assert_eq!(incompatible.failure.unwrap().code, "INVALID_REFERENCE");
    let submit = |text_value: &str| {
        json!({"conversationId":conversation,"clientMessageId":id(),"text":text_value,
        "attachments":[],"declaredIntent":"ImmediateQuestion",
        "context":{"consoleSessionId":console,"contextVersion":1,"projectId":f.project}})
    };
    let first_params = submit("Explain this diagnostic");
    let second_params = submit("What does the second line mean?");
    let response = f.command(
        Principal::Human,
        "conversation.submit",
        first_params,
        vec![],
    );
    assert_eq!(response.status, "ok", "{response:?}");
    let first_turn = response.data.unwrap()["intakeTurnId"]
        .as_str()
        .unwrap()
        .to_owned();
    let invocation = f
        .engine
        .all("Invocation")
        .into_iter()
        .find(|invocation| text(&invocation["subject"], "id") == first_turn)
        .unwrap();
    f.start_invocation(&invocation);
    let response = f.command(
        Principal::Human,
        "conversation.submit",
        second_params,
        vec![],
    );
    assert_eq!(response.status, "ok", "{response:?}");
    let queued_turn = response.data.unwrap()["intakeTurnId"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(first_turn, queued_turn);
    assert!(f
        .engine
        .record(&queued_turn, "CoordinationTurn")
        .unwrap()
        .get("invocationId")
        .is_none());
    assert_eq!(f.engine.all("Invocation").len(), 1);
    f.report(
        &invocation,
        "TextDelta",
        json!({"messageId":invocation["replyMessageId"],"partId":id(),"chunkIndex":0,
        "text":"The diagnostic identifies a missing boundary condition."}),
    );
    let response = f.command(Principal::Invocation{invocation_id:text(&invocation,"id").into()},"coordination.finish",json!({
        "turnId":first_turn,"outcome":"Answered","commandIds":[],"operationIds":[],"messageId":invocation["replyMessageId"],"explanation":"Answered the immediate question"
    }),vec![]);
    assert_eq!(response.status, "ok", "{response:?}");
    f.finish_release(&invocation);
    assert_eq!(f.engine.all("Invocation").len(), 2);
    assert!(f
        .engine
        .record(&queued_turn, "CoordinationTurn")
        .unwrap()
        .get("invocationId")
        .is_some());
    assert!(f.engine.all("Work").is_empty());
    assert!(f
        .engine
        .events_after(None, &json!({"kind":"Conversation","id":conversation}))
        .unwrap()
        .iter()
        .any(|event| text(event, "kind") == "MessageDelta"));
}

#[test]
fn new_work_intake_from_a_registered_work_context_uses_a_distinct_console_binding() {
    use crate::agent_center::{client::new_context, commands};
    let _locale = crate::test_support::lock_locale();
    let mut f = Fixture::new();
    let work = f.draft("Existing work");
    let mut context = new_context();
    context.project_id = Some(f.project.clone());
    context.work_id = Some(text(&work, "id").into());
    let opened = f.command(
        Principal::Human,
        "work.open",
        json!({"workId":work["id"]}),
        vec![],
    );
    assert_eq!(opened.status, "ok", "{opened:?}");
    let opened = opened.data.unwrap();
    context.console_session_id = text(&opened["context"], "consoleSessionId").into();
    context.conversation_id = text(&opened["context"], "conversationId").into();
    context.context_version = number(&opened["context"], "contextVersion");
    let original = commands::conversation("Discuss existing work".into(), &context, false).unwrap();
    let response = f.engine.handle(
        &Principal::Human,
        serde_json::from_value(original.envelope()).unwrap(),
    );
    assert_eq!(response.status, "ok", "{response:?}");
    let arguments = vec!["work".into(), "new".into(), "A separate goal".into()];
    let commands::Action::Operation(intake) =
        commands::compile(&arguments, &context, true).unwrap()
    else {
        panic!("expected new work intake");
    };
    let response = f.engine.handle(
        &Principal::Human,
        serde_json::from_value(intake.envelope()).unwrap(),
    );
    assert_eq!(response.status, "ok", "{response:?}");
    assert_eq!(f.engine.all("ConsoleSession").len(), 2);
    assert_eq!(f.engine.all("Conversation").len(), 2);
    assert!(intake.params["context"].get("selectedWorkId").is_none());
    let mut incompatible = intake.params.clone();
    incompatible["clientMessageId"] = json!(id());
    incompatible["context"]["consoleSessionId"] = json!(context.console_session_id);
    let rejected = f.command(
        Principal::Human,
        "conversation.submit",
        incompatible,
        vec![],
    );
    assert_eq!(rejected.failure.unwrap().code, "INVALID_REFERENCE");
    let followup =
        commands::conversation("Continue the original discussion".into(), &context, false).unwrap();
    let response = f.engine.handle(
        &Principal::Human,
        serde_json::from_value(followup.envelope()).unwrap(),
    );
    assert_eq!(response.status, "ok", "{response:?}");
    assert_eq!(f.engine.all("ConsoleSession").len(), 2);
}

#[test]
fn decision_is_saved_then_applied_only_by_bound_continuation_ack() {
    let mut f = Fixture::new();
    let work = f.draft("Human judgment");
    f.start(&work);
    let coordinator = f.coordinator(&work);
    let worker = f.plan(&work, &coordinator, false);
    f.start_invocation(&worker);
    f.acknowledge(&worker, None);
    let response = f.command(Principal::Invocation{invocation_id:text(&worker,"id").into()},"task.request_context",json!({
        "dispatchId":worker["dispatch"]["id"],"taskRevision":1,"question":"Which approved wording is preferred?",
        "target":{"kind":"Coordinator"},"inputs":[],"blocking":true
    }),vec![]);
    let context = f
        .engine
        .record(&response.input_request.unwrap().id, "ContextRequest")
        .unwrap();
    let current = f.engine.record(text(&work, "id"), "Work").unwrap();
    let response = f.command(Principal::Service,"decision.request",json!({
        "workId":work["id"],"purpose":"TaskInput","subject":Engine::reference(&current),
        "application":{"requestId":context["id"]},"question":"Choose the report wording",
        "options":[{"id":"brief","label":"Brief","impact":"Concise report"},{"id":"detailed","label":"Detailed","impact":"More explanation"}],
        "responseSchema":{"type":"string","enum":["brief","detailed"]},"contextRequestId":context["id"]
    }),vec![current]);
    assert_eq!(response.status, "needs_input", "{response:?}");
    let decision = f
        .engine
        .record(&response.input_request.unwrap().id, "DecisionRequest")
        .unwrap();
    let response = f.command(
        Principal::Human,
        "decision.answer",
        json!({"decisionId":decision["id"],"value":"detailed"}),
        vec![decision.clone()],
    );
    assert_eq!(response.status, "ok", "{response:?}");
    let application_id = response.data.unwrap()["applicationId"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        text(
            &f.engine
                .record(text(&decision, "id"), "DecisionRequest")
                .unwrap(),
            "status"
        ),
        "Answered"
    );
    assert_eq!(
        text(
            &f.engine
                .record(&application_id, "DecisionApplication")
                .unwrap(),
            "status"
        ),
        "Pending"
    );
    let conflict = f.command(
        Principal::Human,
        "decision.answer",
        json!({"decisionId":decision["id"],"value":"brief"}),
        vec![decision.clone()],
    );
    assert_eq!(conflict.status, "conflict");
    f.report(
        &worker,
        "TurnEnded",
        json!({"turnNumber":1,"finish":"Normal","quiescent":true}),
    );
    let operation = f
        .engine
        .all("Operation")
        .into_iter()
        .find(|o| text(&o["effect"], "method") == "runtime.continue")
        .unwrap();
    let continuation = operation["effect"]["params"]["continuation"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    f.engine
        .complete_effect(
            text(&operation, "id"),
            Response::ok(
                id(),
                json!({"continuationId":continuation,"disposition":"Recorded"}),
            ),
        )
        .unwrap();
    assert_eq!(
        text(
            &f.engine
                .record(text(&decision, "id"), "DecisionRequest")
                .unwrap(),
            "status"
        ),
        "Answered"
    );
    f.acknowledge(&worker, Some(&continuation));
    assert_eq!(
        text(
            &f.engine
                .record(text(&decision, "id"), "DecisionRequest")
                .unwrap(),
            "status"
        ),
        "Resolved"
    );
    assert_eq!(
        text(
            &f.engine
                .record(&application_id, "DecisionApplication")
                .unwrap(),
            "status"
        ),
        "Acknowledged"
    );
}

#[test]
fn takeover_handback_captures_new_inputs_and_preserves_work_hold() {
    let mut f = Fixture::new();
    let work = f.draft("Human contribution");
    f.start(&work);
    let coordinator = f.coordinator(&work);
    let worker = f.plan(&work, &coordinator, false);
    f.start_invocation(&worker);
    f.acknowledge(&worker, None);
    let artifact = f.artifact(&work);
    let result = f.submit(&worker, &artifact, None);
    f.finish_release(&worker);
    let workspace = f
        .engine
        .record(text(&work, "workspaceId"), "Workspace")
        .unwrap();
    let response = f.command(
        Principal::Human,
        "workspace.takeover",
        json!({"workspaceId":workspace["id"]}),
        vec![workspace.clone()],
    );
    assert_eq!(response.status, "pending", "{response:?}");
    let operation = f
        .engine
        .record(response.operation_id.as_deref().unwrap(), "Operation")
        .unwrap();
    assert_eq!(text(&operation, "status"), "Succeeded");
    let workspace = f
        .engine
        .record(text(&workspace, "id"), "Workspace")
        .unwrap();
    assert_eq!(text(&workspace, "writer"), "Human");
    let current = f.engine.record(text(&work, "id"), "Work").unwrap();
    let response = f.command(
        Principal::Human,
        "work.control",
        json!({"workId":work["id"],"action":"Hold"}),
        vec![current],
    );
    assert_eq!(response.status, "ok", "{response:?}");
    let response = f.command(
        Principal::Human,
        "workspace.handback",
        json!({"workspaceId":workspace["id"],
        "summary":"Corrected the report boundary explanation","resumeAffected":true}),
        vec![workspace.clone()],
    );
    assert_eq!(response.status, "pending", "{response:?}");
    let manual = f.root.join("manual-snapshot");
    std::fs::create_dir_all(&manual).unwrap();
    std::fs::write(manual.join("report.txt"), b"Human-corrected report").unwrap();
    let captured = super::super::runtime::artifacts::capture(
        &f.root,
        &f.root,
        &json!({"kind":"Tree","relativePath":"manual-snapshot"}),
    )
    .unwrap();
    f.engine
        .complete_effect(
            response.operation_id.as_deref().unwrap(),
            Response::ok(id(), json!({"artifacts":[captured]})),
        )
        .unwrap();
    let current = f.engine.record(text(&work, "id"), "Work").unwrap();
    assert_eq!(text(&current, "desiredAdvancement"), "Hold");
    assert!(current.get("currentCandidateId").is_none());
    let task = f.engine.record(text(&result, "taskId"), "Task").unwrap();
    assert_eq!(number(&task, "revision"), 2);
    assert!(values(&task["contract"], "inputSlots")
        .iter()
        .any(|input| text(input, "slot") == "manual-contribution"));
    assert_eq!(
        text(
            &f.engine.record(text(&result, "id"), "TaskResult").unwrap(),
            "disposition"
        ),
        "Superseded"
    );
    assert_eq!(
        text(
            &f.engine
                .record(text(&workspace, "id"), "Workspace")
                .unwrap(),
            "writer"
        ),
        "None"
    );
    let current = f.engine.record(text(&work, "id"), "Work").unwrap();
    assert!(current.get("integrationResultId").is_none());
    let response = f.command(
        Principal::Human,
        "work.control",
        json!({"workId":work["id"],"action":"Resume"}),
        vec![current],
    );
    assert_eq!(response.status, "ok", "{response:?}");
    f.finish_idle_coordinator(&work);
    let next = f
        .engine
        .related("Invocation", "workId", text(&work, "id"))
        .into_iter()
        .find(|invocation| {
            text(&invocation["dispatch"], "kind") == "ProduceResult"
                && text(invocation, "state") == "Dispatching"
        })
        .unwrap();
    assert!(values(&next["dispatch"], "inputs")
        .iter()
        .any(|input| text(input, "slot") == "manual-contribution"));
    assert_ne!(
        next["dispatch"]["inputManifestDigest"],
        worker["dispatch"]["inputManifestDigest"]
    );
    f.start_invocation(&next);
    f.acknowledge(&next, None);
    let revised = f.artifact(&work);
    f.submit(&next, &revised, Some(text(&result, "id")));
    f.finish_release(&next);
    let current = f.engine.record(text(&work, "id"), "Work").unwrap();
    let candidate = f
        .engine
        .record(text(&current, "currentCandidateId"), "DeliveryCandidate")
        .unwrap();
    let response = f.command(
        Principal::Human,
        "delivery.accept",
        json!({"candidateId":candidate["id"]}),
        vec![current, candidate],
    );
    assert_eq!(response.data.unwrap()["phase"], "Completed");
}

#[test]
fn missing_artifact_blocks_final_acceptance_without_mutating_candidate() {
    let mut f = Fixture::new();
    let work = f.draft("Missing evidence");
    f.start(&work);
    let coordinator = f.coordinator(&work);
    let worker = f.plan(&work, &coordinator, false);
    f.start_invocation(&worker);
    f.acknowledge(&worker, None);
    let artifact = f.artifact(&work);
    f.submit(&worker, &artifact, None);
    f.finish_release(&worker);
    let record = f
        .engine
        .record(text(&artifact, "artifactId"), "Artifact")
        .unwrap();
    let path = Path::new(text(&record, "localPath"));
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    std::fs::set_permissions(path, permissions).unwrap();
    std::fs::remove_file(path).unwrap();
    let current = f.engine.record(text(&work, "id"), "Work").unwrap();
    let candidate = f
        .engine
        .record(text(&current, "currentCandidateId"), "DeliveryCandidate")
        .unwrap();
    let response = f.command(
        Principal::Human,
        "delivery.accept",
        json!({"candidateId":candidate["id"]}),
        vec![current, candidate.clone()],
    );
    assert_eq!(response.failure.unwrap().code, "ARTIFACT_UNAVAILABLE");
    assert!(f.engine.all("Acceptance").is_empty());
    assert_eq!(
        text(
            &f.engine
                .record(text(&candidate, "id"), "DeliveryCandidate")
                .unwrap(),
            "status"
        ),
        "Proposed"
    );
}

#[test]
fn sqlite_failure_rolls_back_cache_events_and_command_receipt_and_backup_is_consistent() {
    let mut f = Fixture::new();
    let work = f.draft("Transactional failure");
    let cursor = f.engine.cursor();
    f.engine.db.execute_batch("CREATE TRIGGER reject_preview BEFORE INSERT ON records WHEN NEW.kind='GrantProposal' BEGIN SELECT RAISE(ABORT,'fixture write failure'); END;").unwrap();
    let response = f.command(
        Principal::Human,
        "grant.preview",
        json!({"workId":work["id"],"specRevision":1,"policyRevision":1}),
        vec![work.clone()],
    );
    assert_eq!(response.status, "error");
    assert_eq!(response.failure.unwrap().code, "EXECUTION_FAILED");
    assert!(f.engine.all("GrantProposal").is_empty());
    assert_eq!(f.engine.cursor(), cursor);
    f.engine
        .db
        .execute_batch("DROP TRIGGER reject_preview")
        .unwrap();
    let backup_root = f.root.join("consistent-backup");
    std::fs::create_dir_all(&backup_root).unwrap();
    f.engine.backup(&backup_root.join("work.db")).unwrap();
    let backup = Engine::open(&backup_root).unwrap();
    assert_eq!(backup.cursor(), cursor);
    assert_eq!(backup.record(text(&work, "id"), "Work").unwrap(), work);
    assert!(f.engine.backup(&backup_root.join("work.db")).is_err());
}

#[test]
fn dispatched_outbox_survives_restart_without_blind_replacement() {
    let mut f = Fixture::new();
    let work = f.draft("Recover effect identity");
    f.start(&work);
    let effects = f.engine.take_effects().unwrap();
    let invoke = effects
        .iter()
        .find(|effect| effect.method == "runtime.invoke")
        .unwrap()
        .clone();
    assert!(f.engine.take_effects().unwrap().is_empty());
    let original_invocation = invoke.params["invocation"]["id"].clone();
    let replacement_root = f.root.join("replace-owner");
    let old = std::mem::replace(&mut f.engine, Engine::open(&replacement_root).unwrap());
    drop(old);
    f.engine = Engine::open(&f.root).unwrap();
    assert_eq!(
        text(&f.engine.record(&invoke.id, "Operation").unwrap(), "status"),
        "RepairRequired"
    );
    assert_eq!(f.engine.all("Invocation").len(), 1);
    assert_eq!(f.engine.all("Invocation")[0]["id"], original_invocation);
    assert!(f.engine.take_effects().unwrap().is_empty());
    f.engine
        .complete_effect(
            &invoke.id,
            Response::ok(
                id(),
                json!({"invocationId":original_invocation,"disposition":"AlreadyRecorded"}),
            ),
        )
        .unwrap();
    assert_eq!(
        text(&f.engine.record(&invoke.id, "Operation").unwrap(), "status"),
        "Succeeded"
    );
    assert!(f
        .engine
        .related("AttentionItem", "subjectId", &invoke.id)
        .iter()
        .all(|item| text(item, "status") != "Open"));
}

#[test]
fn required_review_is_a_settled_evaluation_unit_not_a_recursive_contribution() {
    let mut f = Fixture::new();
    let work = f.draft("Reviewed report");
    f.start(&work);
    let mut task = f.contract(&work, "report", false);
    task["reviewPolicy"] = json!({"revision":1,"required":true,"reviewerCapabilityId":"fixture-agent","rule":"AllRequiredGatesThenReview"});
    let current = f.engine.record(text(&work, "id"), "Work").unwrap();
    let response = f.command(
        Principal::Service,
        "plan.propose",
        json!({"workId":work["id"],"tasks":[task],
        "edges":[],"integrationTaskKey":"report","reason":"Review the readable report"}),
        vec![current],
    );
    assert_eq!(response.status, "ok", "{response:?}");
    let proposal = f
        .engine
        .record(
            response.data.unwrap()["proposalId"].as_str().unwrap(),
            "PlanProposal",
        )
        .unwrap();
    let current = f.engine.record(text(&work, "id"), "Work").unwrap();
    let response = f.command(
        Principal::Service,
        "plan.apply",
        json!({"proposalId":proposal["id"]}),
        vec![current, proposal],
    );
    assert_eq!(response.status, "ok", "{response:?}");
    let worker = f
        .engine
        .all("Invocation")
        .into_iter()
        .find(|v| text(&v["dispatch"], "kind") == "ProduceResult")
        .unwrap();
    f.start_invocation(&worker);
    f.acknowledge(&worker, None);
    let artifact = f.artifact(&work);
    let result = f.submit(&worker, &artifact, None);
    f.finish_release(&worker);
    let reviewer = f
        .engine
        .all("Invocation")
        .into_iter()
        .find(|v| text(&v["dispatch"], "kind") == "ReviewResult")
        .unwrap();
    assert_eq!(
        reviewer["dispatch"]["inputManifestDigest"],
        json!(digest(&reviewer["dispatch"]["inputs"]).unwrap())
    );
    f.start_invocation(&reviewer);
    f.acknowledge(&reviewer, None);
    let response = f.command(Principal::Invocation{invocation_id:text(&reviewer,"id").into()},"review.submit",json!({
        "dispatchId":reviewer["dispatch"]["id"],"evaluationUnitId":reviewer["dispatch"]["evaluationUnitId"],
        "taskRevision":1,"subjectResultId":result["id"],"evaluationRound":1,
        "evidenceManifestDigest":reviewer["dispatch"]["evidenceManifestDigest"],
        "gateResultIds":[],"recommendation":"Accept","findings":[],"preserveArtifacts":[]
    }),vec![]);
    assert_eq!(response.status, "ok", "{response:?}");
    assert_eq!(
        text(
            &f.engine.record(text(&result, "id"), "TaskResult").unwrap(),
            "disposition"
        ),
        "Reviewing"
    );
    f.finish_release(&reviewer);
    assert_eq!(
        text(
            &f.engine.record(text(&result, "id"), "TaskResult").unwrap(),
            "disposition"
        ),
        "Accepted"
    );
    assert_eq!(f.engine.all("TaskResult").len(), 1);
    assert_eq!(f.engine.all("TaskReview").len(), 1);
    assert_eq!(f.engine.all("Attempt").len(), 2);
}

#[test]
fn undispatched_intent_also_requires_reconciliation_after_restart() {
    let mut f = Fixture::new();
    let work = f.draft("Unresolved intent");
    f.start(&work);
    let operation = f
        .engine
        .all("Operation")
        .into_iter()
        .find(|operation| text(&operation["effect"], "method") == "runtime.invoke")
        .unwrap();
    let replacement_root = f.root.join("replace-owner");
    let old = std::mem::replace(&mut f.engine, Engine::open(&replacement_root).unwrap());
    drop(old);
    f.engine = Engine::open(&f.root).unwrap();
    assert_eq!(
        text(
            &f.engine
                .record(text(&operation, "id"), "Operation")
                .unwrap(),
            "status"
        ),
        "RepairRequired"
    );
    assert!(f.engine.take_effects().unwrap().is_empty());
}

#[test]
fn wire_omits_absent_optionals_and_rejects_unknown_payload_fields() {
    let policy = ReviewPolicy {
        revision: 1,
        required: false,
        reviewer_capability_id: None,
        rule: "AllRequiredGatesThenReview".into(),
    };
    let value = serde_json::to_value(policy).unwrap();
    assert!(value.get("reviewerCapabilityId").is_none());
    let body = TaskResultBody {
        dispatch_id: id(),
        task_revision: 1,
        input_manifest_digest: format!("sha256:{}", "0".repeat(64)),
        outputs: Vec::new(),
        criterion_evidence: Vec::new(),
        summary: "Recorded report".into(),
        known_gaps: Vec::new(),
        supersedes_result_id: None,
    };
    assert!(serde_json::to_value(body)
        .unwrap()
        .get("supersedesResultId")
        .is_none());
    let request = serde_json::to_value(Request::new("work.list", json!({"limit":10}))).unwrap();
    assert!(request.get("commandId").is_none());
    assert_eq!(request["type"], "request");
    let response = serde_json::to_value(Response::ok(id(), json!({}))).unwrap();
    for optional in ["failure", "operationId", "inputRequest", "cursor"] {
        assert!(response.get(optional).is_none());
    }
    assert!(parse::<WorkControl>(&json!({"workId":id(),"action":"Hold","actor":"Human"})).is_err());
    assert!(parse::<ReviewPolicy>(
        &json!({"revision":1,"required":false,"reviewerCapabilityId":null,
        "rule":"AllRequiredGatesThenReview"})
    )
    .is_err());
}

#[path = "tests\\acceptance_regressions.rs"]
mod acceptance_regressions_b1_b2;

#[path = "tests\\handoff_regressions.rs"]
mod handoff_regressions_b3_b6;
