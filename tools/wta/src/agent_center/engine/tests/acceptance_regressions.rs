// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

use super::*;
use std::path::PathBuf;

fn approved_check(f: &mut Fixture, evidence_rule: &str) -> Value {
    let response = f.command(
        Principal::Human,
        "work.create_draft",
        json!({
            "executionMode":"LegacyTasks","projectId":f.project,"goal":"Deliver only after the approved boundary check",
            "scope":["reports"],"exclusions":[],
            "criteria":[{"id":"report","description":"Readable report","evidenceRule":evidence_rule}],
            "context":[],"delivery":{"kind":"Report"},"sourceMessageIds":[]
        }),
        vec![],
    );
    assert_eq!(response.status, "ok", "{response:?}");
    f.engine
        .record(text(response.data.as_ref().unwrap(), "workId"), "Work")
        .unwrap()
}

fn propose(f: &mut Fixture, work: &Value, coordinator: &Value, contract: Value) -> Response {
    let current = f.engine.record(text(work, "id"), "Work").unwrap();
    f.command(
        Principal::Invocation { invocation_id: text(coordinator, "id").into() },
        "plan.propose",
        json!({"workId":work["id"],"basedOnPlanRevision":current["currentPlanRevision"],
            "tasks":[contract],"edges":[],"integrationTaskKey":"report","reason":"Preserve approved evidence"}),
        vec![current],
    )
}

#[test]
fn verification_probe_initial_plan_cannot_weaken_approved_evidence() {
    for rule in ["check", "command:check"] {
        let mut f = Fixture::new();
        let work = approved_check(&mut f, rule);
        f.start(&work);
        let coordinator = f.coordinator(&work);
        let contract = f.contract(&work, "report", false);
        let response = propose(&mut f, &work, &coordinator, contract);
        assert_eq!(response.failure.unwrap().code, "INVALID_ARGUMENT");
        assert!(f.engine.all("Task").is_empty());
        assert!(f.engine.all("PlanProposal").is_empty());
        assert!(f.engine.all("DeliveryCandidate").is_empty());
        assert!(f.engine.all("Acceptance").is_empty());
    }
}

#[test]
fn every_plan_preserves_approved_description_and_required_artifact() {
    let mut f = Fixture::new();
    let work = f.draft("Approved report meaning is immutable");
    f.start(&work);
    let coordinator = f.coordinator(&work);
    let mut changed_description = f.contract(&work, "report", false);
    changed_description["criteria"][0]["description"] = json!("An unrelated report");
    let response = propose(&mut f, &work, &coordinator, changed_description);
    assert_eq!(response.failure.unwrap().code, "INVALID_ARGUMENT");
    let mut replaced_artifact = f.contract(&work, "report", true);
    replaced_artifact["criteria"][0]["requiredEvidence"] = json!(["check"]);
    let response = propose(&mut f, &work, &coordinator, replaced_artifact);
    assert_eq!(response.failure.unwrap().code, "INVALID_ARGUMENT");
}

#[test]
fn invalid_plan_output_kind_identifies_the_field_and_can_be_corrected() {
    let mut f = Fixture::new();
    let work = f.draft("Discoverable plan outputs");
    f.start(&work);
    let coordinator = f.coordinator(&work);
    let before = f.engine.record(text(&work, "id"), "Work").unwrap();
    for kind in ["", "file", "Patch", "Markdown", "UnsupportedFutureKind"] {
        let mut contract = f.contract(&work, "report", false);
        contract["outputs"]
            .as_array_mut()
            .unwrap()
            .push(json!({"slot":"extra","kind":kind,"required":false}));
        let response = propose(&mut f, &work, &coordinator, contract);
        assert_eq!(response.status, "error");
        let failure = response.failure.unwrap();
        assert_eq!(failure.code, "INVALID_ARGUMENT");
        assert_eq!(failure.field_errors.len(), 1);
        assert_eq!(
            failure.field_errors[0].path,
            "params.tasks[0].outputs[1].kind"
        );
        for allowed in OutputContract::KINDS {
            assert!(failure.field_errors[0].message.contains(allowed));
        }
        assert!(failure.message.contains("new commandId"));
        assert!(f.engine.all("PlanProposal").is_empty());
        assert!(f.engine.all("Task").is_empty());
        assert_eq!(f.engine.record(text(&work, "id"), "Work").unwrap(), before);
    }
    let worker = f.plan(&work, &coordinator, false);
    assert_eq!(worker["dispatch"]["outputs"][0]["kind"], "Report");
    assert_eq!(
        f.engine.record(text(&work, "id"), "Work").unwrap()["currentPlanRevision"],
        1
    );
}

