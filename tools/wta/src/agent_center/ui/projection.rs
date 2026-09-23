// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

use serde_json::Value;

pub(super) fn text<'a>(value: &'a Value, field: &str) -> &'a str {
    value[field].as_str().unwrap_or("")
}

pub(super) fn goal(view: &Value) -> String {
    view.pointer("/spec/goal")
        .or_else(|| view.pointer("/work/spec/goal"))
        .or_else(|| view.pointer("/work/goal"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| t!("agent_center.console_unknown_work").into_owned())
}

pub(super) fn project(project: &Value) -> String {
    let name = text(project, "name");
    let root = text(project, "root");
    if name.is_empty() {
        t!("agent_center.console_unknown_project").into_owned()
    } else if root.is_empty() {
        name.into()
    } else {
        format!("{name} · {root}")
    }
}

pub(super) fn status(value: &Value) -> String {
    match value.as_str().unwrap_or("") {
        "Draft" => t!("agent_center.console_draft"),
        "Active" => t!("agent_center.console_active"),
        "Completed" => t!("agent_center.console_completed"),
        "Cancelled" => t!("agent_center.console_cancelled"),
        "Hold" | "Paused" => t!("agent_center.console_paused"),
        "Automatic" | "Auto" | "Advance" => t!("agent_center.console_automatic"),
        "Running" | "Executing" => t!("agent_center.console_running"),
        "Pending" | "Queued" | "Proposed" | "Ready" => t!("agent_center.console_pending"),
        "Accepted" | "Succeeded" | "Passed" => t!("agent_center.console_verified"),
        "Failed" | "Rejected" | "Error" => t!("agent_center.console_failed"),
        "Open" | "Blocked" | "Waiting" | "WaitingInput" => t!("agent_center.console_waiting"),
        "RepairRequired" | "Unknown" | "OutcomeUnknown" => t!("agent_center.console_unknown"),
        "Answered" | "Resolved" | "Applied" => t!("agent_center.console_resolved"),
        "Superseded" => t!("agent_center.console_superseded"),
        _ => t!("agent_center.console_unknown"),
    }
    .into_owned()
}

fn strings(value: &Value) -> String {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

fn section(lines: &mut Vec<String>, label: &str, content: String) {
    if !content.is_empty() {
        lines.push(format!("{label}\n{content}"));
    }
}

pub(super) fn brief(view: &Value) -> String {
    format!("{}\n\n{}", agreement(view), next(view))
}

pub(super) fn agreement(view: &Value) -> String {
    let work = &view["work"];
    let spec = &view["spec"];
    let mut lines = vec![goal(view), status(&work["lifecycle"])];
    if !work["desiredAdvancement"].is_null() {
        lines.push(status(&work["desiredAdvancement"]));
    }
    section(
        &mut lines,
        &t!("agent_center.console_scope"),
        strings(&spec["scope"]),
    );
    section(
        &mut lines,
        &t!("agent_center.console_exclusions"),
        strings(&spec["exclusions"]),
    );
    section(
        &mut lines,
        &t!("agent_center.console_criteria"),
        spec["criteria"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .map(|item| {
                        format!(
                            "• {}\n  {}",
                            text(item, "description"),
                            text(item, "evidenceRule")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default(),
    );
    section(
        &mut lines,
        &t!("agent_center.console_delivery"),
        readable(&spec["delivery"]),
    );
    if let Some(tasks) = view["taskSummaries"].as_array() {
        section(
            &mut lines,
            &t!("agent_center.console_progress"),
            tasks
                .iter()
                .map(|task| {
                    format!(
                        "• {} · {}",
                        text(task, "objective"),
                        status(
                            task.get("state")
                                .or_else(|| task.get("status"))
                                .unwrap_or(&Value::Null)
                        )
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    lines.join("\n\n")
}

pub(super) fn next(view: &Value) -> String {
    let work = &view["work"];
    if work["lifecycle"] == "Completed" {
        t!("agent_center.console_next_done")
    } else if work["lifecycle"] == "Cancelled" {
        t!("agent_center.console_next_cancelled")
    } else if work["lifecycle"] == "Draft" {
        t!("agent_center.console_next_approve")
    } else if view["candidate"]["status"] == "Proposed" {
        t!("agent_center.console_next_accept")
    } else if view["obligations"]
        .as_array()
        .is_some_and(|items| !items.is_empty())
    {
        t!("agent_center.console_next_attention")
    } else if work["desiredAdvancement"] == "Hold" {
        t!("agent_center.console_next_resume")
    } else if work["lifecycle"] == "Active" {
        t!("agent_center.console_next_service")
    } else {
        t!("agent_center.console_unknown")
    }
    .into_owned()
}

/// Project known human-authored fields, not a redacted serialization. User text,
/// actual paths and failures are never rewritten, including IDs inside that text.
pub(super) fn readable(value: &Value) -> String {
    if let Some(text) = value.as_str() {
        return text.into();
    }
    if let Some(items) = value.as_array() {
        return items
            .iter()
            .map(readable)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
    }
    let mut lines = Vec::new();
    match value["kind"].as_str() {
        Some("LocalCode") => lines.push(t!("agent_center.console_delivery_code").into_owned()),
        Some("Report") => lines.push(t!("agent_center.console_delivery_report").into_owned()),
        _ => {}
    }
    for key in [
        "goal",
        "question",
        "reason",
        "message",
        "description",
        "summary",
        "text",
        "explanation",
        "requestedChange",
        "claim",
        "impact",
        "label",
        "objective",
        "localPath",
        "localRoot",
        "root",
        "path",
        "branch",
        "commit",
        "headCommit",
        "workingDirectory",
        "repositoryRoot",
        "targetBranch",
        "reportPaths",
        "knownLimitations",
        "knownGaps",
        "scope",
        "exclusions",
        "evidenceRule",
        "nextAction",
    ] {
        if let Some(field) = value.get(key) {
            let content = readable(field);
            if !content.is_empty() {
                lines.push(content);
            }
        }
    }
    for key in [
        "status",
        "state",
        "outcome",
        "disposition",
        "recommendation",
    ] {
        if let Some(field) = value.get(key) {
            lines.push(status(field));
        }
    }
    for key in [
        "destination",
        "candidate",
        "failure",
        "fieldErrors",
        "body",
        "result",
        "gates",
        "reviews",
        "criterionMappings",
        "criterionEvidence",
        "evidence",
        "inspection",
        "workspace",
        "content",
        "artifacts",
        "outputs",
    ] {
        if let Some(field) = value.get(key) {
            let content = readable(field);
            if !content.is_empty() {
                lines.push(content);
            }
        }
    }
    if let Some(recipes) = value["runRecipes"].as_array() {
        for recipe in recipes {
            lines.push(format!(
                "{} {} · {}",
                text(recipe, "executable"),
                recipe["args"]
                    .as_array()
                    .map(|args| args
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" "))
                    .unwrap_or_default(),
                text(recipe, "cwdRelative")
            ));
        }
    }
    lines.join("\n")
}

pub(super) fn confirmation(preview: &Value, operation: &super::Operation, target: &str) -> String {
    let mut lines = vec![
        target.into(),
        super::workflow::method_label(&operation.method, &operation.params),
    ];
    if operation.method == "work.claim_executor" {
        lines.push(t!("agent_center.task_claim_executor_confirm").into_owned());
    }
    if matches!(
        operation.method.as_str(),
        "work.continue" | "work.claim_executor"
    ) && operation.params["restartSession"] == true
    {
        lines.push(t!("agent_center.task_new_session_confirm").into_owned());
    }
    if preview.pointer("/work/work").is_some() {
        lines.push(brief(&preview["work"]));
    }
    if let Some(project) = preview.get("project") {
        lines.push(self::project(project.get("project").unwrap_or(project)));
    }
    if operation.method == "project.configure" && operation.params["createDirectory"] == true {
        lines.push(t!("agent_center.console_project_create_directory").into_owned());
    }
    if let Some(grant) = preview.pointer("/grant/data/proposal") {
        lines.push(t!("agent_center.console_authority").into_owned());
        // These are authorization facts, not protocol record identifiers.
        for (key, label) in [
            (
                "allowedCapabilities",
                t!("agent_center.console_capabilities"),
            ),
            ("dataScopes", t!("agent_center.console_data_scopes")),
            ("limits", t!("agent_center.console_limits")),
        ] {
            if let Some(value) = grant.get(key) {
                lines.push(format!("{label}: {}", scalar_fields(value)));
            }
            lines.push(t!("agent_center.console_managed_writes").into_owned());
        }
    }
    lines.push(readable(preview));
    if let Some(value) = operation.params.get("value") {
        let answer = preview["options"]
            .as_array()
            .and_then(|options| options.iter().find(|option| option["id"] == *value))
            .map(|option| format!("{} — {}", text(option, "label"), text(option, "impact")))
            .unwrap_or_else(|| scalar_fields(value));
        lines.push(t!("agent_center.console_answer_review", value = answer).into_owned());
    }
    lines.push(t!("agent_center.console_frozen").into_owned());
    lines.push(t!("agent_center.confirm_prompt").into_owned());
    lines
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

pub(super) fn scalar_fields(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .map(scalar_fields)
            .collect::<Vec<_>>()
            .join(", "),
        Value::Object(fields) => fields
            .iter()
            .map(|(key, value)| format!("{key}: {}", scalar_fields(value)))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn project_creation_confirmation_discloses_exact_path_and_git_initialization() {
        let _locale = crate::test_support::lock_locale();
        let path = r"C:\Code\3d-human-website";
        let mut operation = super::super::Operation::read(
            "project.configure",
            json!({"root":path,"createDirectory":true}),
        );
        let preview = json!({"project":{"name":"3D human website","root":path}});
        let creating = confirmation(&preview, &operation, "New project");
        assert!(creating.contains(path));
        let disclosure = t!("agent_center.console_project_create_directory");
        assert!(creating.contains(disclosure.as_ref()));
        assert!(disclosure.contains("Git"));
        assert!(!disclosure.contains("agent_center."));
        operation.params["createDirectory"] = json!(false);
        let existing = confirmation(&preview, &operation, "Existing project");
        assert!(existing.contains(path));
        assert!(!existing.contains(disclosure.as_ref()));
    }

    #[test]
    fn default_projection_is_readable_but_user_text_paths_and_evidence_are_not_scrubbed() {
        let _locale = crate::test_support::lock_locale();
        let view = json!({"work":{"id":"opaque-work-id","lifecycle":"Active","desiredAdvancement":"Advance"},
                "spec":{"goal":"Keep literal opaque-user-id in C:\\Reports","scope":["Fix report"],"criteria":[{"id":"opaque-criterion","description":"Check opens","evidenceRule":"Retain failing log"}]},
                "taskSummaries":[{"id":"opaque-task-id","objective":"Check report","state":"Running"}]});
        let readable = brief(&view);
        assert!(readable.contains("opaque-user-id") && readable.contains("C:\\Reports"));
        assert!(!readable.contains("opaque-work-id") && !readable.contains("opaque-task-id"));
        assert!(readable.contains("Running"));
        assert!(!readable.contains("Completed"));
    }

    #[test]
    fn proposed_or_unknown_observations_never_claim_final_completion() {
        let _locale = crate::test_support::lock_locale();
        for value in ["Pending", "Proposed", "FutureStatus", "Succeeded"] {
            assert!(!status(&json!(value)).contains("Completed"));
        }
        assert!(status(&json!("Completed")).contains("human acceptance"));
        let evidence = readable(
            &json!({"gates":[{"id":"gate-opaque","outcome":"Failed","explanation":"Actual check failed: C:\\Report\\check.log"}]}),
        );
        assert!(
            evidence.contains("Actual check failed") && evidence.contains("C:\\Report\\check.log")
        );
        assert!(!evidence.contains("gate-opaque"));
    }
}
