// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

use super::*;
use projection::{goal, project, text};

#[derive(Clone)]
pub(super) enum Choice {
    DemoInput(String),
    DemoDetails,
    DemoTokenCap,
    DemoCancel,
    Home,
    Overview,
    Attention,
    Projects,
    Refresh,
    SelectWork(String),
    OpenTab(String),
    SelectProject(String),
    Start(String),
    Control(Operation),
    Question(Value, Option<String>),
    Inspect(String),
    Evidence(Operation, String, InspectionIdentity),
    Accept(Operation, Value),
    HumanProposal(Value),
    Return(SelectionContext),
    Reconcile(String),
}

pub(super) struct Menu {
    pub items: Vec<(String, Choice)>,
    pub selected: usize,
}

pub(super) fn method_label(method: &str, params: &Value) -> String {
    match method {
        "work.continue" | "work.claim_executor" => {
            if params["restartSession"] == true {
                t!("agent_center.task_new_session")
            } else if method == "work.claim_executor" {
                t!("agent_center.task_claim_executor")
            } else {
                t!("agent_center.task_continue")
            }
        }
        "work.start" => t!("agent_center.console_start"),
        "work.control" => match params["action"].as_str() {
            Some("Hold") => t!("agent_center.console_pause"),
            Some("Resume") => t!("agent_center.console_resume"),
            _ => t!("agent_center.console_cancel"),
        },
        "delivery.accept" => t!("agent_center.console_accept"),
        "decision.answer" | "conversation.answer_input" => t!("agent_center.console_answer"),
        "workspace.takeover" => t!("agent_center.console_takeover"),
        "workspace.handback" => t!("agent_center.console_handback"),
        "work.apply_change" => t!("agent_center.console_apply"),
        _ => t!("agent_center.console_action"),
    }
    .into_owned()
}

impl State {
    pub(super) fn inline_approval(&self) -> bool {
        !self.task_list
            && !self.dashboard
            && !self.diagnostics
            && self
                .pending
                .as_ref()
                .is_some_and(|pending| pending.preview.get("humanActionProposal").is_some())
    }