#[test]
fn every_advertised_plan_output_kind_is_admitted_without_weakening_checks() {
    for kind in OutputContract::KINDS {
        let mut f = Fixture::new();
        let work = f.draft("All supported output categories");
        f.start(&work);
        let coordinator = f.coordinator(&work);
        let mut contract = f.contract(&work, "report", true);
        contract["outputs"][0]["kind"] = json!(kind);
        let response = propose(&mut f, &work, &coordinator, contract);
        assert_eq!(response.status, "ok", "{kind}: {response:?}");
        if ["Code", "Tree", "GitCommit"].contains(kind) {
            let mut unchecked = f.contract(&work, "report", false);
            unchecked["outputs"][0]["kind"] = json!(kind);
            let response = propose(&mut f, &work, &coordinator, unchecked);
            let failure = response.failure.unwrap();
            assert_eq!(failure.code, "INVALID_ARGUMENT");
            assert!(failure.message.contains("required check"));
        }
    }
}

#[test]
fn approved_prose_evidence_maps_to_real_required_checks_without_losing_meaning() {
    for rule in [
        "The previously failing relevant tests pass with captured command output.",
        "\u{6d4b}\u{8bd5}\u{901a}\u{8fc7}\u{5e76}\u{4fdd}\u{7559}\u{8f93}\u{51fa}",
    ] {
        let mut f = Fixture::new();
        let work = approved_check(&mut f, rule);
        f.start(&work);
        let coordinator = f.coordinator(&work);
        let mut contract = f.contract(&work, "report", true);
        let missing = propose(&mut f, &work, &coordinator, contract.clone());
        assert_eq!(missing.failure.unwrap().code, "INVALID_ARGUMENT");
        contract["criteria"][0]["evidenceRule"] = json!("Just produce something");
        let changed = propose(&mut f, &work, &coordinator, contract.clone());
        assert_eq!(changed.failure.unwrap().code, "INVALID_ARGUMENT");
        contract["criteria"][0]["evidenceRule"] = json!(rule);
        let mut unmapped = contract.clone();
        unmapped["criteria"][0]["requiredEvidence"] = json!([]);
        assert_eq!(
            propose(&mut f, &work, &coordinator, unmapped)
                .failure
                .unwrap()
                .code,
            "INVALID_ARGUMENT"
        );
        let mut unbound = contract.clone();
        unbound["criteria"][0]["requiredEvidence"] = json!(["invented-check"]);
        assert_eq!(
            propose(&mut f, &work, &coordinator, unbound)
                .failure
                .unwrap()
                .code,
            "INVALID_ARGUMENT"
        );
        let mut optional_check = contract.clone();
        optional_check["gateDefinitions"][0]["required"] = json!(false);
        assert_eq!(
            propose(&mut f, &work, &coordinator, optional_check)
                .failure
                .unwrap()
                .code,
            "INVALID_ARGUMENT"
        );
        let mut optional_output = contract.clone();
        optional_output["outputs"][0]["required"] = json!(false);
        optional_output["outputs"]
            .as_array_mut()
            .unwrap()
            .push(json!({"slot":"unrelated","kind":"Report","required":true}));
        assert_eq!(
            propose(&mut f, &work, &coordinator, optional_output)
                .failure
                .unwrap()
                .code,
            "INVALID_ARGUMENT"
        );
        assert!(f.engine.all("PlanProposal").is_empty());
        let worker = f.plan_contract(&work, &coordinator, contract);
        assert_eq!(worker["dispatch"]["criteria"][0]["evidenceRule"], rule);
        assert_eq!(
            worker["dispatch"]["criteria"][0]["requiredEvidence"],
            json!(["artifact:report", "check"])
        );
        let task = f
            .engine
            .record(text(&worker["dispatch"], "taskId"), "Task")
            .unwrap();
        assert_eq!(task["contract"]["criteria"][0]["evidenceRule"], rule);
        f.start_invocation(&worker);
        f.acknowledge(&worker, None);
        let artifact = f.artifact(&work);
        f.submit(&worker, &artifact, None);
        f.finish_release(&worker);
        assert!(f.engine.all("DeliveryCandidate").is_empty());
        f.gate(&work, true);
        let response = accept(&mut f, &work);
        assert_eq!(response.status, "ok", "{response:?}");
        assert_eq!(
            f.engine.record(text(&work, "id"), "Work").unwrap()["lifecycle"],
            "Completed"
        );
    }
}

#[test]
fn explicit_evidence_mapping_cannot_replace_approved_machine_bindings() {
    for rule in ["check", "command:check", "artifact:report"] {
        let mut f = Fixture::new();
        let work = approved_check(&mut f, rule);
        f.start(&work);
        let coordinator = f.coordinator(&work);
        let mut contract = f.contract(&work, "report", true);
        contract["criteria"][0]["evidenceRule"] = json!(rule);
        contract["criteria"][0]["requiredEvidence"] = if rule.starts_with("artifact:") {
            json!(["check"])
        } else {
            json!(["artifact:report"])
        };
        let response = propose(&mut f, &work, &coordinator, contract);
        assert_eq!(response.failure.unwrap().code, "INVALID_ARGUMENT");
        assert!(f.engine.all("PlanProposal").is_empty());
    }
}

fn candidate(f: &mut Fixture, gated: bool) -> (Value, Value) {
    let work = if gated {
        approved_check(f, "command:check")
    } else {
        f.draft("Fixed content delivery")
    };
    f.start(&work);
    let coordinator = f.coordinator(&work);
    let worker = f.plan(&work, &coordinator, gated);
    f.start_invocation(&worker);
    f.acknowledge(&worker, None);
    let artifact = f.artifact(&work);
    f.submit(&worker, &artifact, None);
    f.finish_release(&worker);
    if gated {
        assert!(f.engine.all("DeliveryCandidate").is_empty());
        f.gate(&work, true);
    }
    (work, artifact)
}

fn accept(f: &mut Fixture, work: &Value) -> Response {
    let current = f.engine.record(text(work, "id"), "Work").unwrap();
    let candidate = f
        .engine
        .record(text(&current, "currentCandidateId"), "DeliveryCandidate")
        .unwrap();
    f.command(
        Principal::Human,
        "delivery.accept",
        json!({"candidateId":candidate["id"]}),
        vec![current, candidate],
    )
}

fn assert_not_accepted(f: &mut Fixture, work: &Value) {
    let response = accept(f, work);
    let failure = response.failure.unwrap();
    assert_eq!(failure.code, "ARTIFACT_UNAVAILABLE");
    assert_eq!(failure.subjects.len(), 1);
    let affected = &failure.subjects[0];
    assert_eq!(affected.kind, "Artifact");
    let artifact = f.engine.record(&affected.id, "Artifact").unwrap();
    assert_eq!(affected.version, number(&artifact, "version"));
    assert!(crate::agent_center::runtime::artifacts::verify(&artifact).is_err());
    let current = f.engine.record(text(work, "id"), "Work").unwrap();
    assert_ne!(current["lifecycle"], "Completed");
    assert!(current.get("currentAcceptanceId").is_none());
    assert!(f.engine.all("Acceptance").is_empty());
    assert!(!f
        .engine
        .events_after(None, &json!({"kind":"Work","id":work["id"]}))
        .unwrap()
        .iter()
        .any(|event| event["kind"] == "DeliveryAccepted" || event["kind"] == "WorkCompleted"));
}

fn writable(path: &Path) {
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    std::fs::set_permissions(path, permissions).unwrap();
}