    pub(super) fn defer_approval(&mut self, edit: bool) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        let proposal = &pending.preview["humanActionProposal"];
        self.deferred_proposals.insert(
            text(proposal, "id").into(),
            proposal["version"].as_u64().unwrap_or(0),
        );
        if edit {
            self.select(pending.work.clone());
        }
        self.notice = if edit {
            t!(
                "agent_center.approval_edit_hint",
                target = pending.target_label
            )
            .into_owned()
        } else {
            t!(
                "agent_center.approval_deferred",
                target = pending.target_label
            )
            .into_owned()
        };
        self.notice_kind = NoticeKind::Attention;
        self.focus = Focus::Composer;
        self.cards = None;
        self.retry = None;
    }

    pub(super) fn remember_human_proposal(&mut self, proposal: &Value) {
        if proposal["conversationId"] != self.context.conversation_id {
            return;
        }
        let id = text(proposal, "id");
        if uuid::Uuid::parse_str(id).is_ok()
            && proposal["version"].as_u64().is_some()
            && self
                .editor_view()
                .map(|view| &view.records)
                .and_then(|records| records.get(id))
                .is_none_or(|old| old["version"].as_u64() < proposal["version"].as_u64())
        {
            self.editor_view_mut()
                .records
                .insert(id.into(), proposal.clone());
        }
        if let Err(error) = self.human_proposal_operation(proposal) {
            self.notice = error.to_string();
            self.notice_kind = NoticeKind::Error;
            tracing::warn!(target: "agent_center::ui", error = %error, "Invalid human action proposal");
            return;
        }
    }

    fn human_proposal_operation(&self, proposal: &Value) -> Result<Operation> {
        anyhow::ensure!(
            proposal["kind"] == "HumanActionProposal"
                && proposal["conversationId"] == self.context.conversation_id
                && uuid::Uuid::parse_str(text(proposal, "id")).is_ok()
                && proposal["version"]
                    .as_u64()
                    .is_some_and(|version| version > 0)
                && matches!(
                    text(proposal, "status"),
                    "Open" | "Superseded" | "Submitted"
                )
                && proposal["summary"].is_string()
                && proposal["preview"].is_object(),
            "{}",
            t!("agent_center.invalid_request")
        );
        let request = &proposal["request"];
        let method = text(request, "method");
        anyhow::ensure!(
            matches!(
                method,
                "work.start"
                    | "work.control"
                    | "work.continue"
                    | "work.claim_executor"
                    | "work.apply_change"
                    | "delivery.accept"
                    | "delivery.request_changes"
                    | "project.configure"
            ),
            "{}",
            t!("agent_center.status_unsupported")
        );
        anyhow::ensure!(
            request["params"].is_object()
                && uuid::Uuid::parse_str(text(request, "commandId")).is_ok()
                && request["ifMatch"]
                    .as_array()
                    .is_some_and(|guards| guards.iter().all(|guard| guard["kind"]
                        .as_str()
                        .is_some_and(|kind| !kind.is_empty())
                        && guard["id"].as_str().is_some_and(|id| !id.is_empty())
                        && guard["version"].as_u64().is_some_and(|version| version > 0))),
            "{}",
            t!("agent_center.invalid_request")
        );
        let work = proposal["workId"].as_str();
        anyhow::ensure!(
            proposal["preview"]["project"].is_object()
                && proposal["preview"]["project"]["root"]
                    .as_str()
                    .is_some_and(|root| !root.is_empty()),
            "{}",
            t!("agent_center.invalid_request")
        );
        if method == "work.control" {
            anyhow::ensure!(
                matches!(
                    text(&request["params"], "action"),
                    "Hold" | "Resume" | "Cancel"
                ),
                "{}",
                t!("agent_center.status_unsupported")
            );
        }
        if method == "project.configure" {
            anyhow::ensure!(
                proposal["preview"]["project"]["limits"].is_object()
                    && proposal["preview"]["project"]["capabilityIds"].is_array()
                    && proposal["preview"]["approvedModelDestination"]
                        .as_str()
                        .is_some_and(|destination| !destination.is_empty()),
                "{}",
                t!("agent_center.invalid_request")
            );
        }
        anyhow::ensure!(
            work == proposal
                .pointer("/preview/work/work/id")
                .and_then(Value::as_str)
                && (method == "project.configure" || work.is_some()),
            "{}",
            t!("agent_center.invalid_request")
        );
        if let Some(work) = work {
            let expected = json!({"kind":"Work","id":work,"version":proposal["preview"]["work"]["work"]["version"]});
            anyhow::ensure!(
                request["ifMatch"]
                    .as_array()
                    .is_some_and(|guards| guards.contains(&expected)),
                "{}",
                t!("agent_center.invalid_request")
            );
        }
        if matches!(
            method,
            "work.start" | "work.control" | "work.continue" | "work.claim_executor"
        ) {
            anyhow::ensure!(
                request["params"]["workId"].as_str() == work,
                "{}",
                t!("agent_center.invalid_request")
            );
        }
        if matches!(method, "work.continue" | "work.claim_executor") {
            anyhow::ensure!(
                request["params"]
                    .get("restartSession")
                    .is_none_or(Value::is_boolean)
                    && (request["params"]["restartSession"] != true
                        || (proposal["preview"]["work"]["continuation"]["state"] == "Unavailable"
                            && proposal["preview"]["work"]["continuation"]["canRestartSession"]
                                == true)),
                "{}",
                t!("agent_center.invalid_request")
            );
        }
        if matches!(method, "delivery.accept" | "delivery.request_changes") {
            let candidate = &proposal["preview"]["candidate"];
            let expected = json!({"kind":"DeliveryCandidate","id":candidate["id"],"version":candidate["version"]});
            anyhow::ensure!(
                candidate["id"].is_string()
                    && candidate["workId"].as_str() == work
                    && candidate["id"] == request["params"]["candidateId"]
                    && request["ifMatch"]
                        .as_array()
                        .is_some_and(|guards| guards.contains(&expected)),
                "{}",
                t!("agent_center.invalid_request")
            );
        }
        if matches!(method, "work.start" | "work.apply_change") {
            let grant = &proposal["preview"]["grant"]["data"]["proposal"];
            anyhow::ensure!(
                grant.is_object()
                    && grant["id"] == request["params"]["grantProposalId"]
                    && grant["workId"].as_str() == work
                    && grant.get("limits").is_some()
                    && grant.get("allowedCapabilities").is_some()
                    && grant.get("dataScopes").is_some(),
                "{}",
                t!("agent_center.invalid_request")
            );
        }
        if method == "work.apply_change" {
            let change = &proposal["preview"]["changeProposal"];
            let expected =
                json!({"kind":"ChangeProposal","id":change["id"],"version":change["version"]});
            anyhow::ensure!(
                change["replacementSpec"].is_object()
                    && change["id"].is_string()
                    && change["id"] == request["params"]["proposalId"]
                    && request["ifMatch"]
                        .as_array()
                        .is_some_and(|guards| guards.contains(&expected)),
                "{}",
                t!("agent_center.invalid_request")
            );
        }
        Ok(Operation {
            method: method.into(),
            params: request["params"].clone(),
            if_match: request["ifMatch"]
                .as_array()
                .context("HumanActionProposal.request.ifMatch")?
                .clone(),
            command_id: text(request, "commandId").into(),
            mutation: true,
            confirmation: true,
        })
    }

    fn human_proposal_current(&self, proposal: &Value, operation: &Operation) -> bool {
        !self.stale
            && proposal["status"] == "Open"
            && self
                .editor_view()
                .and_then(|view| view.records.get(text(proposal, "id")))
                == Some(proposal)
            && self
                .context
                .versions
                .get(&("HumanActionProposal".into(), text(proposal, "id").into()))
                .is_none_or(|version| proposal["version"].as_u64() == Some(*version))
            && self
                .human_proposal_operation(proposal)
                .is_ok_and(|original| operation_preview(&original) == operation_preview(operation))
            && operation.if_match.iter().all(|guard| {
                self.context
                    .versions
                    .get(&(text(guard, "kind").into(), text(guard, "id").into()))
                    .is_none_or(|version| guard["version"].as_u64() == Some(*version))
            })
    }

    fn human_proposal_inspected(&self, proposal: &Value, operation: &Operation) -> bool {
        let work = proposal["workId"].as_str();
        let Some(view) = self
            .works
            .iter()
            .find(|view| view["work"]["id"].as_str() == work)
        else {
            return false;
        };
        let Some(inspection) = self
            .views
            .get(&work.map(str::to_owned))
            .and_then(|view| view.inspection.as_ref())
        else {
            return false;
        };
        let Ok(accepted) = acceptance(view, inspection) else {
            return false;
        };
        accepted.params == operation.params
            && accepted.if_match.len() == operation.if_match.len()
            && accepted
                .if_match
                .iter()
                .all(|guard| operation.if_match.contains(guard))
            && inspection["candidate"] == proposal["preview"]["candidate"]
            && inspection["work"] == proposal["preview"]["work"]["work"]
    }

    pub(super) fn human_proposal_target(&self, proposal: &Value) -> String {
        if let Some(work) = proposal["workId"].as_str() {
            if self.works.iter().any(|view| view["work"]["id"] == work) {
                self.work_label(Some(work))
            } else {
                format!(
                    "{} · {}",
                    goal(&proposal["preview"]["work"]),
                    project(&proposal["preview"]["project"])
                )
            }
        } else if proposal["preview"]["project"].is_object() {
            project(&proposal["preview"]["project"])
        } else {
            t!("agent_center.chat_global").into_owned()
        }
    }

    pub(super) fn work_label(&self, id: Option<&str>) -> String {
        let Some(id) = id else {
            return if self.context.global_conversation {
                t!("agent_center.chat_global").into_owned()
            } else {
                t!("agent_center.console_home").into_owned()
            };
        };
        let Some(view) = self.works.iter().find(|view| view["work"]["id"] == id) else {
            return t!("agent_center.console_unknown_work").into_owned();
        };
        let project_id = text(&view["work"], "projectId");
        let project_label = self.project_label(Some(project_id));
        let label = format!("{} · {project_label}", goal(view));
        let duplicates = self
            .works
            .iter()
            .filter(|other| goal(other) == goal(view) && other["work"]["projectId"] == project_id)
            .count();
        if duplicates > 1 {
            let position = self
                .works
                .iter()
                .filter(|other| {
                    goal(other) == goal(view) && other["work"]["projectId"] == project_id
                })
                .position(|other| other["work"]["id"] == id)
                .unwrap_or(0)
                + 1;
            t!(
                "agent_center.console_duplicate",
                work = label,
                number = position,
                created = text(&view["work"], "createdAt")
            )
            .into_owned()
        } else {
            label
        }
    }

    pub(super) fn project_label(&self, id: Option<&str>) -> String {
        self.projects
            .iter()
            .find(|project| project["id"].as_str() == id)
            .map(project)
            .unwrap_or_else(|| t!("agent_center.console_unknown_project").into_owned())
    }

    pub(super) fn actions(&self) -> Menu {
        let mut items = vec![
            (t!("agent_center.chat_global").into_owned(), Choice::Home),
            (
                t!("agent_center.console_projects").into_owned(),
                Choice::Projects,
            ),
            (
                t!("agent_center.console_refresh").into_owned(),
                Choice::Refresh,
            ),
            (t!("agent_center.task_all").into_owned(), Choice::Overview),
            (
                t!("agent_center.task_attention").into_owned(),
                Choice::Attention,
            ),
        ];
        if let Some(previous) = &self.return_work {
            items.push((
                t!(
                    "agent_center.console_return",
                    work = self.work_label(previous.work.as_deref())
                )
                .into_owned(),
                Choice::Return(previous.clone()),
            ));
        }
        if !self.stale {
            for (index, mutation) in self
                .mutations
                .values()
                .filter(|mutation| mutation.unknown.is_some())
                .enumerate()
            {
                items.push((
                    format!(
                        "{} · {} · {} ({})",
                        t!("setup.option.retry_detection"),
                        mutation.target_label,
                        t!("agent_center.console_unknown"),
                        index + 1
                    ),
                    Choice::Reconcile(mutation.operation.command_id.clone()),
                ));
            }
        }
        for view in &self.works {
            if let Some(id) = view["work"]["id"].as_str() {
                items.push((
                    t!(
                        "agent_center.console_open",
                        work = self.work_label(Some(id))
                    )
                    .into_owned(),
                    Choice::SelectWork(id.into()),
                ));
            }
        }
        // Decisions are global attention actions, but they capture their owner
        // without selecting it or modifying the current work's text editor.
        for (work, view) in &self.views {
            for question in view.records.values().filter(|record| {
                matches!(text(record, "kind"), "IntakeRequest" | "DecisionRequest")
                    && record["status"] == "Open"
            }) {
                items.push((
                    t!(
                        "agent_center.console_respond",
                        work = self.work_label(work.as_deref()),
                        question = text(question, "question")
                    )
                    .into_owned(),
                    Choice::Question(question.clone(), work.clone()),
                ));
            }
        }
        if self.stale {
            return Menu { items, selected: 0 };
        }
        if let Some(editor) = self.editor_view() {
            for proposal in editor.records.values().filter(|record| {
                record["kind"] == "HumanActionProposal" && record["status"] == "Open"
            }) {
                if let Ok(operation) = self.human_proposal_operation(proposal) {
                    if !self.human_proposal_current(proposal, &operation) {
                        continue;
                    }
                    let label = if operation.method == "delivery.accept"
                        && !self.human_proposal_inspected(proposal, &operation)
                    {
                        t!("agent_center.console_inspect").into_owned()
                    } else {
                        method_label(&operation.method, &operation.params)
                    };
                    items.push((
                        format!(
                            "{label} · {} · {}",
                            self.human_proposal_target(proposal),
                            text(proposal, "summary")
                        ),
                        Choice::HumanProposal(proposal.clone()),
                    ));
                }
            }
        }
        if self.task_list {
            return Menu { items, selected: 0 };
        }
        if let Some(id) = self.context.work_id.as_deref() {
            if let Some(view) = self.works.iter().find(|view| view["work"]["id"] == id) {
                let work = &view["work"];
                let label = self.work_label(Some(id));
                items.push((
                    t!("agent_center.task_open_tab").into_owned(),
                    Choice::OpenTab(id.into()),
                ));
                if let Ok(operation) = self.continue_operation(false) {
                    items.push((
                        method_label(&operation.method, &operation.params),
                        Choice::Control(operation),
                    ));
                } else if let Ok(operation) = self.continue_operation(true) {
                    items.push((
                        t!("agent_center.task_new_session").into_owned(),
                        Choice::Control(operation),
                    ));
                }
                if work["lifecycle"] == "Draft" {
                    items.push((
                        format!("{} · {label}", t!("agent_center.console_start")),
                        Choice::Start(id.into()),
                    ));
                    if let Ok(operation) = control(work, "Cancel") {
                        items.push((
                            format!("{} · {label}", t!("agent_center.console_cancel")),
                            Choice::Control(operation),
                        ));
                    }
                } else if work["lifecycle"] == "Active" {
                    for action in ["Hold", "Cancel"] {
                        if let Ok(operation) = control(work, action) {
                            items.push((
                                format!(
                                    "{} · {label}",
                                    method_label(&operation.method, &operation.params)
                                ),
                                Choice::Control(operation),
                            ));
                        }
                    }
                }
                if work["currentCandidateId"].is_string() {
                    items.push((
                        format!("{} · {label}", t!("agent_center.console_inspect")),
                        Choice::Inspect(id.into()),
                    ));
                }
                if let Some(inspection) = self
                    .views
                    .get(&Some(id.into()))
                    .and_then(|view| view.inspection.as_ref())
                {
                    if let Ok(operation) = acceptance(view, inspection) {
                        items.push((
                            format!("{} · {label}", t!("agent_center.console_accept")),
                            Choice::Accept(operation, inspection.clone()),
                        ));
                    }
                    if let (Some(artifacts), Some(identity)) = (
                        inspection["artifacts"].as_array(),
                        InspectionIdentity::from_view(inspection),
                    ) {
                        for artifact in artifacts {
                            if let Some(entries) = artifact
                                .pointer("/manifest/entries")
                                .and_then(Value::as_array)
                            {
                                for entry in
                                    entries.iter().filter(|entry| entry["kind"] != "Directory")
                                {
                                    if let (Some(artifact_id), Some(path)) =
                                        (artifact["id"].as_str(), entry["path"].as_str())
                                    {
                                        let operation = Operation::read(
                                            "artifact.read",
                                            json!({"artifactId":artifact_id,"relativePath":path,"limit":16384}),
                                        );
                                        items.push((
                                            t!(
                                                "agent_center.console_read_evidence",
                                                work = &label,
                                                path = path
                                            )
                                            .into_owned(),
                                            Choice::Evidence(
                                                operation,
                                                id.into(),
                                                identity.clone(),
                                            ),
                                        ));
                                    }
                                }
                            }
                        }
                    }
                    if let Some(page) = self
                        .views
                        .get(&Some(id.into()))
                        .and_then(|view| view.evidence.as_ref())
                    {
                        if InspectionIdentity::from_view(inspection).as_ref()
                            == Some(&page.inspection)
                            && page.data["eof"] == false
                            && page.data["nextOffset"].is_u64()
                        {
                            let mut params = json!({"artifactId":page.data["artifactId"],"offset":page.data["nextOffset"],"limit":16384});
                            if let Some(path) = page.data["relativePath"].as_str() {
                                params["relativePath"] = json!(path);
                            }
                            items.push((
                                t!("agent_center.console_more_evidence", work = &label)
                                    .into_owned(),
                                Choice::Evidence(
                                    Operation::read("artifact.read", params),
                                    id.into(),
                                    page.inspection.clone(),
                                ),
                            ));
                        }
                    }
                }
            }
        }
        Menu { items, selected: 0 }
    }

    pub(super) fn project_menu(&self) -> Menu {
        Menu {
            items: self
                .projects
                .iter()
                .filter_map(|value| {
                    Some((
                        project(value),
                        Choice::SelectProject(value["id"].as_str()?.into()),
                    ))
                })
                .collect(),
            selected: 0,
        }
    }

    pub(super) fn activate(
        &mut self,
        choice: Choice,
        jobs: &mpsc::UnboundedSender<Job>,
    ) -> Result<()> {
        self.menu = None;
        self.cards = None;
        self.focus = Focus::Composer;
        let selected_work = self.context.work_id.clone();
        match choice {
            Choice::DemoInput(_)
            | Choice::DemoDetails
            | Choice::DemoTokenCap
            | Choice::DemoCancel => {
                bail!("Scripted demo actions must use the isolated demo router");
            }
            Choice::Overview | Choice::Attention => {
                let attention = matches!(choice, Choice::Attention);
                self.task_list = true;
                self.task_index = 0;
                self.dashboard = false;
                self.home_screen = if attention {
                    HomeScreen::Attention
                } else {
                    HomeScreen::Overview
                };
            }
            Choice::Home => {
                self.task_list = false;
                self.diagnostics = false;
                if !self.context.global_conversation {
                    self.sync_view_context();
                    let global = self.global_selection.clone();
                    self.context.global_conversation = true;
                    self.context.work_id = None;
                    self.context.project_id = global.project;
                    self.context.conversation_id = global.conversation;
                    self.context.console_session_id = global.console;
                    self.context.context_version = global.version;
                }
                if self.context.global_conversation {
                    self.dashboard = false;
                    self.view_mut().details_open = false;
                    self.view_mut().show_delivery = false;
                    return Ok(());
                }
                self.select(None);
                // A new intake never borrows the selected A conversation.
                self.context.conversation_id = uuid::Uuid::new_v4().to_string();
                self.select_console_binding();
                self.conversations.insert(
                    (self.context.project_id.clone(), None),
                    self.context.conversation_id.clone(),
                );
                self.sync_view_context();
                self.notice = t!("agent_center.console_new_hint").into_owned();
                self.notice_kind = NoticeKind::Info;
            }
            Choice::Projects => {
                self.menu = Some(self.project_menu());
                self.notice = t!("agent_center.console_projects_hint").into_owned();
                self.notice_kind = NoticeKind::Info;
            }
            Choice::Refresh => {
                if self.stale {
                    self.reconnect_requested = true;
                } else {
                    queue_captured(self, jobs, JobKind::Refresh, selected_work, String::new())?;
                }
            }
            Choice::SelectWork(id) => queue_captured(
                self,
                jobs,
                JobKind::SelectWork(id),
                selected_work,
                String::new(),
            )?,
            Choice::OpenTab(id) => queue_captured(
                self,
                jobs,
                JobKind::OpenTab(id),
                selected_work,
                String::new(),
            )?,
            Choice::SelectProject(id) => {
                // Choosing a new project is explicitly a new-work operation.
                self.select(None);
                queue_captured(self, jobs, JobKind::SelectProject(id), None, String::new())?;
            }
            Choice::Start(id) => queue_captured(
                self,
                jobs,
                JobKind::PrepareStart(id.clone()),
                Some(id),
                String::new(),
            )?,
            Choice::Control(operation) => {
                let work = operation.params["workId"].as_str().map(str::to_owned);
                if operation.confirmation {
                    let preview = self
                        .works
                        .iter()
                        .find(|view| view["work"]["id"].as_str() == work.as_deref())
                        .cloned()
                        .unwrap_or(Value::Null);
                    let target_label = self.work_label(work.as_deref());
                    self.pending = Some(PendingConfirmation {
                        operation,
                        preview: json!({"work":preview}),
                        work,
                        input: String::new(),
                        scroll: 0,
                        target_label,
                        origin: MutationOrigin::Action,
                    });
                } else {
                    queue_captured(self, jobs, JobKind::Send(operation), work, String::new())?;
                }
            }
            Choice::Question(question, work) => {
                anyhow::ensure!(!self.stale, "{}", t!("agent_center.console_stale"));
                self.form = Some(form::Form::new(question, work)?);
            }
            Choice::Inspect(id) => queue_captured(
                self,
                jobs,
                JobKind::Inspect(id.clone()),
                Some(id),
                String::new(),
            )?,
            Choice::Evidence(operation, id, inspection) => queue_captured(
                self,
                jobs,
                JobKind::Evidence {
                    operation,
                    inspection,
                },
                Some(id),
                String::new(),
            )?,
            Choice::Accept(operation, preview) => {
                self.pending = Some(PendingConfirmation {
                    operation,
                    preview,
                    work: self.context.work_id.clone(),
                    input: String::new(),
                    scroll: 0,
                    target_label: self.work_label(self.context.work_id.as_deref()),
                    origin: MutationOrigin::Action,
                });
            }
            Choice::HumanProposal(proposal) => {
                let operation = self.human_proposal_operation(&proposal)?;
                anyhow::ensure!(
                    self.human_proposal_current(&proposal, &operation),
                    "{}",
                    t!("agent_center.status_conflict")
                );
                let work = proposal["workId"].as_str().map(str::to_owned);
                if operation.method == "delivery.accept"
                    && !self.human_proposal_inspected(&proposal, &operation)
                {
                    let id = work.context("HumanActionProposal.workId")?;
                    return queue_captured(
                        self,
                        jobs,
                        JobKind::Inspect(id.clone()),
                        Some(id),
                        String::new(),
                    );
                }
                let mut preview = proposal["preview"].clone();
                preview["humanActionProposal"] = proposal.clone();
                self.pending = Some(PendingConfirmation {
                    operation,
                    preview,
                    work,
                    input: String::new(),
                    scroll: 0,
                    target_label: self.human_proposal_target(&proposal),
                    origin: MutationOrigin::Action,
                });
                self.approval_choice = 0;
                self.approval_expanded = false;
                if self.inline_approval() {
                    self.focus = Focus::Actions;
                }
            }
            Choice::Return(selection) => {
                self.context.global_conversation = selection.work.is_none()
                    && selection.conversation == self.global_selection.conversation;
                self.restore_selection(selection);
                self.task_list = false;
                self.dashboard = false;
                self.return_work = None;
            }
            Choice::Reconcile(id) => {
                anyhow::ensure!(!self.stale, "{}", t!("agent_center.console_stale"));
                let mutation = self
                    .mutations
                    .get(&id)
                    .filter(|mutation| mutation.unknown.is_some())
                    .cloned()
                    .with_context(|| t!("agent_center.status_conflict").into_owned())?;
                if mutation.requires_confirmation {
                    self.pending = Some(PendingConfirmation {
                        operation: mutation.operation,
                        preview: mutation.preview,
                        work: mutation.selection.work,
                        input: mutation.input,
                        scroll: 0,
                        target_label: mutation.target_label,
                        origin: mutation.origin,
                    });
                } else {
                    queue_captured(
                        self,
                        jobs,
                        JobKind::Send(mutation.operation),
                        mutation.selection.work,
                        mutation.input,
                    )?;
                }
            }
        }
        Ok(())
    }

    pub(super) fn confirmation_current(&self, pending: &PendingConfirmation) -> bool {
        // An explicit retry after an uncertain send must retain its old guards:
        // the authority can return the original idempotent receipt even when
        // that very command already advanced the subject's version.
        let exact_retry = self
            .mutations
            .get(&pending.operation.command_id)
            .is_some_and(|mutation| {
                mutation.selection.work == pending.work
                    && mutation.input == pending.input
                    && operation_preview(&mutation.operation)
                        == operation_preview(&pending.operation)
            })
            || self.retry.as_ref().is_some_and(|(input, work, kind)| {
                matches!(kind, JobKind::Send(operation)
                if input == &pending.input
                    && work == &pending.work
                    && operation_preview(operation) == operation_preview(&pending.operation))
            });
        !self.stale
            && pending
                .preview
                .get("humanActionProposal")
                .is_none_or(|proposal| {
                    exact_retry
                        || (self.human_proposal_current(proposal, &pending.operation)
                            && (pending.operation.method != "delivery.accept"
                                || self.human_proposal_inspected(proposal, &pending.operation)))
                })
            && (exact_retry
                || pending.operation.if_match.iter().all(|guard| {
                    self.context
                        .versions
                        .get(&(text(guard, "kind").into(), text(guard, "id").into()))
                        .is_none_or(|version| guard["version"].as_u64() == Some(*version))
                }))
    }
}

pub(super) fn proposal_authority_details(preview: &Value) -> String {
    if preview.get("humanActionProposal").is_none() {
        return String::new();
    }
    let mut lines = Vec::new();
    let project = &preview["project"];
    for (value, label) in [
        (&project["limits"], t!("agent_center.console_limits")),
        (
            &project["capabilityIds"],
            t!("agent_center.console_capabilities"),
        ),
        (
            &preview["approvedModelDestination"],
            t!("agent_center.console_authority"),
        ),
    ] {
        if !value.is_null() {
            lines.push(format!("{label}: {}", projection::scalar_fields(value)));
        }
    }
    lines.push(projection::readable(project));
    if let Some(replacement) = preview.pointer("/changeProposal/replacementSpec") {
        lines.push(t!("agent_center.console_apply").into_owned());
        lines.push(projection::brief(&json!({"spec": replacement})));
    }
    if let Some(changes) = preview.get("requestedChanges") {
        lines.push(projection::readable(&changes["findings"]));
        lines.push(format!(
            "{}: {}",
            t!("agent_center.console_resume"),
            if changes["advance"] == true {
                t!("agent_center.console_yes")
            } else {
                t!("agent_center.console_no")
            }
        ));
    }
    lines
        .into_iter()
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn control(work: &Value, action: &str) -> Result<Operation> {
    let id = work["id"].as_str().context("Work.id")?;
    let version = work["version"]
        .as_u64()
        .filter(|v| *v > 0)
        .context("Work.version")?;
    Ok(Operation {
        method: "work.control".into(),
        params: json!({"workId":id,"action":action}),
        if_match: vec![json!({"kind":"Work","id":id,"version":version})],
        mutation: true,
        confirmation: action == "Cancel",
        command_id: uuid::Uuid::new_v4().to_string(),
    })
}

pub(super) fn acceptance(view: &Value, inspection: &Value) -> Result<Operation> {
    let work = &view["work"];
    let inspected = &inspection["work"];
    let candidate = &inspection["candidate"];
    anyhow::ensure!(
        work == inspected
            && view["candidate"] == *candidate
            && work["lifecycle"] == "Active"
            && work["currentCandidateId"] == candidate["id"]
            && candidate["workId"] == work["id"]
            && candidate["status"] == "Proposed",
        "{}",
        t!("agent_center.status_conflict")
    );
    let work_version = work["version"]
        .as_u64()
        .filter(|v| *v > 0)
        .context("Work.version")?;
    let candidate_version = candidate["version"]
        .as_u64()
        .filter(|v| *v > 0)
        .context("DeliveryCandidate.version")?;
    Ok(Operation {
        method: "delivery.accept".into(),
        params: json!({"candidateId":candidate["id"]}),
        if_match: vec![
            json!({"kind":"Work","id":work["id"],"version":work_version}),
            json!({"kind":"DeliveryCandidate","id":candidate["id"],"version":candidate_version}),
        ],
        mutation: true,
        confirmation: true,
        command_id: uuid::Uuid::new_v4().to_string(),
    })
}

pub(super) async fn inspect(client: &Client, id: &str) -> Result<Value> {
    let work = send(client, &Operation::read("work.get", json!({"workId":id}))).await?;
    anyhow::ensure!(
        status_exit_code(&work) == 0,
        "{}",
        projection::readable(&work["failure"])
    );
    let work = &work["data"]["work"];
    let candidate_id = work["currentCandidateId"]
        .as_str()
        .context("Work.currentCandidateId")?;
    let response = send(
        client,
        &Operation::read("delivery.get", json!({"candidateId":candidate_id})),
    )
    .await?;
    anyhow::ensure!(
        status_exit_code(&response) == 0,
        "{}",
        projection::readable(&response["failure"])
    );
    let candidate = &response["data"];
    anyhow::ensure!(
        candidate["workId"] == id && candidate["id"] == candidate_id,
        "{}",
        t!("agent_center.status_conflict")
    );
    let result_id = candidate["integrationResultId"]
        .as_str()
        .context("DeliveryCandidate.integrationResultId")?;
    let result = send(
        client,
        &Operation::read("result.get", json!({"resultId":result_id})),
    )
    .await?;
    anyhow::ensure!(
        status_exit_code(&result) == 0,
        "{}",
        projection::readable(&result["failure"])
    );
    let mut refs = BTreeSet::new();
    fn collect(value: &Value, refs: &mut BTreeSet<String>) {
        if let Some(id) = value["artifactId"].as_str() {
            refs.insert(id.into());
        }
        match value {
            Value::Object(fields) => fields.values().for_each(|value| collect(value, refs)),
            Value::Array(values) => values.iter().for_each(|value| collect(value, refs)),
            _ => {}
        }
    }
    collect(candidate, &mut refs);
    collect(&result["data"], &mut refs);
    let mut artifacts = Vec::new();
    for id in refs {
        let response = send(
            client,
            &Operation::read("artifact.get", json!({"artifactId":id})),
        )
        .await?;
        anyhow::ensure!(
            status_exit_code(&response) == 0,
            "{}",
            projection::readable(&response["failure"])
        );
        artifacts.push(response["data"].clone());
    }
    Ok(json!({"work":work,"candidate":candidate,"result":result["data"],"artifacts":artifacts}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn work(id: &str, version: u64) -> Value {
        json!({"work":{"id":id,"kind":"Work","projectId":"project-id","version":version,
            "lifecycle":"Active","desiredAdvancement":"Advance","currentCandidateId":"candidate-id"},
            "spec":{"goal":"Fix the report","scope":["Report rendering"],"criteria":[{"description":"Report opens","evidenceRule":"A verified check log"}]},
            "candidate":{"id":"candidate-id","workId":id,"version":2,"status":"Proposed"}})
    }

    fn state() -> State {
        let mut state = State::legacy();
        state.projects =
            vec![json!({"kind":"Project","id":"project-id","name":"Reports","root":"C:\\Reports"})];
        state.context.project_id = Some("project-id".into());
        state.observe(&json!({"data":{"items":[work("a", 3), work("b", 4)]}}));
        state.select(Some("a".into()));
        state
    }

    fn human_proposal(state: &State, method: &str) -> Value {
        let view = &state.works[0];
        let mut proposal = json!({
            "kind":"HumanActionProposal","id":uuid::Uuid::new_v4().to_string(),"version":1,
            "status":"Open","conversationId":state.context.conversation_id,"workId":"a",
            "summary":"Agent explanation only",
            "request":{"method":method,"commandId":uuid::Uuid::new_v4().to_string(),
                "params":{"workId":"a","action":"Hold"},
                "ifMatch":[{"kind":"Work","id":"a","version":3}]},
            "preview":{"work":view,"project":state.projects[0],"candidate":view["candidate"]}
        });
        if method == "delivery.accept" {
            proposal["request"]["params"] = json!({"candidateId":view["candidate"]["id"]});
            proposal["request"]["ifMatch"]
                .as_array_mut()
                .unwrap()
                .push(json!({"kind":"DeliveryCandidate","id":view["candidate"]["id"],"version":2}));
        }
        if method == "work.start" {
            proposal["request"]["params"] = json!({"workId":"a","grantProposalId":"grant-a",
                "specRevision":1,"policyRevision":1});
            proposal["preview"]["grant"] = json!({"data":{"proposal":{
                "id":"grant-a","workId":"a","allowedCapabilities":["approved-report"],
                "dataScopes":["C:\\Reports"],"limits":{"maxAttempts":2}}}});
            proposal["preview"]["destination"] =
                json!({"kind":"Report","path":"C:\\Managed\\report.md"});
        }
        proposal
    }

    #[test]
    fn human_continuation_proposals_keep_exact_guard_and_require_confirmation() {
        let _locale = crate::test_support::lock_locale();
        for method in ["work.continue", "work.claim_executor"] {
            let mut state = state();
            state.context.global_conversation = true;
            state.dashboard = false;
            let mut proposal = human_proposal(&state, method);
            proposal["request"]["params"] = json!({"workId":"a"});
            state.append(None, &proposal);
            let operation = state.human_proposal_operation(&proposal).unwrap();
            assert_eq!(operation.method, method);
            assert_eq!(
                operation.if_match,
                vec![json!({"kind":"Work","id":"a","version":3})]
            );
            assert!(operation.confirmation);
            let (jobs, mut receiver) = mpsc::unbounded_channel();
            state
                .activate(Choice::HumanProposal(proposal.clone()), &jobs)
                .unwrap();
            assert!(receiver.try_recv().is_err());
            assert_eq!(
                state.pending.as_ref().unwrap().operation.command_id,
                operation.command_id
            );
            let mut invalid = proposal.clone();
            invalid["request"]["ifMatch"][0]["version"] = json!(9);
            assert!(state.human_proposal_operation(&invalid).is_err());
            proposal["request"]["params"]["restartSession"] = json!(true);
            assert!(state.human_proposal_operation(&proposal).is_err());
            proposal["preview"]["work"]["continuation"] = json!({
                "state":"Unavailable","canRestartSession":true
            });
            let restart = state.human_proposal_operation(&proposal).unwrap();
            let preview = projection::confirmation(&proposal["preview"], &restart, "Selected task");
            assert!(preview.contains(t!("agent_center.task_new_session_confirm").as_ref()));
            assert!(restart.confirmation);
        }
    }

    #[test]
    fn human_proposals_are_correlated_cards_not_automatic_commands_or_chat_messages() {
        let _locale = crate::test_support::lock_locale();
        let mut state = state();
        state.context.global_conversation = true;
        state.works[0]["work"]["lifecycle"] = json!("Draft");
        let proposal = human_proposal(&state, "work.start");
        let mut foreign = proposal.clone();
        foreign["conversationId"] = json!("another-conversation");
        state.append(None, &foreign);
        assert!(state.chat.records.is_empty());
        state.append(
            None,
            &json!({"kind":"Conversation","id":state.context.conversation_id,
            "actionProposals":[proposal]}),
        );
        assert!(state.pending.is_none());
        assert!(state.chat.messages.is_empty());
        let choice = state
            .visible_actions()
            .items
            .into_iter()
            .find_map(|(_, choice)| matches!(&choice, Choice::HumanProposal(_)).then_some(choice))
            .unwrap();
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        state.chat.replace_draft("Still editing global text".into());
        state.select(Some("b".into()));
        state.activate(choice, &jobs).unwrap();
        let pending = state.pending.as_ref().unwrap();
        assert_eq!(pending.work.as_deref(), Some("a"));
        let visible =
            projection::confirmation(&pending.preview, &pending.operation, &pending.target_label);
        for fact in [
            "Fix the report",
            "approved-report",
            "maxAttempts: 2",
            "C:\\Reports",
            "C:\\Managed\\report.md",
        ] {
            assert!(visible.contains(fact), "missing authoritative fact: {fact}");
        }
        assert!(!visible.contains("Agent explanation only"));
        assert!(!visible.contains(text(&proposal["request"], "commandId")));
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(200, 200)).unwrap();
        handle_key(&mut state, KeyCode::F(12), KeyModifiers::NONE, &jobs).unwrap();
        terminal.draw(|frame| render(frame, &mut state)).unwrap();
        let diagnostic = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(diagnostic.contains(text(&proposal["request"], "commandId")));
        assert!(diagnostic.contains("grantProposalId"));
        handle_key(&mut state, KeyCode::F(12), KeyModifiers::NONE, &jobs).unwrap();
        handle_key(&mut state, KeyCode::Enter, KeyModifiers::NONE, &jobs).unwrap();
        assert!(receiver.try_recv().is_err());
        handle_key(&mut state, KeyCode::Enter, KeyModifiers::CONTROL, &jobs).unwrap();
        let JobKind::Send(operation) = receiver.try_recv().unwrap().kind else {
            panic!("expected explicit confirmation");
        };
        assert_eq!(operation.params, proposal["request"]["params"]);
        assert_eq!(operation.command_id, proposal["request"]["commandId"]);
        assert_eq!(json!(operation.if_match), proposal["request"]["ifMatch"]);
        assert_eq!(state.chat.draft, "Still editing global text");
    }

    #[test]
    fn inline_approval_preserves_chat_enter_and_freezes_explicit_start() {
        let _locale = crate::test_support::lock_locale();
        let mut state = state();
        state.context.global_conversation = true;
        state.dashboard = false;
        state.works[0]["work"]["lifecycle"] = json!("Draft");
        let proposal = human_proposal(&state, "work.start");
        state.append(None, &proposal);
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        state.chat.replace_draft("hi".into());
        handle_key(&mut state, KeyCode::Enter, KeyModifiers::NONE, &jobs).unwrap();
        let chat_job = receiver.try_recv().unwrap();
        let JobKind::Send(chat) = chat_job.kind else {
            panic!("expected chat submission");
        };
        assert_eq!(chat.method, "conversation.submit");
        assert!(state.pending.is_none());
        state.receive(Update {
            work: chat_job.work,
            input: chat_job.input,
            conversation: chat_job.conversation,
            result: Ok(Outcome::Mutation {
                command_id: chat.command_id,
                result: Ok(json!({"status":"ok","data":{}})),
            }),
        });
        state.chat.replace_draft("Keep this draft".into());
        state.select(Some("b".into()));
        handle_key(&mut state, KeyCode::F(6), KeyModifiers::NONE, &jobs).unwrap();
        assert!(state.inline_approval());
        assert_eq!(state.focus, Focus::Actions);
        assert_eq!(state.pending.as_ref().unwrap().work.as_deref(), Some("a"));
        assert!(receiver.try_recv().is_err());
        handle_key(&mut state, KeyCode::Enter, KeyModifiers::NONE, &jobs).unwrap();
        let job = receiver.try_recv().unwrap();
        let JobKind::Send(operation) = job.kind else {
            panic!("expected explicit start");
        };
        assert_eq!(operation.method, "work.start");
        assert_eq!(operation.params, proposal["request"]["params"]);
        assert_eq!(operation.command_id, proposal["request"]["commandId"]);
        assert_eq!(json!(operation.if_match), proposal["request"]["ifMatch"]);
        assert_eq!(state.chat.draft, "Keep this draft");
        state.receive(Update {
            work: job.work,
            input: job.input,
            conversation: job.conversation,
            result: Ok(Outcome::Mutation {
                command_id: operation.command_id,
                result: Ok(json!({"status":"pending","operationId":"start-operation","data":{}})),
            }),
        });
        assert!(state.pending.is_none());
        assert_eq!(state.focus, Focus::Composer);
        assert_eq!(state.chat.draft, "Keep this draft");
    }

    #[test]
    fn inline_edit_and_defer_preserve_drafts_without_mutating_or_hiding_new_versions() {
        let _locale = crate::test_support::lock_locale();
        for choice in 1..=2 {
            let mut state = state();
            state.context.global_conversation = true;
            state.dashboard = false;
            let proposal = human_proposal(&state, "work.control");
            state.append(None, &proposal);
            state.select(Some("b".into()));
            let (jobs, mut receiver) = mpsc::unbounded_channel();
            state.chat.replace_draft("My existing requirements".into());
            handle_key(&mut state, KeyCode::Left, KeyModifiers::SHIFT, &jobs).unwrap();
            let selection = state.chat.editor.selection();
            handle_key(&mut state, KeyCode::F(6), KeyModifiers::NONE, &jobs).unwrap();
            for _ in 0..choice {
                handle_key(&mut state, KeyCode::Right, KeyModifiers::NONE, &jobs).unwrap();
            }
            handle_key(&mut state, KeyCode::Enter, KeyModifiers::NONE, &jobs).unwrap();
            assert!(receiver.try_recv().is_err());
            assert!(state.pending.is_none());
            assert_eq!(state.focus, Focus::Composer);
            assert_eq!(state.chat.draft, "My existing requirements");
            assert_eq!(state.chat.editor.selection(), selection);
            assert_eq!(
                state.context.work_id.as_deref(),
                Some(if choice == 1 { "a" } else { "b" })
            );
            assert!(state.notice.contains("Fix the report"));
            assert!(state.visible_actions().items.is_empty());
            handle_key(&mut state, KeyCode::F(4), KeyModifiers::NONE, &jobs).unwrap();
            assert!(state
                .menu
                .as_ref()
                .unwrap()
                .items
                .iter()
                .any(|(_, choice)| {
                    matches!(choice, Choice::HumanProposal(value) if value["id"] == proposal["id"])
                }));
            handle_key(&mut state, KeyCode::Esc, KeyModifiers::NONE, &jobs).unwrap();
            let mut revised = proposal;
            revised["version"] = json!(2);
            state.append(None, &revised);
            assert_eq!(state.visible_actions().items.len(), 1);
            handle_key(&mut state, KeyCode::F(6), KeyModifiers::NONE, &jobs).unwrap();
            handle_key(&mut state, KeyCode::Esc, KeyModifiers::NONE, &jobs).unwrap();
            assert!(state.pending.is_none());
            assert!(state.visible_actions().items.is_empty());
            assert!(receiver.try_recv().is_err());
        }
    }

    #[test]
    fn inline_approval_renders_with_chat_and_keeps_authority_available_in_details() {
        let _locale = crate::test_support::lock_locale();
        let mut state = state();
        state.context.global_conversation = true;
        state.dashboard = false;
        state.works[0]["work"]["lifecycle"] = json!("Draft");
        let proposal = human_proposal(&state, "work.start");
        state.append(None, &proposal);
        state.chat.replace_draft("Keep my draft".into());
        state
            .chat
            .messages
            .push(json!({"id":"reply","role":"assistant","status":"Complete",
            "parts":[{"text":"Ready for your review."}]}));
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        let draw = |state: &mut State, width, height| {
            let mut terminal =
                Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| render(frame, state)).unwrap();
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
        };
        for width in [1, 8, 40, 80, 120] {
            for height in [1, 8, 24, 45] {
                let screen = draw(&mut state, width, height);
                if width >= 80 && height >= 24 {
                    for text in [
                        "Approval required",
                        "F6 review proposal",
                        "Approve and start",
                        "Change requirements",
                        "Not now",
                        "Fix the report",
                        "C:\\Reports",
                        "Ready for your review.",
                        "Keep my draft",
                    ] {
                        assert!(screen.contains(text), "missing {text} at {width}x{height}");
                    }
                }
            }
        }
        assert_eq!(state.focus, Focus::Composer);
        handle_key(&mut state, KeyCode::F(6), KeyModifiers::NONE, &jobs).unwrap();
        let frozen = operation_preview(&state.pending.as_ref().unwrap().operation);
        for width in [1, 8, 40, 80, 120] {
            for height in [1, 8, 24, 45] {
                let screen = draw(&mut state, width, height);
                if width >= 80 && height >= 24 {
                    for text in [
                        "Approval required",
                        "Approve and start",
                        "Change requirements",
                        "Not now",
                        "Ready for your review.",
                        "Keep my draft",
                    ] {
                        assert!(screen.contains(text), "missing {text} at {width}x{height}");
                    }
                }
            }
        }
        handle_key(&mut state, KeyCode::F(5), KeyModifiers::NONE, &jobs).unwrap();
        assert!(state.approval_expanded);
        let screen = draw(&mut state, 160, 100);
        for text in [
            "approved-report",
            "maxAttempts: 2",
            "C:\\Reports",
            "C:\\Managed\\report.md",
        ] {
            assert!(screen.contains(text), "missing authority: {text}");
        }
        assert_eq!(
            operation_preview(&state.pending.as_ref().unwrap().operation),
            frozen
        );
        assert!(receiver.try_recv().is_err());
        handle_key(&mut state, KeyCode::F(5), KeyModifiers::NONE, &jobs).unwrap();
        assert!(!state.approval_expanded);
    }

    #[test]
    fn multiple_inline_proposals_keep_the_selected_target_when_new_progress_arrives() {
        let _locale = crate::test_support::lock_locale();
        let mut state = state();
        state.context.global_conversation = true;
        state.dashboard = false;
        let first = human_proposal(&state, "work.control");
        let mut second = human_proposal(&state, "work.control");
        second["request"]["params"]["action"] = json!("Cancel");
        state.append(None, &first);
        state.append(None, &second);
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        state.chat.replace_draft("Existing draft".into());
        handle_key(&mut state, KeyCode::F(6), KeyModifiers::NONE, &jobs).unwrap();
        assert!(state.pending.is_none());
        handle_key(&mut state, KeyCode::Down, KeyModifiers::NONE, &jobs).unwrap();
        let Choice::HumanProposal(selected) = state.cards.as_ref().unwrap().items[1].1.clone()
        else {
            panic!("expected proposal card");
        };
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| render(frame, &mut state)).unwrap();
        let screen = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(screen.contains("Approval required 2/2"));
        handle_key(&mut state, KeyCode::Enter, KeyModifiers::NONE, &jobs).unwrap();
        assert!(state.inline_approval());
        assert!(receiver.try_recv().is_err());
        let frozen = operation_preview(&state.pending.as_ref().unwrap().operation);
        let third = human_proposal(&state, "work.control");
        state.append(None, &third);
        state.event(json!({"eventId":"background-progress","changes":[{
            "subject":{"kind":"ConversationItem","id":"progress","version":1},
            "view":{"id":"progress","kind":"ConversationItem","version":1,
                "conversationId":state.context.conversation_id,"role":"assistant",
                "status":"Streaming","parts":[{"text":"Unrelated progress"}]}}]}));
        assert_eq!(state.focus, Focus::Actions);
        assert_eq!(state.approval_choice, 0);
        assert_eq!(state.chat.draft, "Existing draft");
        assert_eq!(
            operation_preview(&state.pending.as_ref().unwrap().operation),
            frozen
        );
        state.stale = true;
        assert!(handle_key(&mut state, KeyCode::Enter, KeyModifiers::NONE, &jobs).is_err());
        assert!(receiver.try_recv().is_err());
        state.stale = false;
        handle_key(&mut state, KeyCode::Enter, KeyModifiers::NONE, &jobs).unwrap();
        let JobKind::Send(operation) = receiver.try_recv().unwrap().kind else {
            panic!("expected selected approval");
        };
        assert_eq!(operation.command_id, selected["request"]["commandId"]);
        assert_eq!(operation.params, selected["request"]["params"]);
    }

    #[test]
    fn human_proposal_supersession_blocks_captured_choices_and_unsent_confirmations() {
        let _locale = crate::test_support::lock_locale();
        let mut state = state();
        state.context.global_conversation = true;
        state.dashboard = false;
        let proposal = human_proposal(&state, "work.control");
        state.append(None, &proposal);
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        state
            .activate(Choice::HumanProposal(proposal.clone()), &jobs)
            .unwrap();
        let frozen = operation_preview(&state.pending.as_ref().unwrap().operation);
        let mut newer = proposal.clone();
        newer["status"] = json!("Superseded");
        newer["version"] = json!(2);
        state.event(
            json!({"eventId":"superseded","changes":[{"subject":{"kind":"HumanActionProposal",
            "id":newer["id"],"version":2},"view":newer}]}),
        );
        state.append(None, &proposal);
        assert!(!state.confirmation_current(state.pending.as_ref().unwrap()));
        assert!(handle_key(&mut state, KeyCode::Enter, KeyModifiers::NONE, &jobs).is_err());
        assert!(handle_key(&mut state, KeyCode::Enter, KeyModifiers::CONTROL, &jobs).is_err());
        assert!(receiver.try_recv().is_err());
        assert_eq!(
            operation_preview(&state.pending.as_ref().unwrap().operation),
            frozen
        );
        handle_key(&mut state, KeyCode::Esc, KeyModifiers::NONE, &jobs).unwrap();
        assert!(state
            .activate(Choice::HumanProposal(proposal), &jobs)
            .is_err());
        assert!(!state
            .visible_actions()
            .items
            .iter()
            .any(|(_, choice)| matches!(choice, Choice::HumanProposal(_))));
    }

    #[test]
    fn human_proposal_uncertain_confirmation_keeps_exact_idempotent_retry_after_supersession() {
        let _locale = crate::test_support::lock_locale();
        let mut state = state();
        state.context.global_conversation = true;
        let mut proposal = human_proposal(&state, "work.control");
        state.append(None, &proposal);
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        state
            .activate(Choice::HumanProposal(proposal.clone()), &jobs)
            .unwrap();
        handle_key(&mut state, KeyCode::Enter, KeyModifiers::CONTROL, &jobs).unwrap();
        let job = receiver.try_recv().unwrap();
        let JobKind::Send(operation) = job.kind else {
            panic!("expected confirmation");
        };
        state.receive(Update {
            work: job.work,
            input: job.input,
            conversation: job.conversation,
            result: Ok(Outcome::Mutation {
                command_id: operation.command_id.clone(),
                result: Err(anyhow::anyhow!("receipt unavailable")),
            }),
        });
        proposal["status"] = json!("Submitted");
        proposal["version"] = json!(2);
        state.append(None, &proposal);
        state
            .activate(Choice::Reconcile(operation.command_id.clone()), &jobs)
            .unwrap();
        handle_key(&mut state, KeyCode::Enter, KeyModifiers::NONE, &jobs).unwrap();
        assert!(receiver.try_recv().is_err());
        handle_key(&mut state, KeyCode::Enter, KeyModifiers::CONTROL, &jobs).unwrap();
        let JobKind::Send(retried) = receiver.try_recv().unwrap().kind else {
            panic!("expected exact retry");
        };
        assert_eq!(operation_preview(&retried), operation_preview(&operation));
    }

    #[test]
    fn human_acceptance_proposal_requires_this_ui_fixed_inspection_and_rechecks_it() {
        let _locale = crate::test_support::lock_locale();
        let mut state = state();
        state.context.global_conversation = true;
        state.dashboard = false;
        let proposal = human_proposal(&state, "delivery.accept");
        state.append(None, &proposal);
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        state
            .activate(Choice::HumanProposal(proposal.clone()), &jobs)
            .unwrap();
        assert!(state.pending.is_none());
        assert!(matches!(receiver.try_recv().unwrap().kind, JobKind::Inspect(id) if id == "a"));
        let inspection = json!({"work":state.works[0]["work"],"candidate":state.works[0]["candidate"],
            "result":{"summary":"Actual captured result"},"artifacts":[]});
        state.receive(Update {
            work: Some("a".into()),
            input: String::new(),
            conversation: None,
            result: Ok(Outcome::Inspected(inspection.clone())),
        });
        state.select(Some("b".into()));
        state
            .activate(Choice::HumanProposal(proposal.clone()), &jobs)
            .unwrap();
        assert_eq!(state.pending.as_ref().unwrap().work.as_deref(), Some("a"));
        assert!(state.confirmation_current(state.pending.as_ref().unwrap()));
        state.works[0]["candidate"]["version"] = json!(3);
        assert!(!state.confirmation_current(state.pending.as_ref().unwrap()));
        assert_eq!(
            state.views[&Some("a".into())].inspection.as_ref(),
            Some(&inspection)
        );
        assert!(handle_key(&mut state, KeyCode::Enter, KeyModifiers::CONTROL, &jobs).is_err());
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn malformed_human_proposal_invalidates_older_card_without_authorizing_a_fallback() {
        let _locale = crate::test_support::lock_locale();
        let mut state = state();
        state.context.global_conversation = true;
        let mut proposal = human_proposal(&state, "work.control");
        state.append(None, &proposal);
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        state
            .activate(Choice::HumanProposal(proposal.clone()), &jobs)
            .unwrap();
        proposal["version"] = json!(2);
        proposal["request"]["method"] = json!("shell.execute");
        state.append(None, &proposal);
        assert!(matches!(state.notice_kind, NoticeKind::Error));
        assert!(!state.confirmation_current(state.pending.as_ref().unwrap()));
        assert!(state
            .activate(Choice::HumanProposal(proposal), &jobs)
            .is_err());
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn human_proposal_authority_cannot_be_replaced_by_summary_or_a_different_grant() {
        let _locale = crate::test_support::lock_locale();
        let mut state = state();
        state.context.global_conversation = true;
        let mut proposal = human_proposal(&state, "work.start");
        proposal["request"]["params"]["grantProposalId"] = json!("different-grant");
        assert!(state.human_proposal_operation(&proposal).is_err());
        proposal["request"]["params"]["grantProposalId"] = json!("grant-a");
        proposal["preview"]["grant"] = Value::Null;
        assert!(state.human_proposal_operation(&proposal).is_err());
        let preview = json!({"humanActionProposal":{},"project":{"name":"Reports","root":"C:\\Reports",
            "limits":{"maxAttempts":2},"capabilityIds":["approved-report"]},
            "approvedModelDestination":"Approved configured provider",
            "requestedChanges":{"findings":[{"requestedChange":"Keep the actual failing row"}],"advance":false}});
        let details = proposal_authority_details(&preview);
        for fact in [
            "maxAttempts: 2",
            "approved-report",
            "Approved configured provider",
            "Keep the actual failing row",
            "No",
        ] {
            assert!(details.contains(fact), "missing authorization fact: {fact}");
        }
    }

    #[test]
    fn legacy_home_rotates_both_binding_identities_while_global_home_preserves_chat() {
        let _locale = crate::test_support::lock_locale();
        let mut state = state();
        let before = state.selection();
        let (jobs, _) = mpsc::unbounded_channel();
        state.activate(Choice::Home, &jobs).unwrap();
        assert_ne!(state.context.conversation_id, before.conversation);
        assert_ne!(state.context.console_session_id, before.console);
        assert_eq!(
            state
                .conversation_consoles
                .get(&state.context.conversation_id),
            Some(&state.context.console_session_id)
        );
        state.context.global_conversation = true;
        state.chat.replace_draft("Preserved global draft".into());
        let before = state.selection();
        state.activate(Choice::Home, &jobs).unwrap();
        assert_eq!(state.selection(), before);
        assert_eq!(state.chat.draft, "Preserved global draft");
    }

    #[test]
    fn same_goal_and_project_require_distinct_explicit_choices_never_fuzzy_resolution() {
        let _locale = crate::test_support::lock_locale();
        let state = state();
        let a = state.work_label(Some("a"));
        let b = state.work_label(Some("b"));
        assert_ne!(a, b);
        assert!(a.contains("Fix the report") && a.contains("Reports"));
        let selections = state
            .actions()
            .items
            .into_iter()
            .filter_map(|(_, choice)| {
                if let Choice::SelectWork(id) = choice {
                    Some(id)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(selections, ["a", "b"]);
    }

    #[test]
    fn actions_bind_real_work_versions_and_pause_uses_hold_wire_action() {
        let _locale = crate::test_support::lock_locale();
        let mut state = state();
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        let control = state.actions().items.into_iter().find_map(|(_, choice)| {
            matches!(&choice, Choice::Control(operation) if operation.params["action"] == "Hold").then_some(choice)
        }).unwrap();
        state.activate(control, &jobs).unwrap();
        let job = receiver.try_recv().unwrap();
        let JobKind::Send(operation) = job.kind else {
            panic!("expected mutation")
        };
        assert_eq!(operation.method, "work.control");
        assert_eq!(operation.params, json!({"workId":"a","action":"Hold"}));
        assert_eq!(
            operation.if_match,
            [json!({"kind":"Work","id":"a","version":3})]
        );
        assert!(operation.mutation && !operation.confirmation);
    }

    #[test]
    fn new_work_clears_captured_selection_but_keeps_each_draft_caret_and_selection() {
        let _locale = crate::test_support::lock_locale();
        let mut state = state();
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        handle_paste(&mut state, "A unfinished");
        handle_key(&mut state, KeyCode::Left, KeyModifiers::SHIFT, &jobs).unwrap();
        let cursor = state.view().unwrap().editor.cursor;
        let anchor = state.view().unwrap().editor.anchor;
        let captured = state.selection();
        state.activate(Choice::Home, &jobs).unwrap();
        handle_paste(&mut state, "Prepare report B");
        submit(&mut state, &jobs).unwrap();
        let job = receiver.try_recv().unwrap();
        let JobKind::Send(operation) = job.kind else {
            panic!("expected conversation")
        };
        assert_eq!(operation.method, "conversation.submit");
        assert!(operation
            .params
            .pointer("/context/selectedWorkId")
            .is_none());
        assert_eq!(operation.params["context"]["scope"], "Global");
        assert!(operation.params["context"].get("projectId").is_none());
        state.activate(Choice::Return(captured), &jobs).unwrap();
        assert_eq!(state.view().unwrap().draft, "A unfinished");
        assert_eq!(state.view().unwrap().editor.cursor, cursor);
        assert_eq!(state.view().unwrap().editor.anchor, anchor);
        assert_eq!(state.chat.draft, "Prepare report B");
    }

    #[test]
    fn frozen_cancel_rejects_stale_guard_without_retargeting_and_escape_preserves_draft() {
        let _locale = crate::test_support::lock_locale();
        let mut state = state();
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        handle_paste(&mut state, "do not lose this");
        let action = state.actions().items.into_iter().find_map(|(_, choice)| {
            matches!(&choice, Choice::Control(operation) if operation.params["action"] == "Cancel").then_some(choice)
        }).unwrap();
        state.activate(action, &jobs).unwrap();
        let frozen = state.pending.as_ref().unwrap().operation.clone();
        state.observe(&json!({"data":work("a", 4)}));
        assert!(handle_key(&mut state, KeyCode::Enter, KeyModifiers::CONTROL, &jobs).is_err());
        assert!(receiver.try_recv().is_err());
        assert_eq!(
            state.pending.as_ref().unwrap().operation.command_id,
            frozen.command_id
        );
        assert_eq!(
            state.pending.as_ref().unwrap().operation.if_match,
            frozen.if_match
        );
        handle_key(&mut state, KeyCode::Esc, KeyModifiers::NONE, &jobs).unwrap();
        assert_eq!(state.view().unwrap().draft, "do not lose this");
        assert!(state.pending.is_none());
    }

    #[test]
    fn attention_answer_targets_a_while_b_editor_remains_untouched() {
        let _locale = crate::test_support::lock_locale();
        let mut state = state();
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        state.select(Some("b".into()));
        handle_paste(&mut state, "B selected text");
        handle_key(&mut state, KeyCode::Left, KeyModifiers::SHIFT, &jobs).unwrap();
        let cursor = state.view().unwrap().editor.cursor;
        let anchor = state.view().unwrap().editor.anchor;
        let question = json!({"id":"question-a","kind":"DecisionRequest","workId":"a","version":7,"status":"Open",
            "question":"Use concise output?","responseSchema":{"type":"boolean"}});
        state.append(Some("a".into()), &question);
        let action = state
            .actions()
            .items
            .into_iter()
            .find_map(|(_, choice)| matches!(choice, Choice::Question(..)).then_some(choice))
            .unwrap();
        state.activate(action, &jobs).unwrap();
        handle_key(&mut state, KeyCode::Right, KeyModifiers::NONE, &jobs).unwrap();
        handle_key(&mut state, KeyCode::Enter, KeyModifiers::CONTROL, &jobs).unwrap();
        let pending = state.pending.as_ref().unwrap();
        assert_eq!(pending.work.as_deref(), Some("a"));
        assert_eq!(
            pending.operation.params,
            json!({"decisionId":"question-a","value":true})
        );
        handle_key(&mut state, KeyCode::Enter, KeyModifiers::CONTROL, &jobs).unwrap();
        let sent = receiver.try_recv().unwrap();
        assert_eq!(sent.work.as_deref(), Some("a"));
        let JobKind::Send(operation) = &sent.kind else {
            panic!("expected form submission")
        };
        assert_eq!(
            state.mutations[&operation.command_id].origin,
            MutationOrigin::Action
        );
        assert!(state.mutations[&operation.command_id].requires_confirmation);
        assert_eq!(state.context.work_id.as_deref(), Some("b"));
        assert_eq!(state.view().unwrap().draft, "B selected text");
        assert_eq!(state.view().unwrap().editor.cursor, cursor);
        assert_eq!(state.view().unwrap().editor.anchor, anchor);
    }

    #[test]
    fn accept_is_only_available_for_the_actual_inspected_candidate_and_current_work() {
        let _locale = crate::test_support::lock_locale();
        let mut state = state();
        assert!(!state
            .actions()
            .items
            .iter()
            .any(|(_, choice)| matches!(choice, Choice::Accept(..))));
        let view = work("a", 3);
        let inspection = json!({"work":view["work"],"candidate":view["candidate"],"artifacts":[]});
        let operation = acceptance(&view, &inspection).unwrap();
        assert_eq!(operation.params, json!({"candidateId":"candidate-id"}));
        assert_eq!(
            operation.if_match,
            [
                json!({"kind":"Work","id":"a","version":3}),
                json!({"kind":"DeliveryCandidate","id":"candidate-id","version":2}),
            ]
        );
        assert!(operation.confirmation);
        assert!(acceptance(&work("a", 4), &inspection).is_err());
        assert!(acceptance(&work("b", 3), &inspection).is_err());
        state.views.get_mut(&Some("a".into())).unwrap().inspection = Some(inspection);
        assert!(state
            .actions()
            .items
            .iter()
            .any(|(_, choice)| matches!(choice, Choice::Accept(..))));
    }

    #[test]
    fn stale_stream_preserves_editor_and_blocks_approval_without_replaying_mutations() {
        let _locale = crate::test_support::lock_locale();
        let mut state = state();
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        handle_paste(&mut state, "keep my draft");
        state.event(json!({"type":"stream_error","failure":{"message":"connection ended"}}));
        assert!(state.stale);
        assert_eq!(state.view().unwrap().draft, "keep my draft");
        let operation = control(&work("a", 3)["work"], "Hold").unwrap();
        assert!(queue_captured(
            &mut state,
            &jobs,
            JobKind::Send(operation),
            Some("a".into()),
            String::new()
        )
        .is_err());
        assert!(receiver.try_recv().is_err());
        assert!(!state.actions().items.iter().any(|(_, choice)| matches!(
            choice,
            Choice::Start(_) | Choice::Control(_) | Choice::Accept(..)
        )));
    }

    #[test]
    fn recovery_inbox_refresh_uses_captured_versions_and_preserves_later_tombstones() {
        let _locale = crate::test_support::lock_locale();
        let mut state = state();
        let attention = |version, status| {
            json!({
                "kind":"AttentionItem","id":"attention-a","workId":"a","version":version,
                "status":status,"reason":"Decision","requiredAction":"Answer the question"
            })
        };
        let event = |version, status| {
            json!({
                "eventId":format!("attention-{version}"),"workId":"a",
                "changes":[{"subject":{"kind":"AttentionItem","id":"attention-a","version":version},
                    "view":attention(version, status)}]
            })
        };
        state.event(event(1, "Open"));
        let baseline = state.event_versions.clone();
        state.receive(Update {
            work: None,
            input: String::new(),
            conversation: None,
            result: Ok(Outcome::Refreshed {
                works: json!({"status":"ok","data":{"items":[]}}),
                projects: json!({"status":"ok","data":{"items":[]}}),
                decisions: json!({"status":"ok","data":{"items":[]}}),
                inbox: json!({"status":"ok","data":{"items":[attention(2, "Open")]}}),
                inbox_versions: baseline,
            }),
        });
        assert_eq!(state.inbox[0]["version"], 2);
        let baseline = state.event_versions.clone();
        state.event(event(3, "Resolved"));
        state.receive(Update {
            work: None,
            input: String::new(),
            conversation: None,
            result: Ok(Outcome::Refreshed {
                works: json!({"status":"ok","data":{"items":[]}}),
                projects: json!({"status":"ok","data":{"items":[]}}),
                decisions: json!({"status":"ok","data":{"items":[]}}),
                inbox: json!({"status":"ok","data":{"items":[attention(2, "Open")]}}),
                inbox_versions: baseline,
            }),
        });
        assert!(
            state.inbox.is_empty(),
            "recovery must not resurrect answered attention"
        );
    }

    #[test]
    fn conversation_snapshot_replaces_only_its_intake_questions_and_closes_resolved_form() {
        let _locale = crate::test_support::lock_locale();
        let mut state = state();
        let question = |id, conversation, version, status| {
            json!({
                "kind":"IntakeRequest","id":id,"conversationId":conversation,"version":version,
                "status":status,"question":"Choose report detail","responseSchema":{"type":"boolean"}
            })
        };
        state.append(None, &question("a-question", "conversation-a", 1, "Open"));
        state.append(None, &question("b-question", "conversation-b", 1, "Open"));
        state.append(
            None,
            &json!({"kind":"Conversation","id":"conversation-a","version":1,
            "messages":[],"intakeRequests":[]}),
        );
        assert!(!state.views[&None].records.contains_key("a-question"));
        assert!(state.views[&None].records.contains_key("b-question"));
        let captured = question("a-current", "conversation-a", 2, "Open");
        state.append(None, &captured);
        state.form = Some(form::Form::new(captured, None).unwrap());
        state.append(None, &json!({"kind":"Conversation","id":"conversation-a","version":1,
            "messages":[],"intakeRequests":[question("a-current", "conversation-a", 3, "Cancelled")]}));
        assert!(state.form.is_none());
        assert_eq!(
            state.views[&None].records["a-current"]["status"],
            "Cancelled"
        );
        assert!(state.views[&None].records.contains_key("b-question"));
    }

    fn refreshed(state: &State) -> Update {
        Update {
            work: None,
            input: String::new(),
            conversation: None,
            result: Ok(Outcome::Refreshed {
                works: json!({"status":"ok","data":{"items":[]}}),
                projects: json!({"status":"ok","data":{"items":[]}}),
                decisions: json!({"status":"ok","data":{"items":[]}}),
                inbox: json!({"status":"ok","data":{"items":[]}}),
                inbox_versions: state.event_versions.clone(),
            }),
        }
    }

    fn uncertain_submission(
        state: &mut State,
        jobs: &mpsc::UnboundedSender<Job>,
        receiver: &mut mpsc::UnboundedReceiver<Job>,
        input: &str,
    ) -> Operation {
        handle_paste(state, input);
        submit(state, jobs).unwrap();
        let job = receiver.try_recv().unwrap();
        let JobKind::Send(operation) = job.kind else {
            panic!("expected conversation")
        };
        state.receive(Update {
            work: job.work,
            input: job.input,
            conversation: job.conversation,
            result: Ok(Outcome::Mutation {
                command_id: operation.command_id.clone(),
                result: Err(anyhow::anyhow!(
                    "OUTCOME_UNKNOWN: committed intake receipt was lost"
                )),
            }),
        });
        operation
    }

    #[test]
    fn unresolved_intakes_survive_refresh_navigation_and_other_work_with_original_identities() {
        let _locale = crate::test_support::lock_locale();
        let mut state = state();
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        let a = uncertain_submission(&mut state, &jobs, &mut receiver, "Goal A");
        let a_selection = state.selection();
        state.stale = true;
        let refresh = refreshed(&state);
        state.receive(refresh);
        assert!(!state.stale);
        assert!(state.notice.contains("OUTCOME_UNKNOWN"));
        assert!(!state.notice.contains("Request succeeded"));
        state.receive(Update {
            work: Some("a".into()),
            input: String::new(),
            conversation: None,
            result: Ok(Outcome::ReadResponse(json!({"status":"ok","data":{}}))),
        });
        assert!(state.notice.contains("OUTCOME_UNKNOWN"));
        state.receive(Update {
            work: Some("a".into()),
            input: String::new(),
            conversation: None,
            result: Ok(Outcome::SelectedWork {
                id: "b".into(),
                response: json!({"status":"ok","data":work("b", 4)}),
            }),
        });
        let b = uncertain_submission(&mut state, &jobs, &mut receiver, "Goal B");
        assert_eq!(state.mutations.len(), 2);
        state.restore_selection(a_selection);
        submit(&mut state, &jobs).unwrap();
        let retry = receiver.try_recv().unwrap();
        let JobKind::Send(sent) = retry.kind else {
            panic!("expected exact reconciliation")
        };
        assert_eq!(operation_preview(&sent), operation_preview(&a));
        assert_ne!(sent.command_id, b.command_id);
        assert_eq!(sent.params["clientMessageId"], a.params["clientMessageId"]);
        state.receive(Update {
            work: retry.work, input: retry.input, conversation: retry.conversation,
            result: Ok(Outcome::Mutation {
                command_id: sent.command_id,
                result: Ok(json!({"status":"pending","operationId":"intake-operation","data":{"intakeTurnId":"original-turn"}})),
            }),
        });
        assert!(!state.mutations.contains_key(&a.command_id));
        assert!(state.mutations.contains_key(&b.command_id));
        assert!(state.views[&Some("a".into())].draft.is_empty());
        assert_eq!(state.views[&Some("b".into())].draft, "Goal B");
        assert!(state.notice.contains("OUTCOME_UNKNOWN"));
        assert!(!state.views[&Some("a".into())]
            .records
            .contains_key(&format!("unresolved:{}", a.command_id)));
    }

    #[test]
    fn only_an_absent_optional_intake_scope_can_be_skipped_during_reconciliation() {
        let missing = json!({"status":"error","failure":{"code":"INVALID_REFERENCE"}});
        assert!(absent_intake_conversation(
            &json!({"kind":"Conversation","id":"not-yet-created"}),
            &missing
        ));
        assert!(!absent_intake_conversation(
            &json!({"kind":"Work","id":"missing"}),
            &missing
        ));
        assert!(!absent_intake_conversation(
            &json!({"kind":"Conversation","id":"forbidden"}),
            &json!({"status":"error","failure":{"code":"FORBIDDEN"}})
        ));
    }

    #[test]
    fn changed_global_conversation_requires_explicit_original_scope_reconciliation() {
        let _locale = crate::test_support::lock_locale();
        let mut state = state();
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        state.activate(Choice::Home, &jobs).unwrap();
        let original = uncertain_submission(&mut state, &jobs, &mut receiver, "Unchanged goal");
        state.activate(Choice::Home, &jobs).unwrap();
        // A genuinely different global binding cannot inherit an uncertain send.
        state.context.conversation_id = uuid::Uuid::new_v4().to_string();
        state.context.console_session_id = uuid::Uuid::new_v4().to_string();
        let new_context = state.selection();
        assert_ne!(
            new_context.conversation,
            original.params["conversationId"].as_str().unwrap()
        );
        assert!(submit(&mut state, &jobs).is_err());
        assert!(receiver.try_recv().is_err());
        state
            .activate(Choice::Reconcile(original.command_id.clone()), &jobs)
            .unwrap();
        let retried = receiver.try_recv().unwrap();
        let JobKind::Send(operation) = retried.kind else {
            panic!("expected reconciliation")
        };
        assert_eq!(operation_preview(&operation), operation_preview(&original));
        assert_eq!(state.selection(), new_context);
        state.receive(Update {
            work: retried.work,
            input: retried.input,
            conversation: retried.conversation,
            result: Ok(Outcome::Mutation {
                command_id: operation.command_id,
                result: Ok(json!({"status":"pending","data":{"intakeTurnId":"original-turn"}})),
            }),
        });
        assert_eq!(
            state.editor_view().unwrap().draft,
            "Unchanged goal",
            "old conversation receipt must not clear the new conversation draft"
        );
    }

    #[test]
    fn return_restores_project_conversation_and_draft_for_work_and_home() {
        let _locale = crate::test_support::lock_locale();
        let mut state = state();
        state.works[0]["work"]["projectId"] = json!("project-a");
        state.works[1]["work"]["projectId"] = json!("project-b");
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        for destination in [Some("b".to_owned()), None] {
            if destination.is_none() {
                state.context.project_id = Some("project-global".into());
            }
            state.select(destination.clone());
            state.view_mut().clear_draft();
            handle_paste(&mut state, "preserved draft");
            handle_key(&mut state, KeyCode::Left, KeyModifiers::SHIFT, &jobs).unwrap();
            let captured = state.selection();
            let caret = state.view().unwrap().editor.cursor;
            let anchor = state.view().unwrap().editor.anchor;
            let a = state.works[0].clone();
            state.receive(Update {
                work: destination.clone(),
                input: String::new(),
                conversation: None,
                result: Ok(Outcome::SelectedWork {
                    id: "a".into(),
                    response: json!({"status":"ok","data":a}),
                }),
            });
            state
                .activate(Choice::Return(state.return_work.clone().unwrap()), &jobs)
                .unwrap();
            assert_eq!(state.selection(), captured);
            assert_eq!(state.view().unwrap().editor.cursor, caret);
            assert_eq!(state.view().unwrap().editor.anchor, anchor);
            submit(&mut state, &jobs).unwrap();
            let job = receiver.try_recv().unwrap();
            let JobKind::Send(operation) = job.kind else {
                panic!("expected captured conversation")
            };
            assert_eq!(
                operation.params["context"]["projectId"].as_str(),
                captured.project.as_deref()
            );
            assert_eq!(operation.params["conversationId"], captured.conversation);
            assert_eq!(
                operation
                    .params
                    .pointer("/context/selectedWorkId")
                    .and_then(Value::as_str),
                destination.as_deref()
            );
            state.receive(Update {
                work: job.work,
                input: job.input,
                conversation: job.conversation,
                result: Ok(Outcome::Mutation {
                    command_id: operation.command_id,
                    result: Ok(json!({"status":"ok","data":{}})),
                }),
            });
        }
    }

    #[test]
    fn changing_inspection_clears_evidence_and_rejects_late_old_pages() {
        let _locale = crate::test_support::lock_locale();
        let mut state = state();
        let first = work("a", 3);
        let first_inspection =
            json!({"work":first["work"],"candidate":first["candidate"],"artifacts":[]});
        let first_id = InspectionIdentity::from_view(&first_inspection).unwrap();
        let first_read = Operation::read(
            "artifact.read",
            json!({"artifactId":"artifact-one","relativePath":"check.log"}),
        );
        let old_page = json!({"status":"ok","data":{"artifactId":"artifact-one","relativePath":"check.log","text":"OLD C1 CONTENT","eof":false,"nextOffset":14}});
        state.receive(Update {
            work: Some("a".into()),
            input: String::new(),
            conversation: None,
            result: Ok(Outcome::Inspected(first_inspection)),
        });
        state.receive(Update {
            work: Some("a".into()),
            input: String::new(),
            conversation: None,
            result: Ok(Outcome::Evidence {
                response: old_page.clone(),
                inspection: first_id.clone(),
                operation: first_read.clone(),
            }),
        });
        assert!(state.view().unwrap().evidence.is_some());
        let mut second = work("a", 4);
        second["work"]["currentCandidateId"] = json!("candidate-two");
        second["candidate"]["id"] = json!("candidate-two");
        second["candidate"]["version"] = json!(1);
        state.observe(&json!({"data":second}));
        let second_inspection =
            json!({"work":second["work"],"candidate":second["candidate"],"artifacts":[]});
        let second_id = InspectionIdentity::from_view(&second_inspection).unwrap();
        state.receive(Update {
            work: Some("a".into()),
            input: String::new(),
            conversation: None,
            result: Ok(Outcome::Inspected(second_inspection)),
        });
        assert!(state.view().unwrap().evidence.is_none());
        state.receive(Update {
            work: Some("a".into()),
            input: String::new(),
            conversation: None,
            result: Ok(Outcome::Evidence {
                response: old_page.clone(),
                inspection: first_id.clone(),
                operation: first_read.clone(),
            }),
        });
        assert!(state.view().unwrap().evidence.is_none());
        let second_read = Operation::read(
            "artifact.read",
            json!({"artifactId":"artifact-two","relativePath":"check.log"}),
        );
        state.receive(Update { work:Some("a".into()), input:String::new(), conversation:None,
            result:Ok(Outcome::Evidence { response:json!({"status":"ok","data":{"artifactId":"artifact-two","relativePath":"check.log","text":"CURRENT C2 CONTENT","eof":false,"nextOffset":18}}), inspection:second_id, operation:second_read }) });
        state.receive(Update {
            work: Some("a".into()),
            input: String::new(),
            conversation: None,
            result: Ok(Outcome::Evidence {
                response: old_page,
                inspection: first_id,
                operation: first_read,
            }),
        });
        assert_eq!(
            state.view().unwrap().evidence.as_ref().unwrap().data["text"],
            "CURRENT C2 CONTENT"
        );
        let actions = state.actions();
        assert!(actions.items.iter().any(|(_, choice)| matches!(choice, Choice::Accept(operation, _) if operation.params["candidateId"] == "candidate-two")));
        assert!(actions.items.iter().any(|(_, choice)| matches!(choice, Choice::Evidence(operation, _, _) if operation.params["artifactId"] == "artifact-two" && operation.params["offset"] == 18)));
        assert!(!actions.items.iter().any(|(_, choice)| matches!(choice, Choice::Evidence(operation, _, _) if operation.params["artifactId"] == "artifact-one")));
    }

    fn uncertain_cancel(
        state: &mut State,
        jobs: &mpsc::UnboundedSender<Job>,
        receiver: &mut mpsc::UnboundedReceiver<Job>,
        capture_editor: bool,
    ) -> Operation {
        let cancel = state.actions().items.into_iter().find_map(|(_, choice)| {
            matches!(&choice, Choice::Control(operation) if operation.params["action"] == "Cancel").then_some(choice)
        }).unwrap();
        state.activate(cancel, jobs).unwrap();
        if capture_editor {
            let draft = state.view().unwrap().draft.clone();
            state.pending.as_mut().unwrap().input = draft;
        }
        handle_key(state, KeyCode::Enter, KeyModifiers::CONTROL, jobs).unwrap();
        let sent = receiver.try_recv().unwrap();
        let JobKind::Send(operation) = sent.kind else {
            panic!("expected cancellation")
        };
        state.receive(Update {
            work: sent.work,
            input: sent.input,
            conversation: sent.conversation,
            result: Ok(Outcome::Mutation {
                command_id: operation.command_id.clone(),
                result: Err(anyhow::anyhow!(
                    "OUTCOME_UNKNOWN: cancellation receipt lost"
                )),
            }),
        });
        handle_key(state, KeyCode::Esc, KeyModifiers::NONE, jobs).unwrap();
        operation
    }

    #[test]
    fn dismissed_uncertain_menu_cancel_requires_explicit_reconcile_and_ctrl_enter() {
        let _locale = crate::test_support::lock_locale();
        let mut state = state();
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        let original = uncertain_cancel(&mut state, &jobs, &mut receiver, false);
        let captured = &state.mutations[&original.command_id];
        assert_eq!(captured.origin, MutationOrigin::Action);
        assert!(captured.requires_confirmation);
        assert!(state.retry.is_none());
        let _ = handle_key(&mut state, KeyCode::Enter, KeyModifiers::NONE, &jobs);
        assert!(
            receiver.try_recv().is_err(),
            "empty editor Enter must not retry a menu cancellation"
        );
        state
            .context
            .versions
            .insert(("Work".into(), "a".into()), 99);
        handle_key(&mut state, KeyCode::F(4), KeyModifiers::NONE, &jobs).unwrap();
        let index = state
            .menu
            .as_ref()
            .unwrap()
            .items
            .iter()
            .position(
                |(_, choice)| matches!(choice, Choice::Reconcile(id) if id == &original.command_id),
            )
            .unwrap();
        for _ in 0..index {
            handle_key(&mut state, KeyCode::Down, KeyModifiers::NONE, &jobs).unwrap();
        }
        handle_key(&mut state, KeyCode::Enter, KeyModifiers::NONE, &jobs).unwrap();
        assert!(state.pending.is_some());
        assert_eq!(
            operation_preview(&state.pending.as_ref().unwrap().operation),
            operation_preview(&original)
        );
        handle_key(&mut state, KeyCode::Enter, KeyModifiers::NONE, &jobs).unwrap();
        assert!(
            receiver.try_recv().is_err(),
            "ordinary Enter cannot approve reconciliation"
        );
        handle_key(&mut state, KeyCode::Enter, KeyModifiers::CONTROL, &jobs).unwrap();
        let JobKind::Send(retried) = receiver.try_recv().unwrap().kind else {
            panic!("expected explicit reconciliation")
        };
        assert_eq!(operation_preview(&retried), operation_preview(&original));
    }

    #[test]
    fn uncertain_menu_action_cannot_intercept_a_coincidentally_matching_editor_draft() {
        let _locale = crate::test_support::lock_locale();
        let mut state = state();
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        handle_paste(&mut state, "Write an independent report");
        let original = uncertain_cancel(&mut state, &jobs, &mut receiver, true);
        assert_eq!(
            state.mutations[&original.command_id].input,
            state.view().unwrap().draft
        );
        handle_key(&mut state, KeyCode::Enter, KeyModifiers::NONE, &jobs).unwrap();
        let JobKind::Send(submitted) = receiver.try_recv().unwrap().kind else {
            panic!("expected editor submission")
        };
        assert_eq!(submitted.method, "conversation.submit");
        assert_eq!(submitted.params["text"], "Write an independent report");
        assert_ne!(submitted.command_id, original.command_id);
        assert!(state.mutations[&original.command_id].unknown.is_some());
    }

    #[test]
    fn prepared_editor_command_reconciliation_preserves_confirmation_even_if_operation_flag_is_false(
    ) {
        let _locale = crate::test_support::lock_locale();
        let mut state = state();
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        state.view_mut().replace_draft("/workspace takeover".into());
        submit(&mut state, &jobs).unwrap();
        let prepared = receiver.try_recv().unwrap();
        assert_eq!(prepared.origin, MutationOrigin::Editor);
        let original = Operation {
            method: "workspace.takeover".into(),
            params: json!({"workspaceId":"workspace-a"}),
            if_match: vec![json!({"kind":"Workspace","id":"workspace-a","version":4})],
            mutation: true,
            confirmation: false,
            command_id: "prepared-original".into(),
        };
        state.receive(Update {
            work: prepared.work,
            input: prepared.input,
            conversation: prepared.conversation,
            result: Ok(Outcome::StartPreview {
                operation: original.clone(),
                preview: operation_preview(&original),
                origin: prepared.origin,
            }),
        });
        handle_key(&mut state, KeyCode::Enter, KeyModifiers::CONTROL, &jobs).unwrap();
        let sent = receiver.try_recv().unwrap();
        state.receive(Update {
            work: sent.work,
            input: sent.input,
            conversation: sent.conversation,
            result: Ok(Outcome::Mutation {
                command_id: original.command_id.clone(),
                result: Err(anyhow::anyhow!("OUTCOME_UNKNOWN: receipt lost")),
            }),
        });
        handle_key(&mut state, KeyCode::Esc, KeyModifiers::NONE, &jobs).unwrap();
        assert!(state.mutations[&original.command_id].requires_confirmation);
        handle_key(&mut state, KeyCode::Enter, KeyModifiers::NONE, &jobs).unwrap();
        assert!(receiver.try_recv().is_err());
        assert!(
            state.pending.is_some(),
            "typed command retry must reopen its frozen approval"
        );
        handle_key(&mut state, KeyCode::Enter, KeyModifiers::NONE, &jobs).unwrap();
        assert!(receiver.try_recv().is_err());
        handle_key(&mut state, KeyCode::Enter, KeyModifiers::CONTROL, &jobs).unwrap();
        let JobKind::Send(retried) = receiver.try_recv().unwrap().kind else {
            panic!("expected approved retry")
        };
        assert_eq!(operation_preview(&retried), operation_preview(&original));
    }
}