#[test]
fn verification_probe_mutated_artifact_cannot_be_accepted() {
    let mut f = Fixture::new();
    let (work, reference) = candidate(&mut f, false);
    let artifact = f
        .engine
        .record(text(&reference, "artifactId"), "Artifact")
        .unwrap();
    let path = Path::new(text(&artifact, "localPath"));
    writable(path);
    std::fs::write(path, b"modified after candidate preparation\n").unwrap();
    assert!(path.exists());
    assert_not_accepted(&mut f, &work);
}

#[test]
fn command_evidence_mapping_and_production_capture_allow_gated_delivery() {
    let mut f = Fixture::new();
    let (work, reference) = candidate(&mut f, true);
    let artifact = f
        .engine
        .record(text(&reference, "artifactId"), "Artifact")
        .unwrap();
    assert_eq!(artifact["manifest"]["kind"], "File");
    let content_path = Path::new(text(&artifact, "localPath"));
    let manifest_bytes = std::fs::read(
        content_path
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("manifest.json"),
    )
    .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&manifest_bytes).unwrap(),
        artifact["manifest"]
    );
    assert_eq!(
        artifact["digest"],
        format!("sha256:{:x}", Sha256::digest(&manifest_bytes))
    );
    assert_eq!(
        artifact["manifest"]["entries"][0]["digest"],
        format!(
            "sha256:{:x}",
            Sha256::digest(std::fs::read(content_path).unwrap())
        )
    );
    assert_ne!(
        artifact["digest"],
        artifact["manifest"]["entries"][0]["digest"]
    );
    let response = accept(&mut f, &work);
    assert_eq!(response.status, "ok", "{response:?}");
    assert_eq!(response.data.unwrap()["phase"], "Completed");
    assert_eq!(f.engine.all("Acceptance").len(), 1);
}

#[test]
fn corrupted_gate_evidence_blocks_final_acceptance() {
    let mut f = Fixture::new();
    let (work, _) = candidate(&mut f, true);
    let gate = f.engine.all("GateResult").pop().unwrap();
    let evidence = f
        .engine
        .record(text(&gate["evidence"][0], "artifactId"), "Artifact")
        .unwrap();
    let path = Path::new(text(&evidence, "localPath"));
    writable(path);
    std::fs::write(path, b"fabricated passing check").unwrap();
    assert_not_accepted(&mut f, &work);
}

#[test]
fn deleted_substituted_and_manifest_mismatched_outputs_block_acceptance() {
    for mutation in [
        "delete",
        "directory",
        "manifest",
        "record-manifest",
        "extra",
        "same-size",
    ] {
        let mut f = Fixture::new();
        let (work, reference) = candidate(&mut f, false);
        let mut artifact = f
            .engine
            .record(text(&reference, "artifactId"), "Artifact")
            .unwrap();
        let path = PathBuf::from(text(&artifact, "localPath"));
        match mutation {
            "delete" | "directory" => {
                writable(&path);
                std::fs::remove_file(&path).unwrap();
                if mutation == "directory" {
                    std::fs::create_dir(&path).unwrap();
                }
            }
            "manifest" => {
                let manifest = path
                    .parent()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .join("manifest.json");
                writable(&manifest);
                std::fs::write(manifest, b"{}").unwrap();
            }
            "record-manifest" => {
                artifact["manifest"]["entries"][0]["digest"] =
                    json!(format!("sha256:{}", "0".repeat(64)));
                f.engine.put(artifact);
            }
            "extra" => std::fs::write(
                path.parent().unwrap().join("unrecorded.txt"),
                b"substitution",
            )
            .unwrap(),
            "same-size" => {
                let length = std::fs::metadata(&path).unwrap().len() as usize;
                writable(&path);
                std::fs::write(path, vec![b'x'; length]).unwrap();
            }
            _ => unreachable!(),
        }
        assert_not_accepted(&mut f, &work);
    }
}

#[test]
fn corrupted_review_preservation_evidence_blocks_final_acceptance() {
    let mut f = Fixture::new();
    let work = approved_check(&mut f, "command:check");
    f.start(&work);
    let coordinator = f.coordinator(&work);
    let mut contract = f.contract(&work, "report", true);
    contract["reviewPolicy"]["required"] = json!(true);
    contract["reviewPolicy"]["reviewerCapabilityId"] = json!("fixture-agent");
    let response = propose(&mut f, &work, &coordinator, contract);
    assert_eq!(response.status, "ok", "{response:?}");
    let proposal = f
        .engine
        .record(
            text(response.data.as_ref().unwrap(), "proposalId"),
            "PlanProposal",
        )
        .unwrap();
    let current = f.engine.record(text(&work, "id"), "Work").unwrap();
    let response = f.command(
        Principal::Invocation {
            invocation_id: text(&coordinator, "id").into(),
        },
        "plan.apply",
        json!({"proposalId":proposal["id"]}),
        vec![current, proposal],
    );
    assert_eq!(response.status, "ok", "{response:?}");
    let response = f.command(
        Principal::Invocation { invocation_id: text(&coordinator, "id").into() },
        "coordination.finish",
        json!({"turnId":coordinator["subject"]["id"],"outcome":"NoActionNeeded",
            "commandIds":[],"operationIds":[],"explanation":"The accepted plan owns the remaining work"}),
        vec![],
    );
    assert_eq!(response.status, "ok", "{response:?}");
    f.finish_release(&coordinator);
    let worker = f
        .engine
        .related("Invocation", "workId", text(&work, "id"))
        .into_iter()
        .find(|invocation| text(&invocation["dispatch"], "kind") == "ProduceResult")
        .unwrap();
    f.start_invocation(&worker);
    f.acknowledge(&worker, None);
    let output = f.artifact(&work);
    f.submit(&worker, &output, None);
    f.finish_release(&worker);
    f.gate(&work, true);
    let reviewer = f
        .engine
        .related("Invocation", "workId", text(&work, "id"))
        .into_iter()
        .find(|invocation| text(&invocation["dispatch"], "kind") == "ReviewResult")
        .unwrap();
    f.start_invocation(&reviewer);
    f.acknowledge(&reviewer, None);
    let evidence = f.artifact(&work);
    let dispatch = &reviewer["dispatch"];
    let unit = f
        .engine
        .record(text(dispatch, "evaluationUnitId"), "EvaluationUnit")
        .unwrap();
    let response = f.command(
        Principal::Invocation { invocation_id: text(&reviewer, "id").into() },
        "review.submit",
        json!({"dispatchId":dispatch["id"],"taskRevision":dispatch["taskRevision"],
            "evaluationUnitId":dispatch["evaluationUnitId"],"subjectResultId":dispatch["subjectResultId"],
            "evaluationRound":dispatch["evaluationRound"],"evidenceManifestDigest":dispatch["evidenceManifestDigest"],
            "gateResultIds":unit["gateResultIds"],"recommendation":"Accept",
            "findings":[],"preserveArtifacts":[evidence]}), vec![],
    );
    assert_eq!(response.status, "ok", "{response:?}");
    f.finish_release(&reviewer);
    let record = f
        .engine
        .record(text(&evidence, "artifactId"), "Artifact")
        .unwrap();
    let path = Path::new(text(&record, "localPath"));
    writable(path);
    std::fs::write(path, b"review evidence was replaced").unwrap();
    assert_not_accepted(&mut f, &work);
}

#[test]
fn production_tree_and_git_commit_manifests_verify_exact_directory_contents() {
    for kind in ["Tree", "GitCommit"] {
        for mutation in [
            "file",
            "deleted-directory",
            "extra-file",
            "extra-directory",
            "manifest",
        ] {
            let f = Fixture::new();
            let source = f.root.join("tree-source");
            std::fs::create_dir_all(source.join("empty")).unwrap();
            std::fs::write(source.join("value.txt"), b"captured tree").unwrap();
            let mut captured = crate::agent_center::runtime::artifacts::capture(
                &f.root,
                &f.root,
                &json!({"kind":"Tree","relativePath":"tree-source"}),
            )
            .unwrap();
            // GitCommit capture uses this same Tree manifest and adds provenance
            // outside the digest, as exercised by the real workspace capture test.
            captured["kind"] = json!(kind);
            if kind == "GitCommit" {
                captured["commitId"] = json!("0123456789012345678901234567890123456789");
                captured["repositoryPath"] = json!(source);
            }
            let root = crate::agent_center::runtime::artifacts::verify(&captured).unwrap();
            match mutation {
                "file" => {
                    writable(&root.join("value.txt"));
                    std::fs::write(root.join("value.txt"), b"tampered tree").unwrap();
                }
                "deleted-directory" => std::fs::remove_dir(root.join("empty")).unwrap(),
                "extra-file" => std::fs::write(root.join("extra.txt"), b"unrecorded").unwrap(),
                "extra-directory" => std::fs::create_dir(root.join("extra")).unwrap(),
                "manifest" => captured["manifest"]["kind"] = json!("File"),
                _ => unreachable!(),
            }
            assert!(
                crate::agent_center::runtime::artifacts::verify(&captured).is_err(),
                "{kind}/{mutation}"
            );
        }
    }
}

#[cfg(windows)]
#[test]
fn substituted_capture_root_junction_blocks_acceptance_even_with_identical_bytes() {
    let mut f = Fixture::new();
    let (work, reference) = candidate(&mut f, false);
    let record = f
        .engine
        .record(text(&reference, "artifactId"), "Artifact")
        .unwrap();
    let content = Path::new(text(&record, "localPath")).parent().unwrap();
    let replacement = content.with_file_name("replacement");
    std::fs::rename(content, &replacement).unwrap();
    let output = std::process::Command::new("cmd.exe")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(content)
        .arg(&replacement)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let response = accept(&mut f, &work);
    // Remove the junction before fixture cleanup, including when the assertion fails.
    std::fs::remove_dir(content).unwrap();
    assert_eq!(response.failure.unwrap().code, "ARTIFACT_UNAVAILABLE");
    assert!(f.engine.all("Acceptance").is_empty());
    assert_ne!(
        f.engine.record(text(&work, "id"), "Work").unwrap()["lifecycle"],
        "Completed"
    );
}

#[test]
fn handback_preserves_other_workspace_contract_and_accepted_result_in_full_plan() {
    let mut f = Fixture::new();
    let work = f.draft("Preserve an independent workspace during handback");
    f.start(&work);
    let coordinator = f.coordinator(&work);
    let workspace = f
        .engine
        .record(text(&work, "workspaceId"), "Workspace")
        .unwrap();
    let other_root = f.root.join("independent-workspace");
    std::fs::create_dir_all(&other_root).unwrap();
    let mut other_workspace = workspace.clone();
    other_workspace["localRoot"] = json!(other_root);
    let other_workspace = f.engine.create("Workspace", other_workspace);
    let integration = f.contract(&work, "report", false);
    let mut independent = f.contract(&work, "independent", false);
    independent["role"] = json!("Contribution");
    independent["requiredForDelivery"] = json!(false);
    independent["resourceRequirements"]["workspaceId"] = other_workspace["id"].clone();
    let current = f.engine.record(text(&work, "id"), "Work").unwrap();
    let response = f.command(
        Principal::Invocation {
            invocation_id: text(&coordinator, "id").into(),
        },
        "plan.propose",
        json!({"workId":work["id"],"basedOnPlanRevision":current["currentPlanRevision"],
            "tasks":[integration, independent],"edges":[],"integrationTaskKey":"report",
            "reason":"Two independently owned workspaces"}),
        vec![current],
    );
    assert_eq!(response.status, "ok", "{response:?}");
    let proposal = f
        .engine
        .record(
            text(response.data.as_ref().unwrap(), "proposalId"),
            "PlanProposal",
        )
        .unwrap();
    let current = f.engine.record(text(&work, "id"), "Work").unwrap();
    let response = f.command(
        Principal::Invocation {
            invocation_id: text(&coordinator, "id").into(),
        },
        "plan.apply",
        json!({"proposalId":proposal["id"]}),
        vec![current, proposal],
    );
    assert_eq!(response.status, "ok", "{response:?}");
    let response = f.command(
        Principal::Invocation {
            invocation_id: text(&coordinator, "id").into(),
        },
        "coordination.finish",
        json!({"turnId":coordinator["subject"]["id"],"outcome":"NoActionNeeded",
            "commandIds":[],"operationIds":[],"explanation":"The plan owns the remaining work"}),
        vec![],
    );
    assert_eq!(response.status, "ok", "{response:?}");
    f.finish_release(&coordinator);
    let invocations = f.engine.related("Invocation", "workId", text(&work, "id"));
    for workspace_id in [&other_workspace["id"], &workspace["id"]] {
        let worker = invocations
            .iter()
            .find(|invocation| {
                text(&invocation["dispatch"], "kind") == "ProduceResult"
                    && invocation["dispatch"]["workspaceId"] == *workspace_id
            })
            .unwrap();
        f.start_invocation(worker);
        f.acknowledge(worker, None);
        let mut capture_work = work.clone();
        capture_work["workspaceId"] = workspace_id.clone();
        let artifact = f.artifact(&capture_work);
        f.submit(worker, &artifact, None);
        f.finish_release(worker);
    }
    let independent_before = f
        .engine
        .related("Task", "workId", text(&work, "id"))
        .into_iter()
        .find(|task| text(task, "clientKey") == "independent")
        .unwrap();
    assert_eq!(independent_before["state"], "Accepted");
    let result_before = f
        .engine
        .record(text(&independent_before, "acceptedResultId"), "TaskResult")
        .unwrap();
    let other_before = f
        .engine
        .record(text(&other_workspace, "id"), "Workspace")
        .unwrap();
    let current = f.engine.record(text(&work, "id"), "Work").unwrap();
    let response = f.command(
        Principal::Human,
        "work.control",
        json!({"workId":work["id"],"action":"Hold"}),
        vec![current],
    );
    assert_eq!(response.status, "ok", "{response:?}");
    let workspace = f
        .engine
        .record(text(&workspace, "id"), "Workspace")
        .unwrap();
    let response = f.command(
        Principal::Human,
        "workspace.takeover",
        json!({"workspaceId":workspace["id"]}),
        vec![workspace.clone()],
    );
    assert_eq!(response.status, "pending", "{response:?}");
    let workspace = f
        .engine
        .record(text(&workspace, "id"), "Workspace")
        .unwrap();
    assert_eq!(workspace["writer"], "Human");
    let response = f.command(
        Principal::Human,
        "workspace.handback",
        json!({"workspaceId":workspace["id"],"summary":"Edited only the integration workspace",
            "resumeAffected":false}),
        vec![workspace],
    );
    assert_eq!(response.status, "pending", "{response:?}");
    let manual = f.root.join("handback-snapshot");
    std::fs::create_dir_all(&manual).unwrap();
    std::fs::write(manual.join("report.txt"), b"Human revised integration").unwrap();
    let captured = crate::agent_center::runtime::artifacts::capture(
        &f.root,
        &f.root,
        &json!({"kind":"Tree","relativePath":"handback-snapshot"}),
    )
    .unwrap();
    f.engine
        .complete_effect(
            response.operation_id.as_deref().unwrap(),
            Response::ok(id(), json!({"artifacts":[captured]})),
        )
        .unwrap();
    let current = f.engine.record(text(&work, "id"), "Work").unwrap();
    let plan = f.engine.record(text(&current, "planId"), "Plan").unwrap();
    let tasks = values(&plan, "tasks");
    assert_eq!(tasks.len(), 2);
    let retained = tasks
        .iter()
        .find(|task| task["id"] == independent_before["id"])
        .unwrap();
    assert_eq!(retained, &independent_before);
    assert_eq!(
        f.engine
            .record(text(&independent_before, "id"), "Task")
            .unwrap(),
        independent_before
    );
    assert_eq!(
        f.engine
            .record(text(&result_before, "id"), "TaskResult")
            .unwrap(),
        result_before
    );
    assert_eq!(
        f.engine
            .record(text(&other_before, "id"), "Workspace")
            .unwrap(),
        other_before
    );
    let changed = tasks
        .iter()
        .find(|task| text(task, "clientKey") == "report")
        .unwrap();
    assert_eq!(changed["revision"], 2);
    assert!(values(&changed["contract"], "inputSlots")
        .iter()
        .any(|input| text(input, "slot") == "manual-contribution"));
}
