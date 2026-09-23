// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

use super::client::{capture_versions, new_context, resolve_input_args, send, status_exit_code};
use super::commands::{self, Action, CommandContext, Input, Operation};
use super::transport::Client;
use anyhow::{bail, Context, Result};
use crossterm::{
    event::{
        DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
#[cfg(not(windows))]
use futures::StreamExt;
use ratatui::{backend::CrosstermBackend, Terminal};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use tokio::sync::mpsc;

mod demo;
pub(super) mod editor;
mod form;
mod projection;
mod renderer;
mod task_tab;
mod workflow;
use editor::DraftEditor;
#[cfg(windows)]
pub(super) mod input;

struct WorkView {
    draft: String,
    editor: DraftEditor,
    transcript: Vec<Value>,
    scroll: u16,
    follow: bool,
    messages: Vec<Value>,
    records: BTreeMap<String, Value>,
    inspection: Option<Value>,
    evidence: Option<EvidencePage>,
    context: Option<SelectionContext>,
    details_open: bool,
    details_scroll: u16,
    show_delivery: bool,
    delivery_scroll: u16,
    action_index: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Focus {
    #[default]
    Composer,
    Actions,
    Details,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum HomeScreen {
    #[default]
    Overview,
    Attention,
}

#[derive(Clone, Copy, Default)]
enum NoticeKind {
    #[default]
    Info,
    Attention,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SelectionContext {
    work: Option<String>,
    project: Option<String>,
    conversation: String,
    console: String,
    version: u64,
}

#[derive(Clone)]
struct PendingMutation {
    operation: Operation,
    input: String,
    selection: SelectionContext,
    preview: Value,
    target_label: String,
    unknown: Option<String>,
    origin: MutationOrigin,
    requires_confirmation: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MutationOrigin {
    Editor,
    Action,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct InspectionIdentity {
    work: String,
    work_version: u64,
    candidate: String,
    candidate_version: u64,
}

impl InspectionIdentity {
    fn from_view(view: &Value) -> Option<Self> {
        Some(Self {
            work: view["work"]["id"].as_str()?.into(),
            work_version: view["work"]["version"].as_u64()?,
            candidate: view["candidate"]["id"].as_str()?.into(),
            candidate_version: view["candidate"]["version"].as_u64()?,
        })
    }
}

struct EvidencePage {
    inspection: InspectionIdentity,
    data: Value,
}

impl Default for WorkView {
    fn default() -> Self {
        Self {
            draft: String::new(),
            editor: DraftEditor::default(),
            transcript: vec![],
            scroll: 0,
            follow: true,
            messages: vec![],
            records: BTreeMap::new(),
            inspection: None,
            evidence: None,
            context: None,
            details_open: false,
            details_scroll: 0,
            show_delivery: false,
            delivery_scroll: 0,
            action_index: 0,
        }
    }
}

impl WorkView {
    fn replace_draft(&mut self, draft: String) {
        self.editor.reset(draft.len());
        self.draft = draft;
    }

    fn clear_draft(&mut self) {
        self.replace_draft(String::new());
    }

    fn insert(&mut self, value: &str) {
        self.editor.insert(&mut self.draft, value);
    }
}

struct PendingConfirmation {
    operation: Operation,
    preview: Value,
    work: Option<String>,
    input: String,
    scroll: u16,
    target_label: String,
    origin: MutationOrigin,
}

#[derive(Clone)]
struct InboxRefresh {
    operation: Operation,
    versions: BTreeMap<(String, String), u64>,
}

struct State {
    demo: Option<demo::Presentation>,
    context: CommandContext,
    chat: WorkView,
    dashboard_view: WorkView,
    dashboard: bool,
    dashboard_work: bool,
    views: BTreeMap<Option<String>, WorkView>,
    works: Vec<Value>,
    inbox: Vec<Value>,
    notice: String,
    notice_kind: NoticeKind,
    completion: usize,
    pending: Option<PendingConfirmation>,
    approval_choice: usize,
    approval_expanded: bool,
    deferred_proposals: BTreeMap<String, u64>,
    busy: bool,
    activity_frame: usize,
    event_ids: BTreeSet<String>,
    chunks: BTreeSet<(String, String, u64)>,
    event_versions: BTreeMap<(String, String), u64>,
    cursor: Option<String>,
    retry: Option<(String, Option<String>, JobKind)>,
    mutations: BTreeMap<String, PendingMutation>,
    conversations: BTreeMap<(Option<String>, Option<String>), String>,
    conversation_consoles: BTreeMap<String, String>,
    conversation_targets: BTreeMap<String, Option<String>>,
    prepared: Option<(Operation, Option<String>, String, MutationOrigin)>,
    projects: Vec<Value>,
    menu: Option<workflow::Menu>,
    form: Option<form::Form>,
    diagnostics: bool,
    stale: bool,
    return_work: Option<SelectionContext>,
    queued_jobs: usize,
    refresh_needed: bool,
    reconnect_requested: bool,
    focus: Focus,
    cards: Option<workflow::Menu>,
    home_screen: HomeScreen,
    task_list: bool,
    task_index: usize,
    global_selection: SelectionContext,
}

impl State {
    fn new() -> Self {
        let mut context = new_context();
        context.global_conversation = true;
        let conversation_consoles = BTreeMap::from([(
            context.conversation_id.clone(),
            context.console_session_id.clone(),
        )]);
        let global_selection = SelectionContext {
            work: None,
            project: None,
            conversation: context.conversation_id.clone(),
            console: context.console_session_id.clone(),
            version: context.context_version,
        };
        Self {
            demo: None,
            context,
            chat: WorkView::default(),
            dashboard_view: WorkView::default(),
            dashboard: false,
            dashboard_work: false,
            views: BTreeMap::from([(None, WorkView::default())]),
            works: vec![],
            inbox: vec![],
            notice: String::new(),
            notice_kind: NoticeKind::Info,
            completion: 0,
            pending: None,
            approval_choice: 0,
            approval_expanded: false,
            deferred_proposals: BTreeMap::new(),
            busy: false,
            activity_frame: 0,
            event_ids: BTreeSet::new(),
            chunks: BTreeSet::new(),
            event_versions: BTreeMap::new(),
            cursor: None,
            retry: None,
            mutations: BTreeMap::new(),
            conversations: BTreeMap::new(),
            conversation_consoles,
            conversation_targets: BTreeMap::new(),
            prepared: None,
            projects: vec![],
            menu: None,
            form: None,
            diagnostics: false,
            stale: false,
            return_work: None,
            queued_jobs: 0,
            refresh_needed: false,
            reconnect_requested: false,
            focus: Focus::Composer,
            cards: None,
            home_screen: HomeScreen::Overview,
            task_list: true,
            task_index: 0,
            global_selection,
        }
    }
    #[cfg(test)]
    fn legacy() -> Self {
        let mut state = Self::new();
        state.context.global_conversation = false;
        state.dashboard = true;
        state.task_list = false;
        state.notice = t!("agent_center.welcome").into_owned();
        state
    }
    fn thinking(&self) -> bool {
        if self.stale {
            return false;
        }
        let active_responses = self
            .selected_view()
            .and_then(|work| work["continuation"]["activeResponses"].as_array());
        self.editor_view().is_some_and(|view| {
            view.messages.iter().any(|message| {
                message["role"] == "assistant"
                    && message["status"] == "Streaming"
                    && (self.context.global_conversation
                        || active_responses.is_some_and(|responses| {
                            responses.iter().any(|response| {
                                message["id"].is_string()
                                    && response["messageId"] == message["id"]
                                    && response["deadlineUtc"]
                                        .as_str()
                                        .and_then(|deadline| {
                                            time::OffsetDateTime::parse(
                                                deadline,
                                                &time::format_description::well_known::Rfc3339,
                                            )
                                            .ok()
                                        })
                                        .is_some_and(|deadline| {
                                            deadline > time::OffsetDateTime::now_utc()
                                        })
                            })
                        }))
            })
        }) || self.mutations.values().any(|mutation| {
            mutation.operation.method == "conversation.submit"
                && mutation.selection.conversation == self.context.conversation_id
                && mutation.selection.console == self.context.console_session_id
                && mutation.unknown.is_none()
        })
    }
    fn recovery_failure(&self) -> Option<&Value> {
        if self.context.global_conversation || self.task_list {
            return None;
        }
        self.selected_view()?
            .pointer("/continuation/recoveryFailure")
            .filter(|failure| failure.is_object())
    }
    fn editor_view(&self) -> Option<&WorkView> {
        if let Some(demo) = self.demo.as_ref().filter(|demo| demo.legacy) {
            return demo.legacy_views.get(demo.legacy_index);
        }
        if self.context.global_conversation {
            Some(&self.chat)
        } else {
            self.view()
        }
    }
    fn editor_view_mut(&mut self) -> &mut WorkView {
        if self.demo.as_ref().is_some_and(|demo| demo.legacy) {
            let demo = self.demo.as_mut().expect("demo checked above");
            return &mut demo.legacy_views[demo.legacy_index];
        }
        if self.context.global_conversation {
            &mut self.chat
        } else {
            self.view_mut()
        }
    }
    fn reading_view_mut(&mut self) -> &mut WorkView {
        if self.demo.as_ref().is_some_and(|demo| demo.legacy) {
            return self.editor_view_mut();
        }
        if self.context.global_conversation {
            if !self.dashboard
                && !self.diagnostics
                && !self.view().is_some_and(|view| view.show_delivery)
            {
                return &mut self.chat;
            }
            if self.dashboard && !self.dashboard_work {
                return &mut self.dashboard_view;
            }
        }
        self.view_mut()
    }
    fn view(&self) -> Option<&WorkView> {
        self.views.get(&self.context.work_id)
    }
    fn set_response_notice(&mut self, response: &Value) {
        if matches!(
            response["status"].as_str(),
            Some("ok" | "pending" | "needs_input")
        ) {
            let data = &response["data"];
            let view = if data["continuation"]["state"].is_string() {
                data
            } else {
                &data["workView"]
            };
            let kind = match view["continuation"]["state"].as_str() {
                Some("NeedsRecovery" | "WaitingForInput" | "Paused" | "Unavailable") => {
                    Some(NoticeKind::Attention)
                }
                Some("Running" | "Ready" | "Completed" | "Cancelled") => Some(NoticeKind::Info),
                _ => None,
            };
            if let Some(kind) = kind {
                self.notice = task_tab::continuation_label(view);
                self.notice_kind = kind;
                return;
            }
        }
        self.notice = status_label(response);
        self.notice_kind = match response["status"].as_str() {
            Some("ok") => NoticeKind::Info,
            Some("pending" | "needs_input") => NoticeKind::Attention,
            _ => NoticeKind::Error,
        };
    }
    fn view_mut(&mut self) -> &mut WorkView {
        self.views.entry(self.context.work_id.clone()).or_default()
    }
    fn select(&mut self, work: Option<String>) {
        self.sync_view_context();
        if let Some(project) = self
            .works
            .iter()
            .find(|view| view["work"]["id"].as_str() == work.as_deref())
            .and_then(|view| view["work"]["projectId"].as_str())
        {
            self.context.project_id = Some(project.into());
        }
        self.context.work_id = work;
        self.dashboard_work = self.context.work_id.is_some();
        self.context.context_version += 1;
        self.views.entry(self.context.work_id.clone()).or_default();
        if self.context.global_conversation {
            self.view_mut().details_open = false;
            self.view_mut().show_delivery = false;
        }
        self.completion = 0;
        // A confirmation must never follow the user to a different work.
        self.pending = None;
        self.menu = None;
        self.form = None;
        self.focus = if self.view().is_some_and(|view| view.details_open) {
            Focus::Details
        } else {
            Focus::Composer
        };
        self.cards = None;
        self.home_screen = HomeScreen::Overview;
        self.select_conversation();
        self.sync_view_context();
    }
    fn sync_view_context(&mut self) {
        let selection = self.selection();
        self.view_mut().context = Some(selection);
    }
    fn selection(&self) -> SelectionContext {
        SelectionContext {
            work: self.context.work_id.clone(),
            project: self.context.project_id.clone(),
            conversation: self.context.conversation_id.clone(),
            console: self.context.console_session_id.clone(),
            version: self.context.context_version,
        }
    }
    fn restore_selection(&mut self, selection: SelectionContext) {
        self.select(selection.work.clone());
        self.context.project_id = selection.project.clone();
        self.context.conversation_id = selection.conversation.clone();
        self.context.context_version = selection.version;
        if self.context.global_conversation {
            self.context.console_session_id = selection.console;
            self.sync_view_context();
            return;
        }
        self.conversation_consoles
            .insert(selection.conversation.clone(), selection.console);
        self.conversations
            .insert((selection.project, selection.work), selection.conversation);
        self.select_console_binding();
        self.sync_view_context();
    }
    fn unresolved_notice(&self) -> Option<String> {
        self.mutations
            .values()
            .find(|mutation| {
                mutation.unknown.is_some() && mutation.selection.work == self.context.work_id
            })
            .or_else(|| {
                self.mutations
                    .values()
                    .find(|mutation| mutation.unknown.is_some())
            })
            .map(|mutation| {
                format!(
                    "{} · {}\n{}",
                    t!("agent_center.console_unknown"),
                    mutation.target_label,
                    mutation.unknown.as_deref().unwrap_or("")
                )
            })
    }
    fn settle_mutation(&mut self, command_id: &str, result: &Result<Value>) {
        let failure = match result {
            Err(error) => Some(format!("{error:#}")),
            Ok(response)
                if response.pointer("/failure/code").and_then(Value::as_str)
                    == Some("OUTCOME_UNKNOWN") =>
            {
                Some(projection::readable(&response["failure"]))
            }
            _ => None,
        };
        if let Some(failure) = failure {
            if let Some(mutation) = self.mutations.get_mut(command_id) {
                mutation.unknown = Some(failure);
            }
        } else {
            if let Some(mutation) = self.mutations.remove(command_id) {
                if let Some(view) = self.views.get_mut(&mutation.selection.work) {
                    view.records.remove(&format!("unresolved:{command_id}"));
                }
                self.chat
                    .records
                    .remove(&format!("unresolved:{command_id}"));
            }
            if self.retry.as_ref().is_some_and(|(_, _, kind)| {
                matches!(kind,
                JobKind::Send(operation) if operation.command_id == command_id)
            }) {
                self.retry = None;
            }
        }
    }
    fn response(
        &mut self,
        work: Option<String>,
        input: String,
        result: Result<Value>,
        mutation: Option<String>,
    ) {
        let captured_mutation = mutation
            .as_ref()
            .and_then(|id| self.mutations.get(id))
            .cloned();
        let conversation_scope = mutation
            .as_ref()
            .and_then(|id| self.mutations.get(id))
            .filter(|mutation| mutation.operation.method == "conversation.submit")
            .map(|mutation| mutation.selection.clone());
        if let Some(id) = &mutation {
            self.settle_mutation(id, &result);
        }
        match result {
            Err(error) => {
                self.notice = format!("{error:#}");
                self.notice_kind = NoticeKind::Error;
                self.append_outcome(
                    work,
                    &json!({"failure":{"message":self.notice}}),
                    mutation.as_deref(),
                );
            }
            Ok(response) => {
                if let Some(view) = response.pointer("/data/workView") {
                    self.observe(&json!({"data":view}));
                }
                if captured_mutation.as_ref().is_some_and(|mutation| {
                    matches!(
                        mutation.operation.method.as_str(),
                        "work.continue" | "work.claim_executor"
                    )
                }) {
                    if let Some(conversation) = response.pointer("/data/conversation") {
                        self.append(work.clone(), conversation);
                    }
                }
                if captured_mutation
                    .as_ref()
                    .is_some_and(|mutation| mutation.operation.method == "work.open")
                    && status_exit_code(&response) == 0
                {
                    if let Err(error) = self.open_work(&response["data"]) {
                        self.notice = format!("{error:#}");
                        self.notice_kind = NoticeKind::Error;
                        return;
                    }
                }
                self.observe(&response);
                self.set_response_notice(&response);
                if matches!(status_exit_code(&response), 0 | 2 | 3) {
                    if self.context.global_conversation
                        && mutation.is_none()
                        && input.trim_start().starts_with('/')
                    {
                        let data = response.get("data").unwrap_or(&response);
                        let text = if data.get("work").is_some() {
                            projection::brief(data)
                        } else {
                            projection::readable(data)
                        };
                        self.chat.records.insert("command-result".into(), json!({"text":
                            if text.is_empty() { t!("agent_center.console_diagnostics").into_owned() } else { text }}));
                    }
                    if self.context.global_conversation {
                        let belongs_to_editor = captured_mutation
                            .as_ref()
                            .map(|captured| {
                                captured.origin == MutationOrigin::Editor
                                    && captured.selection.conversation
                                        == self.context.conversation_id
                                    && captured.selection.console == self.context.console_session_id
                            })
                            .unwrap_or_else(|| input.trim_start().starts_with('/'));
                        if belongs_to_editor && self.chat.draft == input {
                            self.chat.clear_draft();
                        }
                    }
                    let view = self.views.entry(work.clone()).or_default();
                    if !self.context.global_conversation
                        && view.draft == input
                        && conversation_scope
                            .as_ref()
                            .is_none_or(|scope| view.context.as_ref() == Some(scope))
                    {
                        view.clear_draft();
                    }
                    if mutation.as_ref().is_some_and(|id| {
                        self.pending
                            .as_ref()
                            .is_some_and(|pending| pending.operation.command_id == *id)
                    }) {
                        if self.context.global_conversation
                            && self.pending.as_ref().is_some_and(|pending| {
                                pending.preview.get("humanActionProposal").is_some()
                            })
                        {
                            self.focus = Focus::Composer;
                            self.cards = None;
                        }
                        self.pending = None;
                        self.form = None;
                    }
                }
                self.append_outcome(work, &response, mutation.as_deref());
            }
        }
    }
    fn append_outcome(&mut self, work: Option<String>, response: &Value, mutation: Option<&str>) {
        if let Some(id) = mutation.filter(|id| {
            self.mutations
                .get(*id)
                .is_some_and(|mutation| mutation.unknown.is_some())
        }) {
            if self.context.global_conversation {
                self.chat
                    .records
                    .insert(format!("unresolved:{id}"), response.clone());
            }
            let view = self.views.entry(work).or_default();
            view.records
                .insert(format!("unresolved:{id}"), response.clone());
            view.transcript.push(response.clone());
            if view.transcript.len() > 200 {
                view.transcript.remove(0);
            }
        } else {
            self.append(work, response);
        }
    }
    fn select_conversation(&mut self) {
        if self.context.global_conversation {
            return;
        }
        self.context.conversation_id = self
            .conversations
            .entry((
                self.context.project_id.clone(),
                self.context.work_id.clone(),
            ))
            .or_insert_with(|| uuid::Uuid::new_v4().to_string())
            .clone();
        self.select_console_binding();
    }
    fn select_console_binding(&mut self) {
        if self.context.global_conversation {
            return;
        }
        // v1 registration binds a console ID to one project/conversation, not a window.
        self.context.console_session_id = self
            .conversation_consoles
            .entry(self.context.conversation_id.clone())
            .or_insert_with(|| uuid::Uuid::new_v4().to_string())
            .clone();
    }
    fn append(&mut self, work: Option<String>, value: &Value) {
        if let Some(data) = value.get("data") {
            self.append(work.clone(), data);
        }
        if let Some(items) = value.get("items").and_then(Value::as_array) {
            for item in items {
                self.append(work.clone(), item);
            }
            return;
        }
        self.append_global_conversation(value);
        let record_conversation = if value["kind"] == "Conversation" {
            value["id"].as_str()
        } else {
            value["conversationId"].as_str()
        };
        if let Some(conversation) = record_conversation {
            if let Some(bound) = self.views.get(&work).and_then(|view| view.context.as_ref()) {
                if self.conversation_targets.get(&bound.conversation) == Some(&work)
                    && conversation != bound.conversation
                {
                    return;
                }
            }
        }
        if let Some(proposal) = value
            .get("proposal")
            .filter(|proposal| proposal["kind"] == "HumanActionProposal")
        {
            self.append(work.clone(), proposal);
        }
        if value["kind"] == "HumanActionProposal" {
            self.remember_human_proposal(value);
        }
        if self.context.global_conversation {
            if value.get("failure").is_some() {
                self.chat.records.insert("failure".into(), value.clone());
            }
        }
        if value["kind"] == "Conversation" {
            if let (Some(conversation), Some(requests)) =
                (value["id"].as_str(), value["intakeRequests"].as_array())
            {
                let current: BTreeSet<_> = requests
                    .iter()
                    .filter_map(|request| request["id"].as_str())
                    .collect();
                self.views
                    .entry(work.clone())
                    .or_default()
                    .records
                    .retain(|id, record| {
                        record["kind"] != "IntakeRequest"
                            || record["conversationId"] != conversation
                            || current.contains(id.as_str())
                    });
            }
        }
        for field in ["intakeRequests", "inputRequests", "actionProposals"] {
            if let Some(items) = value[field].as_array() {
                for item in items {
                    self.append(work.clone(), item);
                }
            }
        }
        if matches!(
            value["kind"].as_str(),
            Some("IntakeRequest" | "DecisionRequest")
        ) && value["status"]
            .as_str()
            .is_some_and(|status| status != "Open")
            && self.form.as_ref().is_some_and(|form| {
                form.question["id"] == value["id"]
                    && value["version"].as_u64().unwrap_or(0)
                        >= form.question["version"].as_u64().unwrap_or(0)
            })
        {
            self.form = None;
            self.notice = t!("agent_center.status_conflict").into_owned();
            self.notice_kind = NoticeKind::Error;
        }
        let view = self.views.entry(work).or_default();
        if value["kind"] == "ConversationItem" {
            Self::merge_chat_message(view, value);
        }
        if let Some(messages) = value.get("messages").and_then(Value::as_array) {
            for message in messages {
                Self::merge_chat_message(view, message);
            }
            if view.messages.len() > 200 {
                view.messages.drain(..view.messages.len() - 200);
            }
            return;
        }
        if let (Some(id), Some(delta)) = (value["messageId"].as_str(), value["text"].as_str()) {
            if let Some(message) = view.messages.iter_mut().find(|message| message["id"] == id) {
                let mut text = message["text"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| {
                        message["parts"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|part| part["text"].as_str())
                            .collect()
                    });
                text.push_str(delta);
                message["text"] = json!(text);
            } else {
                view.messages
                    .push(json!({"id":id,"role":"assistant","text":delta}));
            }
        }
        if let Some(id) = value["id"].as_str() {
            let replace = view.records.get(id).is_none_or(|previous| {
                value["version"].as_u64().unwrap_or(0) >= previous["version"].as_u64().unwrap_or(0)
            });
            if replace {
                view.records.insert(id.into(), value.clone());
            }
        }
        if value.get("failure").is_some() {
            view.records.insert("failure".into(), value.clone());
        }
        view.transcript.push(value.clone());
        if view.transcript.len() > 200 {
            view.transcript.remove(0);
        }
    }
    fn append_global_conversation(&mut self, value: &Value) {
        let conversation = if self.context.global_conversation {
            self.context.conversation_id.as_str()
        } else {
            self.global_selection.conversation.as_str()
        };
        let snapshot = value["kind"] == "Conversation" && value["id"] == conversation;
        let correlated = value["conversationId"] == conversation;
        if correlated && value["kind"] == "HumanActionProposal" {
            if let Some(id) = value["id"].as_str() {
                if self
                    .chat
                    .records
                    .get(id)
                    .is_none_or(|old| old["version"].as_u64() < value["version"].as_u64())
                {
                    self.chat.records.insert(id.into(), value.clone());
                }
            }
        }
        if let Some(messages) = value["messages"].as_array() {
            for message in messages
                .iter()
                .filter(|message| snapshot || message["conversationId"] == conversation)
            {
                Self::merge_chat_message(&mut self.chat, message);
            }
        } else if correlated && value["kind"] == "ConversationItem" {
            Self::merge_chat_message(&mut self.chat, value);
        } else if let (Some(id), Some(delta), true) = (
            value["messageId"].as_str(),
            value["text"].as_str(),
            correlated,
        ) {
            if let Some(message) = self
                .chat
                .messages
                .iter_mut()
                .find(|message| message["id"] == id)
            {
                let mut text = message["text"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| {
                        message["parts"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|part| part["text"].as_str())
                            .collect::<String>()
                    });
                text.push_str(delta);
                message["text"] = json!(text);
            } else if correlated {
                self.chat
                    .messages
                    .push(json!({"id":id,"conversationId":conversation,"text":delta}));
            }
        }
        if snapshot || correlated {
            self.chat.transcript.push(value.clone());
            if self.chat.transcript.len() > 200 {
                self.chat.transcript.remove(0);
            }
        }
        if self.chat.messages.len() > 200 {
            self.chat.messages.drain(..self.chat.messages.len() - 200);
        }
    }
    fn merge_chat_message(chat: &mut WorkView, message: &Value) {
        let Some(id) = message["id"].as_str().filter(|id| !id.is_empty()) else {
            tracing::warn!("Ignoring global conversation item without a message identity");
            return;
        };
        if let Some(existing) = chat
            .messages
            .iter_mut()
            .find(|existing| existing["id"].as_str() == Some(id))
        {
            if message["version"].as_u64().unwrap_or(0) >= existing["version"].as_u64().unwrap_or(0)
            {
                *existing = message.clone();
            }
        } else {
            chat.messages.push(message.clone());
        }
    }
    fn observe(&mut self, response: &Value) {
        capture_versions(&mut self.context, response);
        if let Some(items) = response.pointer("/data/items").and_then(Value::as_array) {
            if !items.is_empty() && items.iter().all(|item| item.get("work").is_some()) {
                for item in items {
                    self.remember_work(item);
                }
            }
            if !items.is_empty() && items.iter().all(|item| item["kind"] == "Project") {
                self.projects = items.clone();
            }
        }
        if let Some(data) = response.get("data") {
            self.remember_work(data);
        }
        for (key, version) in &self.context.versions {
            let current = self.event_versions.entry(key.clone()).or_default();
            *current = (*current).max(*version);
        }
    }
    fn observe_inbox(&mut self, response: &Value, refresh: &InboxRefresh) {
        if status_exit_code(response) != 0 {
            return;
        }
        let Some(items) = response.pointer("/data/items").and_then(Value::as_array) else {
            return;
        };
        let scope = refresh.operation.params["workId"].as_str();
        // An in-flight list must not erase or resurrect obligations updated by
        // events since the read was queued. Keep tombstone versions as well.
        let changed = |id: &str, versions: &BTreeMap<(String, String), u64>| {
            let key = ("AttentionItem".into(), id.into());
            versions.get(&key).copied().unwrap_or(0)
                > refresh.versions.get(&key).copied().unwrap_or(0)
        };
        let after = refresh.operation.params["afterId"].as_str();
        let next = response
            .pointer("/data/nextAfterId")
            .and_then(Value::as_str);
        self.inbox.retain(|item| {
            let id = item["id"].as_str().unwrap_or("");
            scope.is_some_and(|work| item["workId"] != work)
                || after.is_some_and(|after| id <= after)
                || next.is_some_and(|next| id > next)
                || changed(id, &self.event_versions)
        });
        for item in items {
            let Some(id) = item["id"].as_str() else {
                continue;
            };
            if item["kind"] != "AttentionItem"
                || scope.is_some_and(|work| item["workId"] != work)
                || changed(id, &self.event_versions)
            {
                continue;
            }
            self.inbox.retain(|existing| existing["id"] != id);
            if attention_open(item) {
                self.inbox.push(item.clone());
            }
        }
        self.observe(response);
    }
    fn remember_work(&mut self, view: &Value) {
        let work = &view["work"];
        if let (Some(id), Some(version)) = (work["id"].as_str(), work["version"].as_u64()) {
            let current = self
                .context
                .versions
                .entry(("Work".into(), id.into()))
                .or_default();
            *current = (*current).max(version);
            if let Some(existing) = self
                .works
                .iter_mut()
                .find(|existing| existing["work"]["id"] == id)
            {
                if existing["work"]["version"].as_u64().unwrap_or(0) <= version {
                    *existing = view.clone();
                }
            } else {
                self.works.push(view.clone());
            }
        }
        let candidate = &view["candidate"];
        if let (Some(id), Some(version)) = (candidate["id"].as_str(), candidate["version"].as_u64())
        {
            let current = self
                .context
                .versions
                .entry(("DeliveryCandidate".into(), id.into()))
                .or_default();
            *current = (*current).max(version);
        }
    }
    fn receive(&mut self, update: Update) {
        self.queued_jobs = self.queued_jobs.saturating_sub(1);
        self.busy = self.queued_jobs != 0;
        match update.result {
            Ok(Outcome::Subscribed { scope, response }) => {
                self.refresh_needed = true;
                if let Some(snapshot) = response.pointer("/data/snapshot") {
                    self.observe(&json!({"data":snapshot}));
                    let target = scope["id"]
                        .as_str()
                        .and_then(|id| self.conversation_targets.get(id))
                        .cloned()
                        .flatten();
                    self.append(target, snapshot);
                } else {
                    self.append(update.work, &response);
                }
            }
            Ok(Outcome::Reconnected { snapshots, .. }) => {
                for (scope, snapshot) in snapshots {
                    self.observe(&json!({"data":snapshot}));
                    let target = scope["id"]
                        .as_str()
                        .and_then(|id| self.conversation_targets.get(id))
                        .cloned()
                        .flatten();
                    self.append(target, &snapshot);
                }
                self.refresh_needed = true;
                // Mutations stay blocked until a fresh work/project/inbox read.
                self.stale = true;
            }
            Ok(Outcome::Prepared(operation, origin)) => {
                self.prepared = Some((operation, update.work, update.input, origin));
            }
            Err(error) => {
                self.response(update.work, update.input, Err(error), None);
            }
            Ok(Outcome::ReadResponse(response)) => {
                self.response(update.work, update.input, Ok(response), None);
            }
            Ok(Outcome::Mutation { command_id, result }) => {
                self.response(update.work, update.input, result, Some(command_id));
            }
            Ok(Outcome::Refreshed {
                works,
                projects,
                decisions,
                inbox,
                inbox_versions,
            }) => {
                self.observe_inbox(
                    &inbox,
                    &InboxRefresh {
                        operation: Operation::read("inbox.list", json!({"limit":100})),
                        versions: inbox_versions,
                    },
                );
                self.observe(&works);
                self.observe(&projects);
                self.observe(&decisions);
                if let Some(items) = decisions.pointer("/data/items").and_then(Value::as_array) {
                    for item in items {
                        self.append(item["workId"].as_str().map(str::to_owned), item);
                    }
                }
                self.stale = false;
                self.notice = t!("agent_center.status_ok").into_owned();
                self.notice_kind = NoticeKind::Info;
            }
            Ok(Outcome::Inspected(inspection)) => {
                capture_versions(
                    &mut self.context,
                    &json!({"data":{"work":inspection["work"],"candidate":inspection["candidate"]}}),
                );
                let view = self.views.entry(update.work).or_default();
                if view
                    .inspection
                    .as_ref()
                    .and_then(InspectionIdentity::from_view)
                    != InspectionIdentity::from_view(&inspection)
                {
                    view.evidence = None;
                    view.delivery_scroll = 0;
                }
                view.inspection = Some(inspection);
                view.show_delivery = true;
                self.notice = t!("agent_center.console_inspect").into_owned();
                self.notice_kind = NoticeKind::Info;
            }
            Ok(Outcome::Evidence {
                response,
                inspection,
                operation,
            }) => {
                let view = self.views.entry(update.work.clone()).or_default();
                if view
                    .inspection
                    .as_ref()
                    .and_then(InspectionIdentity::from_view)
                    .as_ref()
                    == Some(&inspection)
                {
                    if status_exit_code(&response) == 0
                        && response["data"]["artifactId"] == operation.params["artifactId"]
                        && operation
                            .params
                            .get("relativePath")
                            .is_none_or(|path| response["data"]["relativePath"] == *path)
                    {
                        view.evidence = response
                            .get("data")
                            .cloned()
                            .map(|data| EvidencePage { inspection, data });
                        view.delivery_scroll = 0;
                        view.show_delivery = true;
                        self.set_response_notice(&response);
                    } else if status_exit_code(&response) != 0 {
                        self.append(update.work, &response);
                        self.set_response_notice(&response);
                    } else {
                        self.notice = t!("agent_center.invalid_response").into_owned();
                        self.notice_kind = NoticeKind::Error;
                    }
                }
            }
            Ok(Outcome::Inbox { response, refresh }) => {
                self.observe_inbox(&response, &refresh);
                self.set_response_notice(&response);
                if status_exit_code(&response) == 0 {
                    if self.context.global_conversation
                        && self.chat.draft == update.input
                        && update.input.trim_start().starts_with('/')
                    {
                        self.chat.clear_draft();
                    }
                    let view = self.views.entry(update.work.clone()).or_default();
                    if view.draft == update.input {
                        view.clear_draft();
                    }
                }
                self.append(update.work, &response);
            }
            Ok(Outcome::StartPreview {
                operation,
                preview,
                origin,
            }) => {
                self.observe(&preview);
                let target_label = self.work_label(update.work.as_deref());
                self.pending = Some(PendingConfirmation {
                    operation,
                    preview,
                    work: update.work,
                    input: update.input,
                    scroll: 0,
                    target_label,
                    origin,
                });
                self.notice = t!("agent_center.confirm_prompt").into_owned();
                self.notice_kind = NoticeKind::Attention;
            }
            Ok(Outcome::SelectedWork { id, response }) => {
                self.observe(&response);
                if status_exit_code(&response) == 0 {
                    if self.context.global_conversation
                        && self.chat.draft == update.input
                        && update.input.trim_start().starts_with("/work use")
                    {
                        self.chat.clear_draft();
                    }
                    let previous_context = self.selection();
                    let previous = self.views.entry(update.work).or_default();
                    if previous.draft == update.input
                        && update.input.trim_start().starts_with("/work use")
                    {
                        previous.clear_draft();
                    }
                    self.return_work = Some(previous_context);
                    self.select(Some(id.clone()));
                    self.append(Some(id), &response);
                } else {
                    self.append(update.work, &response);
                }
                self.set_response_notice(&response);
            }
            Ok(Outcome::SelectedProject { id, response }) => {
                self.observe(&response);
                if status_exit_code(&response) == 0 {
                    if self.context.global_conversation
                        && self.chat.draft == update.input
                        && update.input.trim_start().starts_with("/project use")
                    {
                        self.chat.clear_draft();
                    }
                    if self.context.work_id.is_some()
                        && self.context.project_id.as_deref() != Some(id.as_str())
                    {
                        self.return_work = Some(self.selection());
                        self.select(None);
                    }
                    self.context.project_id = Some(id);
                    self.context.context_version += 1;
                    self.select_conversation();
                    self.sync_view_context();
                    let view = self.views.entry(update.work.clone()).or_default();
                    if view.draft == update.input {
                        view.clear_draft();
                    }
                }
                self.append(update.work, &response);
                self.set_response_notice(&response);
            }
        }
        if let Some(notice) = self.unresolved_notice() {
            self.notice = notice;
        }
    }

    fn event(&mut self, event: Value) {
        if event["type"] == "stream_error" {
            self.stale = true;
            self.notice = format!(
                "{}: {}",
                t!("agent_center.stream_failed"),
                projection::readable(&event["failure"])
            );
            return;
        }
        let Some(id) = event["eventId"].as_str() else {
            return;
        };
        if !self.event_ids.insert(id.into()) {
            return;
        }
        self.refresh_needed = true;
        capture_versions(&mut self.context, &event);
        let work = event["workId"].as_str().map(str::to_owned);
        if let Some(changes) = event["changes"].as_array() {
            for change in changes {
                let subject = &change["subject"];
                let key = (
                    subject["kind"].as_str().unwrap_or("").to_owned(),
                    subject["id"].as_str().unwrap_or("").to_owned(),
                );
                let version = subject["version"].as_u64().unwrap_or(0);
                let view = &change["view"];
                if let (Some(message), Some(part), Some(chunk)) = (
                    view["messageId"].as_str(),
                    view["partId"].as_str(),
                    view["chunkIndex"].as_u64(),
                ) {
                    if !self.chunks.insert((message.into(), part.into(), chunk)) {
                        continue;
                    }
                } else {
                    let previous = self.event_versions.entry(key).or_default();
                    if version <= *previous {
                        continue;
                    }
                    *previous = version;
                }
                if subject["kind"] == "Work" {
                    self.remember_work(view);
                }
                if subject["kind"] == "AttentionItem" {
                    self.inbox.retain(|item| item["id"] != subject["id"]);
                    if attention_open(view) {
                        self.inbox.push(view.clone());
                    }
                }
                let target = work.clone().or_else(|| {
                    if let Some(conversation) = view["conversationId"].as_str() {
                        self.conversation_targets
                            .get(conversation)
                            .cloned()
                            .flatten()
                    } else if subject["kind"] == "Conversation" {
                        subject["id"]
                            .as_str()
                            .and_then(|id| self.conversation_targets.get(id))
                            .cloned()
                            .flatten()
                    } else {
                        None
                    }
                });
                self.append(target, view);
            }
        }
        self.cursor = event["cursor"].as_str().map(str::to_owned);
        // Routine progress/text is a silent view update. Human attention has a
        // separate, coalesced surface derived from current obligations.
    }
}

fn attention_open(item: &Value) -> bool {
    matches!(
        item["state"].as_str().or_else(|| item["status"].as_str()),
        Some("Open")
    )
}

fn status_label(response: &Value) -> String {
    match response["status"].as_str() {
        Some("ok") => t!("agent_center.status_ok").into_owned(),
        Some("pending") => t!("agent_center.console_pending").into_owned(),
        Some("needs_input") => t!("agent_center.console_next_attention").into_owned(),
        Some("conflict") => t!("agent_center.status_conflict").into_owned(),
        Some("unsupported") => t!("agent_center.status_unsupported").into_owned(),
        _ => t!("agent_center.status_error").into_owned(),
    }
}

#[derive(Clone)]
enum JobKind {
    OpenTab(String),
    Subscribe(Value),
    Recover(Vec<Value>),
    Refresh,
    Inspect(String),
    Evidence {
        operation: Operation,
        inspection: InspectionIdentity,
    },
    Send(Operation),
    RefreshInbox(InboxRefresh),
    Resolve {
        args: Vec<String>,
        context: CommandContext,
        fallback_command_id: String,
    },
    PrepareStart(String),
    PrepareApply(commands::preparation::ApplyTarget),
    PrepareTransfer(commands::preparation::TransferTarget),
    PrepareProposal(commands::preparation::ProposalIntake),
    SelectWork(String),
    SelectProject(String),
}
struct Job {
    kind: JobKind,
    work: Option<String>,
    input: String,
    conversation: Option<String>,
    inbox_versions: BTreeMap<(String, String), u64>,
    origin: MutationOrigin,
}
enum Outcome {
    Subscribed {
        scope: Value,
        response: Value,
    },
    Reconnected {
        client: Arc<Client>,
        subscriptions: BTreeMap<String, Value>,
        snapshots: Vec<(Value, Value)>,
    },
    Refreshed {
        works: Value,
        projects: Value,
        decisions: Value,
        inbox: Value,
        inbox_versions: BTreeMap<(String, String), u64>,
    },
    Inspected(Value),
    Evidence {
        response: Value,
        inspection: InspectionIdentity,
        operation: Operation,
    },
    Prepared(Operation, MutationOrigin),
    ReadResponse(Value),
    Mutation {
        command_id: String,
        result: Result<Value>,
    },
    Inbox {
        response: Value,
        refresh: InboxRefresh,
    },
    SelectedWork {
        id: String,
        response: Value,
    },
    SelectedProject {
        id: String,
        response: Value,
    },
    StartPreview {
        operation: Operation,
        preview: Value,
        origin: MutationOrigin,
    },
}
struct Update {
    work: Option<String>,
    input: String,
    result: Result<Outcome>,
    conversation: Option<String>,
}

async fn prepare_action(
    client: &Client,
    work_id: &str,
    apply: Option<&commands::preparation::ApplyTarget>,
) -> Result<Outcome> {
    match commands::preparation::prepare(client, work_id, apply).await? {
        commands::preparation::Preparation::Ready { operation, preview } => {
            Ok(Outcome::StartPreview {
                operation,
                preview,
                origin: MutationOrigin::Action,
            })
        }
        commands::preparation::Preparation::Response(response) => {
            Ok(Outcome::ReadResponse(response))
        }
    }
}

async fn resolve_operation(
    args: Vec<String>,
    context: CommandContext,
    fallback_command_id: String,
) -> Result<Outcome> {
    let mut args = resolve_input_args(args, false).await?;
    if !args
        .iter()
        .any(|arg| arg == "--request-json" || arg == "--command-id")
    {
        args.extend(["--command-id".into(), fallback_command_id]);
    }
    match commands::compile(&args, &context, true)? {
        Action::Operation(operation) if operation.confirmation => Ok(Outcome::StartPreview {
            preview: operation_preview(&operation),
            operation,
            origin: MutationOrigin::Editor,
        }),
        Action::Operation(operation) => Ok(Outcome::Prepared(operation, MutationOrigin::Editor)),
        _ => bail!("{}", t!("agent_center.invalid_request")),
    }
}

fn operation_preview(operation: &Operation) -> Value {
    json!({"method":operation.method,"params":operation.params,"ifMatch":operation.if_match,
        "commandId":operation.command_id})
}

fn absent_intake_conversation(scope: &Value, response: &Value) -> bool {
    scope["kind"] == "Conversation"
        && response.pointer("/failure/code").and_then(Value::as_str) == Some("INVALID_REFERENCE")
}

pub(super) struct TerminalGuard;
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if let Err(error) = disable_raw_mode() {
            tracing::warn!(target:"wta::agent_center", %error, "Unable to restore terminal raw mode");
        }
        if let Err(error) = execute!(
            std::io::stdout(),
            DisableBracketedPaste,
            LeaveAlternateScreen
        ) {
            tracing::warn!(target:"wta::agent_center", %error, "Unable to leave Agent Center screen");
        }
    }
}

pub async fn run_async(work: Option<uuid::Uuid>) -> Result<()> {
    if let Some(root) = std::env::var_os("INTELLIGENT_TERMINAL_WORK_DEMO_STATE") {
        let root = std::path::PathBuf::from(root);
        anyhow::ensure!(
            !root.as_os_str().is_empty() && root.is_absolute() && work.is_none(),
            "INTELLIGENT_TERMINAL_WORK_DEMO_STATE must be an absolute, nonempty path; --work is unavailable in the demo"
        );
        return super::demo::run(Some(root), false, false).await;
    }
    // Fail before taking over the terminal. Closing this client does not stop work.
    let mut connection = Arc::new(
        Client::connect()
            .await
            .with_context(|| t!("agent_center.service_unavailable").into_owned())?,
    );
    let initial = send(
        &connection,
        &Operation::read("work.list", json!({"limit":100})),
    )
    .await?;
    if status_exit_code(&initial) != 0 {
        bail!("{}: {}", t!("agent_center.service_unavailable"), initial);
    }
    let subscription = send(
        &connection,
        &Operation::read("events.subscribe", json!({"scope":{"kind":"WorkList"}})),
    )
    .await?;
    if status_exit_code(&subscription) != 0 {
        bail!("{}: {}", t!("agent_center.stream_failed"), subscription);
    }
    let (jobs_tx, mut jobs_rx) = mpsc::unbounded_channel::<Job>();
    let (updates_tx, mut updates_rx) = mpsc::unbounded_channel::<Update>();
    let mut worker_connection = connection.clone();
    let worker = tokio::spawn(async move {
        while let Some(job) = jobs_rx.recv().await {
            let mut result = match job.kind {
                JobKind::OpenTab(id) => task_tab::open(&id).await.map(Outcome::ReadResponse),
                JobKind::Subscribe(scope) => send(
                    &worker_connection,
                    &Operation::read("events.subscribe", json!({"scope":scope})),
                )
                .await
                .map(|response| Outcome::Subscribed { scope, response }),
                JobKind::Recover(scopes) => {
                    async {
                        let client = Arc::new(worker_connection.reconnect().await?);
                        let mut subscriptions = BTreeMap::new();
                        let mut snapshots = Vec::new();
                        for scope in scopes {
                            let response = send(
                                &client,
                                &Operation::read("events.subscribe", json!({"scope":scope})),
                            )
                            .await?;
                            // An uncertain submit may not have created its
                            // conversation. Keep the authority available for
                            // explicit reconciliation of the original command.
                            if absent_intake_conversation(&scope, &response) {
                                snapshots.push((scope, json!({"failure":response["failure"]})));
                                continue;
                            }
                            anyhow::ensure!(
                                status_exit_code(&response) == 0,
                                "{}",
                                projection::readable(&response["failure"])
                            );
                            if let Some(id) = response
                                .pointer("/data/subscriptionId")
                                .and_then(Value::as_str)
                            {
                                subscriptions.insert(id.into(), scope.clone());
                            }
                            if let Some(snapshot) = response.pointer("/data/snapshot") {
                                snapshots.push((scope, snapshot.clone()));
                            }
                        }
                        worker_connection = client.clone();
                        Ok(Outcome::Reconnected {
                            client,
                            subscriptions,
                            snapshots,
                        })
                    }
                    .await
                }
                JobKind::Refresh => {
                    async {
                        let mut responses = Vec::new();
                        for method in ["work.list", "project.list", "decision.list", "inbox.list"] {
                            let response = send(
                                &worker_connection,
                                &Operation::read(method, json!({"limit":100})),
                            )
                            .await?;
                            anyhow::ensure!(
                                status_exit_code(&response) == 0,
                                "{}",
                                projection::readable(&response["failure"])
                            );
                            responses.push(response);
                        }
                        let mut responses = responses.into_iter();
                        Ok(Outcome::Refreshed {
                            works: responses.next().unwrap_or(Value::Null),
                            projects: responses.next().unwrap_or(Value::Null),
                            decisions: responses.next().unwrap_or(Value::Null),
                            inbox: responses.next().unwrap_or(Value::Null),
                            inbox_versions: job.inbox_versions,
                        })
                    }
                    .await
                }
                JobKind::Inspect(id) => workflow::inspect(&worker_connection, &id)
                    .await
                    .map(Outcome::Inspected),
                JobKind::Evidence {
                    operation,
                    inspection,
                } => {
                    let response = send(&worker_connection, &operation).await.unwrap_or_else(|error|
                        json!({"status":"error","failure":{"code":"READ_FAILED","message":format!("{error:#}")}}));
                    Ok(Outcome::Evidence {
                        response,
                        inspection,
                        operation,
                    })
                }
                JobKind::Send(operation) if operation.mutation => Ok(Outcome::Mutation {
                    command_id: operation.command_id.clone(),
                    result: send(&worker_connection, &operation).await,
                }),
                JobKind::Send(operation) => send(&worker_connection, &operation)
                    .await
                    .map(Outcome::ReadResponse),
                JobKind::RefreshInbox(refresh) => send(&worker_connection, &refresh.operation)
                    .await
                    .map(|response| Outcome::Inbox { response, refresh }),
                JobKind::Resolve {
                    args,
                    context,
                    fallback_command_id,
                } => resolve_operation(args, context, fallback_command_id).await,
                JobKind::PrepareStart(work) => {
                    prepare_action(&worker_connection, &work, None).await
                }
                JobKind::PrepareApply(target) => {
                    prepare_action(&worker_connection, &target.work_id, Some(&target)).await
                }
                JobKind::PrepareTransfer(target) => {
                    match commands::preparation::prepare_transfer(&worker_connection, &target).await
                    {
                        Ok(commands::preparation::Preparation::Ready { operation, preview }) => {
                            Ok(Outcome::StartPreview {
                                operation,
                                preview,
                                origin: job.origin,
                            })
                        }
                        Ok(commands::preparation::Preparation::Response(response)) => {
                            Ok(Outcome::ReadResponse(response))
                        }
                        Err(error) => Err(error),
                    }
                }
                JobKind::PrepareProposal(intake) => {
                    match commands::preparation::prepare_proposal(&worker_connection, &intake).await
                    {
                        Ok(commands::preparation::Preparation::Ready { operation, .. }) => {
                            Ok(Outcome::Prepared(operation, job.origin))
                        }
                        Ok(commands::preparation::Preparation::Response(response)) => {
                            Ok(Outcome::ReadResponse(response))
                        }
                        Err(error) => Err(error),
                    }
                }
                JobKind::SelectWork(id) => send(
                    &worker_connection,
                    &Operation::read("work.get", json!({"workId":id})),
                )
                .await
                .map(|response| Outcome::SelectedWork { id, response }),
                JobKind::SelectProject(id) => send(
                    &worker_connection,
                    &Operation::read("project.get", json!({"projectId":id})),
                )
                .await
                .map(|response| Outcome::SelectedProject { id, response }),
            };
            match &mut result {
                Ok(Outcome::StartPreview { origin, .. } | Outcome::Prepared(_, origin)) => {
                    *origin = job.origin
                }
                _ => {}
            }
            if updates_tx
                .send(Update {
                    work: job.work,
                    input: job.input,
                    conversation: job.conversation,
                    result,
                })
                .is_err()
            {
                break;
            }
        }
    });
    enable_raw_mode()?;
    let _guard = TerminalGuard;
    #[cfg(windows)]
    let mut input = input::ConsoleInput::new()?;
    #[cfg(not(windows))]
    let mut input = crossterm::event::EventStream::new();
    execute!(
        std::io::stdout(),
        EnterAlternateScreen,
        EnableBracketedPaste
    )?;
    let mut terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    let mut state = State::new();
    state.observe(&initial);
    state.append(None, &initial);
    {
        let projects = send(
            &connection,
            &Operation::read("project.list", json!({"limit":100})),
        )
        .await?;
        state.observe(&projects);
    }
    let inbox = send(
        &connection,
        &Operation::read("inbox.list", json!({"limit":100})),
    )
    .await?;
    state.observe_inbox(
        &inbox,
        &InboxRefresh {
            operation: Operation::read("inbox.list", json!({"limit":100})),
            versions: state.event_versions.clone(),
        },
    );
    if status_exit_code(&inbox) != 0 {
        state.append(None, &inbox);
    }
    if let Some(snapshot) = subscription.pointer("/data/snapshot") {
        state.observe(&json!({"data":snapshot}));
    }
    let mut conversation_subscriptions = BTreeSet::new();
    let mut events_open = true;
    let mut subscriptions = BTreeMap::new();
    if let Some(id) = subscription
        .pointer("/data/subscriptionId")
        .and_then(Value::as_str)
    {
        subscriptions.insert(id.to_owned(), json!({"kind":"WorkList"}));
    }
    queue_captured(&mut state, &jobs_tx, JobKind::Refresh, None, String::new())?;
    if let Some(work) = work {
        queue_captured(
            &mut state,
            &jobs_tx,
            JobKind::SelectWork(work.to_string()),
            None,
            String::new(),
        )?;
    }
    let mut paint = tokio::time::interval(std::time::Duration::from_millis(50));
    paint.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut refresh = tokio::time::interval(std::time::Duration::from_secs(2));
    refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut recovery_attempts = 0;
    let animation_started = std::time::Instant::now();
    loop {
        tokio::select! {
            _ = paint.tick() => {
                state.activity_frame = ((animation_started.elapsed().as_millis() / 120)
                    % crate::ui::shimmer::CYCLE_FRAMES as u128) as usize;
                let before = state.pending.as_ref().map(|pending| pending.scroll);
                terminal.draw(|frame| render(frame, &mut state))?;
                if state.composer_focused() {
                    terminal.hide_cursor()?;
                }
                let after = state.pending.as_ref().map(|pending| pending.scroll);
                if before != after {
                    tracing::debug!(target:"agent_center::navigation",
                        pid=std::process::id(), ?before, ?after,
                        diagnostics=state.diagnostics, "Preview scroll clamped during render");
                }
            }
            _ = refresh.tick() => {
                if !state.busy {
                    if (!events_open || state.reconnect_requested) && (recovery_attempts < 3 || state.reconnect_requested) {
                        if state.reconnect_requested {
                            state.reconnect_requested = false;
                            recovery_attempts = 0;
                        }
                        recovery_attempts += 1;
                        events_open = false;
                        state.stale = true;
                        let scopes = subscriptions.values().cloned()
                            .chain(std::iter::once(json!({"kind":"WorkList"})))
                            .chain(state.conversation_targets.keys().map(|id| json!({"kind":"Conversation","id":id})))
                            .map(|scope| (scope.to_string(), scope)).collect::<BTreeMap<_, _>>().into_values().collect();
                        queue_captured(&mut state, &jobs_tx, JobKind::Recover(scopes), None, String::new())?;
                    } else if events_open && state.refresh_needed {
                        state.refresh_needed = false;
                        queue_captured(&mut state, &jobs_tx, JobKind::Refresh, None, String::new())?;
                    }
                }
            }
            event = connection.next_event(), if events_open => {
                match event {
                    Ok(event) => {
                        if event["subscriptionId"].as_str().is_none_or(|id| !subscriptions.contains_key(id)) {
                            continue;
                        }
                        if event["type"] == "stream_error" {
                            state.event(event.clone());
                            events_open = false;
                        } else { state.event(event); }
                    },
                    Err(error) => {
                        events_open = false;
                        state.stale = true;
                        state.notice = format!("{}: {error:#}", t!("agent_center.stream_failed"));
                    }
                }
            }
            update = updates_rx.recv() => {
                if let Some(update) = update {
                    match &update.result {
                        Ok(Outcome::Reconnected { client, subscriptions: refreshed, .. }) => {
                            connection = client.clone();
                            subscriptions = refreshed.clone();
                            conversation_subscriptions = refreshed.values()
                                .filter(|scope| scope["kind"] == "Conversation")
                                .filter_map(|scope| scope["id"].as_str().map(str::to_owned)).collect();
                            events_open = true;
                            recovery_attempts = 0;
                        }
                        Ok(Outcome::Subscribed { scope, response }) => {
                            if let Some(id) = response.pointer("/data/subscriptionId").and_then(Value::as_str) {
                                subscriptions.insert(id.into(), scope.clone());
                            }
                            events_open = status_exit_code(response) == 0;
                        }
                        _ => {}
                    }
                    let opened_conversation = match &update.result {
                        Ok(Outcome::Mutation { result: Ok(response), .. }) if response["status"] == "ok" =>
                            response.pointer("/data/context/conversationId").and_then(Value::as_str).map(str::to_owned),
                        _ => None,
                    };
                    let submitted_conversation = opened_conversation.clone().or(update.conversation.clone());
                    let intake_recorded = opened_conversation.is_some() || matches!(&update.result,
                        Ok(Outcome::Mutation { result: Ok(response), .. })
                            if response.pointer("/data/intakeTurnId").is_some());
                    state.receive(update);
                    if !events_open { state.stale = true; }
                    if let Some((operation, work, input, origin)) = state.prepared.take() {
                        if let Err(error) = queue_captured_with_origin(&mut state, &jobs_tx, JobKind::Send(operation), work, input, origin) {
                            state.notice = format!("{error:#}");
                            state.notice_kind = NoticeKind::Error;
                        }
                    }
                    if let Some(conversation) = submitted_conversation.filter(|id| intake_recorded && !conversation_subscriptions.contains(id)) {
                        conversation_subscriptions.insert(conversation.clone());
                        // A subscriber can receive events before its request
                        // receipt reaches this loop. Pause consumption until the
                        // subscription ID and its snapshot are installed.
                        events_open = false;
                        state.stale = true;
                        queue_captured(&mut state, &jobs_tx, JobKind::Subscribe(json!({"kind":"Conversation","id":conversation})), None, String::new())?;
                    }
                }
            }
            event = input.next() => {
                let Some(event) = event else { break; };
                let event = event?;
                trace_navigation(&state, &event, "received");
                match &event {
                    Event::Key(key) if key.kind != KeyEventKind::Release => {
                        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                            break;
                        }
                        if let Err(error) = handle_key(&mut state, key.code, key.modifiers, &jobs_tx) {
                            state.notice = format!("{error:#}");
                            state.notice_kind = NoticeKind::Error;
                        }
                    }
                    Event::Paste(text) => handle_paste(&mut state, text),
                    _ => {}
                }
                trace_navigation(&state, &event, "handled");
            }
        }
    }
    worker.abort();
    terminal.show_cursor()?;
    Ok(())
}

pub(super) async fn run_demo(
    store: super::demo::Store,
    scenario: super::demo::scenario::Scenario,
) -> Result<()> {
    demo::run(store, scenario).await
}

fn handle_paste(state: &mut State, text: &str) {
    if state.task_list && !state.diagnostics && state.form.is_none() {
        return;
    }
    if let Some(form) = state.form.as_mut() {
        if state.pending.is_none() {
            form.follow = true;
            let field = &mut form.fields[form.selected];
            if field.choices.is_empty() {
                field.editor.insert(
                    &mut field.draft,
                    &text.replace("\r\n", "\n").replace('\r', "\n"),
                );
            }
        }
        return;
    }
    if state.menu.is_some() || state.focus != Focus::Composer {
        return;
    }
    if state.pending.is_none() {
        state
            .editor_view_mut()
            .insert(&text.replace("\r\n", "\n").replace('\r', "\n"));
        state.completion = 0;
    }
}

fn navigation_key(event: &Event) -> Option<KeyEvent> {
    match event {
        Event::Key(key)
            if matches!(
                key.code,
                KeyCode::PageUp | KeyCode::PageDown | KeyCode::F(12)
            ) =>
        {
            Some(*key)
        }
        _ => None,
    }
}

fn trace_navigation(state: &State, event: &Event, phase: &'static str) {
    if let Some(key) = navigation_key(event) {
        tracing::debug!(target:"agent_center::navigation",
            pid=std::process::id(), phase, code=?key.code, modifiers=?key.modifiers, kind=?key.kind,
            scroll=?state.pending.as_ref().map(|pending| pending.scroll),
            command_id=?state.pending.as_ref().map(|pending| pending.operation.command_id.as_str()),
            diagnostics=state.diagnostics, busy=state.busy,
            composer_focused=state.focus == Focus::Composer,
            menu_open=state.menu.is_some(), form_open=state.form.is_some(),
            "Navigation input");
    }
}

fn handle_key(
    state: &mut State,
    key: KeyCode,
    modifiers: KeyModifiers,
    jobs: &mpsc::UnboundedSender<Job>,
) -> Result<()> {
    if key == KeyCode::F(12) {
        state.diagnostics = !state.diagnostics;
        return Ok(());
    }
    if state.pending.is_some() {
        let inline = state.inline_approval();
        if inline && !state.busy {
            match key {
                KeyCode::Left => {
                    state.approval_choice = state.approval_choice.saturating_sub(1);
                    return Ok(());
                }
                KeyCode::Right | KeyCode::Tab => {
                    state.approval_choice = (state.approval_choice + 1) % 3;
                    return Ok(());
                }
                KeyCode::F(5) => {
                    state.approval_expanded = !state.approval_expanded;
                    if let Some(pending) = state.pending.as_mut() {
                        pending.scroll = 0;
                    }
                    return Ok(());
                }
                KeyCode::Esc => {
                    state.defer_approval(false);
                    return Ok(());
                }
                KeyCode::Enter
                    if !modifiers.contains(KeyModifiers::CONTROL) && state.approval_choice != 0 =>
                {
                    state.defer_approval(state.approval_choice == 1);
                    return Ok(());
                }
                _ => {}
            }
        }
        match key {
            KeyCode::Esc if !inline || !state.busy => {
                state.pending = None;
                state.retry = None;
                state.notice = t!("agent_center.confirm_cancelled").into_owned();
                state.notice_kind = NoticeKind::Info;
            }
            KeyCode::Enter
                if !state.busy
                    && (modifiers.contains(KeyModifiers::CONTROL)
                        || inline && state.focus == Focus::Actions) =>
            {
                if let Some(pending) = state.pending.as_ref() {
                    anyhow::ensure!(
                        state.confirmation_current(pending),
                        "{}",
                        t!("agent_center.status_conflict")
                    );
                    let kind = JobKind::Send(pending.operation.clone());
                    let work = pending.work.clone();
                    let input = pending.input.clone();
                    queue_captured(state, jobs, kind, work, input)?;
                }
            }
            KeyCode::PageUp => {
                if let Some(pending) = state.pending.as_mut() {
                    pending.scroll = pending.scroll.saturating_sub(10);
                }
            }
            KeyCode::PageDown => {
                if let Some(pending) = state.pending.as_mut() {
                    pending.scroll = pending.scroll.saturating_add(10);
                }
            }
            _ => {}
        }
        return Ok(());
    }
    let form_target = state
        .form
        .as_ref()
        .map(|form| state.work_label(form.work.as_deref()))
        .unwrap_or_default();
    if let Some(form) = state.form.as_mut() {
        if !matches!(key, KeyCode::PageUp | KeyCode::PageDown) {
            form.follow = true;
        }
        match key {
            KeyCode::PageUp => {
                form.follow = false;
                form.scroll = form.scroll.saturating_sub(10);
            }
            KeyCode::PageDown => {
                form.follow = false;
                form.scroll = form.scroll.saturating_add(10);
            }
            KeyCode::Esc => {
                state.form = None;
            }
            KeyCode::Up => {
                form.selected = form.selected.saturating_sub(1);
            }
            KeyCode::Down => {
                form.selected = (form.selected + 1).min(form.fields.len() - 1);
            }
            KeyCode::Enter if modifiers.contains(KeyModifiers::CONTROL) && !state.busy => {
                let operation = form.operation()?;
                state.pending = Some(PendingConfirmation {
                    operation,
                    preview: form.question.clone(),
                    work: form.work.clone(),
                    input: String::new(),
                    scroll: 0,
                    target_label: form_target,
                    origin: MutationOrigin::Action,
                });
            }
            KeyCode::Enter => {
                form.selected = (form.selected + 1).min(form.fields.len() - 1);
            }
            _ => {
                let field = &mut form.fields[form.selected];
                if !field.choices.is_empty() {
                    match key {
                        KeyCode::Left => {
                            field.choice = Some(field.choice.unwrap_or(0).saturating_sub(1))
                        }
                        KeyCode::Right => {
                            field.choice = Some(
                                field
                                    .choice
                                    .map(|choice| (choice + 1).min(field.choices.len() - 1))
                                    .unwrap_or(0),
                            )
                        }
                        KeyCode::Backspace | KeyCode::Delete => field.choice = None,
                        _ => {}
                    }
                } else if let KeyCode::Char(ch) = key {
                    if !modifiers.contains(KeyModifiers::CONTROL) {
                        field
                            .editor
                            .insert(&mut field.draft, ch.encode_utf8(&mut [0; 4]));
                    } else {
                        field.editor.key(&mut field.draft, key, modifiers);
                    }
                } else {
                    field.editor.key(&mut field.draft, key, modifiers);
                }
            }
        }
        return Ok(());
    }
    if key == KeyCode::F(1) {
        state.activate(workflow::Choice::Home, jobs)?;
        return Ok(());
    }
    if key == KeyCode::F(4) {
        state.focus = Focus::Composer;
        state.cards = None;
        state.menu = if state.menu.is_some() {
            None
        } else {
            Some(state.actions())
        };
        return Ok(());
    }
    if let Some(menu) = state.menu.as_mut() {
        match key {
            KeyCode::Esc => state.menu = None,
            KeyCode::Up => menu.selected = menu.selected.saturating_sub(1),
            KeyCode::Down => {
                menu.selected = (menu.selected + 1).min(menu.items.len().saturating_sub(1))
            }
            KeyCode::Enter if !state.busy => {
                if let Some((_, choice)) = menu.items.get(menu.selected) {
                    let choice = choice.clone();
                    state.activate(choice, jobs)?;
                }
            }
            _ => {}
        }
        return Ok(());
    }
    if key == KeyCode::F(2) {
        state.task_list = true;
        state.task_index = state
            .tasks()
            .iter()
            .position(|view| view["work"]["id"].as_str() == state.context.work_id.as_deref())
            .unwrap_or(0);
        state.focus = Focus::Composer;
        state.view_mut().details_open = false;
        state.cards = None;
        return Ok(());
    }
    if state.task_list && state.focus != Focus::Details {
        match key {
            KeyCode::Esc => {
                state.task_list = false;
            }
            KeyCode::Up => state.task_index = state.task_index.saturating_sub(1),
            KeyCode::Down => {
                state.task_index = (state.task_index + 1).min(state.tasks().len().saturating_sub(1))
            }
            KeyCode::Enter if !state.busy => {
                if let Some(id) = state
                    .tasks()
                    .get(state.task_index)
                    .and_then(|view| view["work"]["id"].as_str())
                    .map(str::to_owned)
                {
                    state.activate(workflow::Choice::SelectWork(id), jobs)?;
                }
            }
            KeyCode::Char('n' | 'N') => state.activate(workflow::Choice::Home, jobs)?,
            KeyCode::F(5) => {
                state.view_mut().details_open = true;
                state.focus = Focus::Details;
            }
            _ => {}
        }
        return Ok(());
    }
    if state.context.global_conversation
        && state.dashboard
        && key == KeyCode::Esc
        && state.focus == Focus::Composer
        && !state
            .view()
            .is_some_and(|view| view.details_open || view.show_delivery)
    {
        state.dashboard = false;
        state.diagnostics = false;
        return Ok(());
    }
    if state.diagnostics && state.focus != Focus::Composer {
        if key == KeyCode::Esc {
            state.diagnostics = false;
            state.focus = Focus::Composer;
            state.cards = None;
            state.view_mut().details_open = false;
            return Ok(());
        }
        if !matches!(key, KeyCode::F(2) | KeyCode::PageUp | KeyCode::PageDown)
            && !(key == KeyCode::End && modifiers.contains(KeyModifiers::CONTROL))
        {
            return Ok(());
        }
    }
    if !state.diagnostics {
        match key {
            KeyCode::F(5) => {
                let open = !state.view_mut().details_open;
                state.view_mut().details_open = open;
                state.focus = if open {
                    Focus::Details
                } else {
                    Focus::Composer
                };
                state.cards = None;
                return Ok(());
            }
            KeyCode::F(6) => {
                if state.context.global_conversation && !state.dashboard {
                    let mut actions = state.visible_actions();
                    if actions.items.len() == 1
                        && matches!(actions.items[0].1, workflow::Choice::HumanProposal(_))
                    {
                        let (_, choice) = actions.items.remove(0);
                        return state.activate(choice, jobs);
                    }
                }
                if !state.dashboard && state.visible_actions().items.is_empty() {
                    return Ok(());
                }
                if state.focus == Focus::Actions {
                    state.focus = Focus::Composer;
                    state.cards = None;
                } else {
                    state.view_mut().details_open = false;
                    state.cards = Some(state.visible_actions());
                    state.focus = Focus::Actions;
                }
                if let Some(selected) = state.cards.as_ref().map(|menu| menu.selected) {
                    state.view_mut().action_index = selected;
                }
                return Ok(());
            }
            KeyCode::F(2) => {
                state.focus = Focus::Composer;
                state.cards = None;
            }
            KeyCode::Esc if state.focus != Focus::Composer || state.view_mut().details_open => {
                state.focus = Focus::Composer;
                state.cards = None;
                state.view_mut().details_open = false;
                return Ok(());
            }
            KeyCode::Esc if state.view().is_some_and(|view| view.show_delivery) => {
                state.view_mut().show_delivery = false;
                return Ok(());
            }
            _ => {}
        }
        if state.focus == Focus::Details {
            match key {
                KeyCode::PageUp | KeyCode::Up => {
                    state.view_mut().details_scroll = state
                        .view_mut()
                        .details_scroll
                        .saturating_sub(if key == KeyCode::Up { 1 } else { 10 });
                }
                KeyCode::PageDown | KeyCode::Down => {
                    state.view_mut().details_scroll = state
                        .view_mut()
                        .details_scroll
                        .saturating_add(if key == KeyCode::Down { 1 } else { 10 });
                }
                _ => {}
            }
            return Ok(());
        }
        if state.focus == Focus::Actions {
            if let Some(menu) = state.cards.as_mut() {
                match key {
                    KeyCode::Up => menu.selected = menu.selected.saturating_sub(1),
                    KeyCode::Down => {
                        menu.selected = (menu.selected + 1).min(menu.items.len().saturating_sub(1))
                    }
                    KeyCode::Enter if !state.busy => {
                        if let Some((_, choice)) = menu.items.get(menu.selected) {
                            let choice = choice.clone();
                            state.activate(choice, jobs)?;
                        }
                    }
                    _ => {}
                }
            }
            if let Some(selected) = state.cards.as_ref().map(|menu| menu.selected) {
                state.view_mut().action_index = selected;
            }
            return Ok(());
        }
    }
    let draft = state
        .editor_view()
        .map(|view| view.draft.as_str())
        .unwrap_or("");
    let suggestions = if draft.trim_start().starts_with('/') {
        commands::complete_with_context(draft, &state.context)
    } else {
        vec![]
    };
    match key {
        KeyCode::Char(c) if !modifiers.contains(KeyModifiers::CONTROL) => {
            state.editor_view_mut().insert(c.encode_utf8(&mut [0; 4]));
            state.completion = 0;
        }
        KeyCode::Up if modifiers.is_empty() && !suggestions.is_empty() => {
            state.completion = state.completion.saturating_sub(1);
        }
        KeyCode::Down if modifiers.is_empty() && !suggestions.is_empty() => {
            state.completion = (state.completion + 1).min(suggestions.len() - 1);
        }
        KeyCode::Tab if !suggestions.is_empty() => {
            let draft = format!(
                "{} ",
                suggestions[state.completion.min(suggestions.len() - 1)]
            );
            state.editor_view_mut().replace_draft(draft);
        }
        KeyCode::PageUp => {
            let diagnostics = state.diagnostics;
            let view = state.reading_view_mut();
            if view.show_delivery && !diagnostics {
                view.delivery_scroll = view.delivery_scroll.saturating_sub(10);
            } else {
                view.follow = false;
                view.scroll = view.scroll.saturating_sub(10);
            }
        }
        KeyCode::PageDown => {
            let diagnostics = state.diagnostics;
            let view = state.reading_view_mut();
            if view.show_delivery && !diagnostics {
                view.delivery_scroll = view.delivery_scroll.saturating_add(10);
            } else {
                view.follow = false;
                view.scroll = view.scroll.saturating_add(10);
            }
        }
        KeyCode::End if modifiers.contains(KeyModifiers::CONTROL) => {
            if state.view().is_some_and(|view| view.show_delivery) && !state.diagnostics {
                state.view_mut().delivery_scroll = u16::MAX;
            } else {
                state.reading_view_mut().follow = true;
            }
        }
        KeyCode::F(2) if !state.works.is_empty() => {
            let current = state
                .works
                .iter()
                .position(|view| view["work"]["id"].as_str() == state.context.work_id.as_deref());
            let next = current
                .map(|index| (index + 1) % state.works.len())
                .unwrap_or(0);
            if let Some(id) = state.works[next]["work"]["id"].as_str() {
                queue(state, jobs, JobKind::SelectWork(id.into()))?;
            }
        }
        KeyCode::Esc => {
            state.completion = 0;
            state.editor_view_mut().editor.anchor = None;
        }
        KeyCode::Enter if modifiers.contains(KeyModifiers::SHIFT) => {
            state.editor_view_mut().insert("\n");
            state.completion = 0;
        }
        KeyCode::Enter if !state.busy => submit(state, jobs)?,
        _ => {
            let view = state.editor_view_mut();
            if view.editor.key(&mut view.draft, key, modifiers) {
                state.completion = 0;
            }
        }
    }
    Ok(())
}

fn queue(state: &mut State, jobs: &mpsc::UnboundedSender<Job>, kind: JobKind) -> Result<()> {
    let work = state.context.work_id.clone();
    let input = state
        .editor_view()
        .map(|view| view.draft.clone())
        .unwrap_or_default();
    queue_captured_with_origin(state, jobs, kind, work, input, MutationOrigin::Editor)
}

fn queue_captured(
    state: &mut State,
    jobs: &mpsc::UnboundedSender<Job>,
    kind: JobKind,
    work: Option<String>,
    input: String,
) -> Result<()> {
    queue_captured_with_origin(state, jobs, kind, work, input, MutationOrigin::Action)
}

fn queue_captured_with_origin(
    state: &mut State,
    jobs: &mpsc::UnboundedSender<Job>,
    kind: JobKind,
    work: Option<String>,
    input: String,
    origin: MutationOrigin,
) -> Result<()> {
    anyhow::ensure!(
        state.demo.is_none(),
        "Scripted demo cannot queue production service or provider jobs"
    );
    let (kind, work) = match kind {
        JobKind::SelectWork(id) => {
            uuid::Uuid::parse_str(&id).context("Invalid task ID")?;
            let mut operation = Operation::read("work.open", json!({"workId":id}));
            operation.mutation = true;
            (JobKind::Send(operation), Some(id))
        }
        other => (other, work),
    };
    if state.stale {
        anyhow::ensure!(
            !matches!(&kind, JobKind::Send(operation) if operation.mutation)
                && !matches!(
                    &kind,
                    JobKind::PrepareStart(_)
                        | JobKind::PrepareApply(_)
                        | JobKind::PrepareTransfer(_)
                        | JobKind::PrepareProposal(_)
                ),
            "{}",
            t!("agent_center.console_stale")
        );
    }
    let kind = match kind {
        JobKind::Send(operation) if operation.method == "inbox.list" => {
            JobKind::RefreshInbox(InboxRefresh {
                operation,
                versions: state.event_versions.clone(),
            })
        }
        other => other,
    };
    let conversation = if let JobKind::Send(operation) = &kind {
        operation.params["conversationId"]
            .as_str()
            .map(str::to_owned)
    } else {
        None
    };
    if let Some(id) = &conversation {
        let target = if let JobKind::Send(operation) = &kind {
            operation
                .params
                .pointer("/context/selectedWorkId")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| work.clone())
        } else {
            work.clone()
        };
        state.conversation_targets.insert(
            id.clone(),
            if state.context.global_conversation {
                None
            } else {
                target
            },
        );
    }
    let mutation = if let JobKind::Send(operation) = &kind {
        if operation.mutation {
            let mut selection = state.selection();
            selection.work = work.clone();
            if operation.method == "conversation.submit" {
                selection.project = operation
                    .params
                    .pointer("/context/projectId")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                selection.conversation = operation.params["conversationId"]
                    .as_str()
                    .unwrap_or("")
                    .into();
                selection.console = operation
                    .params
                    .pointer("/context/consoleSessionId")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .into();
                selection.version = operation
                    .params
                    .pointer("/context/contextVersion")
                    .and_then(Value::as_u64)
                    .unwrap_or(1);
            } else if let Some(project) = state
                .works
                .iter()
                .find(|view| view["work"]["id"].as_str() == work.as_deref())
                .and_then(|view| view["work"]["projectId"].as_str())
            {
                selection.project = Some(project.into());
            }
            if let Some(previous) = state.mutations.get(&operation.command_id) {
                anyhow::ensure!(
                    operation_preview(&previous.operation) == operation_preview(operation)
                        && previous.selection.work == work,
                    "{}",
                    t!("agent_center.invalid_request")
                );
            }
            let captured = state
                .pending
                .as_ref()
                .filter(|pending| pending.operation.command_id == operation.command_id);
            Some(PendingMutation {
                operation: operation.clone(),
                input: input.clone(),
                selection,
                preview: captured
                    .map(|pending| pending.preview.clone())
                    .unwrap_or_else(|| json!({"text":operation.params["text"]})),
                target_label: captured
                    .map(|pending| pending.target_label.clone())
                    .unwrap_or_else(|| {
                        if state.context.global_conversation
                            && operation.method == "conversation.submit"
                        {
                            let hint = operation
                                .params
                                .pointer("/context/selectedWorkId")
                                .and_then(Value::as_str)
                                .map(|work| state.work_label(Some(work)))
                                .or_else(|| {
                                    operation
                                        .params
                                        .pointer("/context/projectId")
                                        .and_then(Value::as_str)
                                        .map(|project| state.project_label(Some(project)))
                                });
                            return hint
                                .map(|hint| format!("{} · {hint}", t!("agent_center.chat_global")))
                                .unwrap_or_else(|| t!("agent_center.chat_global").into_owned());
                        }
                        format!(
                            "{} · {}",
                            state.work_label(work.as_deref()),
                            state.project_label(state.context.project_id.as_deref())
                        )
                    }),
                unknown: None,
                origin: captured.map(|pending| pending.origin).unwrap_or(origin),
                requires_confirmation: captured.is_some() || operation.confirmation,
            })
        } else {
            None
        }
    } else {
        None
    };
    let effective_origin = mutation
        .as_ref()
        .map(|mutation| mutation.origin)
        .unwrap_or(origin);
    let retry = if effective_origin == MutationOrigin::Editor
        && (matches!(&kind, JobKind::Resolve { .. } | JobKind::PrepareTransfer(_))
            || matches!(&kind, JobKind::Send(operation) if operation.mutation))
    {
        Some((input.clone(), work.clone(), kind.clone()))
    } else {
        None
    };
    jobs.send(Job {
        kind,
        conversation,
        work,
        input,
        inbox_versions: state.event_versions.clone(),
        origin,
    })
    .with_context(|| t!("agent_center.worker_stopped").into_owned())?;
    if let Some(retry) = retry {
        state.retry = Some(retry);
    }
    if let Some(mutation) = mutation {
        state
            .mutations
            .entry(mutation.operation.command_id.clone())
            .or_insert(mutation);
    }
    state.busy = true;
    state.queued_jobs += 1;
    state.notice = t!("agent_center.sending").into_owned();
    state.notice_kind = NoticeKind::Info;
    Ok(())
}

fn submit(state: &mut State, jobs: &mpsc::UnboundedSender<Job>) -> Result<()> {
    state.sync_view_context();
    let input = state
        .editor_view()
        .map(|view| view.draft.clone())
        .unwrap_or_default();
    let unresolved: Vec<_> = state
        .mutations
        .values()
        .filter(|mutation| {
            mutation.unknown.is_some()
                && mutation.origin == MutationOrigin::Editor
                && !input.trim().is_empty()
                && (state.context.global_conversation
                    || mutation.selection.work == state.context.work_id)
                && mutation.input == input
        })
        .collect();
    if !unresolved.is_empty() {
        let exact: Vec<_> = unresolved
            .iter()
            .filter(|mutation| {
                mutation.selection == state.selection()
                    || (state.context.global_conversation
                        && mutation.operation.method == "conversation.submit"
                        && mutation.selection.conversation == state.context.conversation_id
                        && mutation.selection.console == state.context.console_session_id)
            })
            .collect();
        anyhow::ensure!(exact.len() == 1, "{}", t!("agent_center.console_unknown"));
        let command_id = exact[0].operation.command_id.clone();
        return state.activate(workflow::Choice::Reconcile(command_id), jobs);
    }
    if let Some((draft, work, kind)) = state.retry.clone() {
        if !input.trim().is_empty()
            && draft == input
            && (state.context.global_conversation || work == state.context.work_id)
        {
            if let JobKind::Send(operation) = &kind {
                if state
                    .mutations
                    .get(&operation.command_id)
                    .is_some_and(|mutation| mutation.requires_confirmation)
                {
                    return state.activate(
                        workflow::Choice::Reconcile(operation.command_id.clone()),
                        jobs,
                    );
                }
                if operation.confirmation {
                    state.pending = Some(PendingConfirmation {
                        operation: operation.clone(),
                        preview: operation_preview(operation),
                        work: work.clone(),
                        input,
                        scroll: 0,
                        target_label: state.work_label(work.as_deref()),
                        origin: MutationOrigin::Editor,
                    });
                    state.notice = t!("agent_center.confirm_prompt").into_owned();
                    state.notice_kind = NoticeKind::Attention;
                    return Ok(());
                }
            }
            return queue_captured_with_origin(
                state,
                jobs,
                kind,
                work,
                input,
                MutationOrigin::Editor,
            );
        }
    }
    let action = match commands::parse_input(&input)? {
        Input::Text(_) if state.can_claim_executor() => {
            let operation = state.continue_operation(false)?;
            state.activate(workflow::Choice::Control(operation), jobs)?;
            return Ok(());
        }
        Input::Text(text)
            if state.context.work_id.is_some()
                && (text.trim().eq_ignore_ascii_case("continue work")
                    || text.trim().eq_ignore_ascii_case("continue task")
                    || text.trim() == "继续任务"
                    || text.trim() == t!("agent_center.task_continue").as_ref()) =>
        {
            Action::Operation(state.continue_operation(false)?)
        }
        Input::Text(text) => {
            Action::Operation(commands::conversation(text, &state.context, false)?)
        }
        Input::Command(args) => {
            let external_input = args
                .iter()
                .take_while(|arg| arg.as_str() != "--")
                .any(|arg| arg == "--input-json");
            if external_input && !args.last().is_some_and(|arg| arg == "--help") {
                let context = state.context.clone();
                return queue(
                    state,
                    jobs,
                    JobKind::Resolve {
                        args,
                        context,
                        fallback_command_id: uuid::Uuid::new_v4().to_string(),
                    },
                );
            }
            commands::compile(&args, &state.context, true)?
        }
    };
    match action {
        Action::Home => {
            if state.context.global_conversation {
                state.activate(workflow::Choice::Home, jobs)?;
            } else {
                state.select(None);
            }
        }
        Action::Help(prefix) => {
            let help = commands::help(&prefix);
            state.append(state.context.work_id.clone(), &help);
            let names = help["commands"]
                .as_array()
                .map(|commands| {
                    commands
                        .iter()
                        .filter_map(|command| command["command"].as_str())
                        .map(|command| format!("/{command}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            state.editor_view_mut().records.insert(
                "help".into(),
                json!({
                    "text":format!("{}\n\n{names}", t!("agent_center.console_menu_help"))
                }),
            );
            state.editor_view_mut().clear_draft();
        }
        Action::SelectWork(work) => {
            queue(state, jobs, JobKind::SelectWork(work))?;
        }
        Action::SelectProject(project) => {
            queue(state, jobs, JobKind::SelectProject(project))?;
        }
        Action::PrepareStart(work) => queue(state, jobs, JobKind::PrepareStart(work))?,
        Action::PrepareApply(target) => queue(state, jobs, JobKind::PrepareApply(target))?,
        Action::PrepareTransfer(target) => queue(state, jobs, JobKind::PrepareTransfer(target))?,
        Action::PrepareProposal(intake) => queue(state, jobs, JobKind::PrepareProposal(intake))?,
        Action::Unsupported(command) => {
            let (code, message) = commands::unsupported_failure(&command);
            bail!("{code}: {message}")
        }
        Action::Operation(operation) => {
            if operation.confirmation {
                state.pending = Some(PendingConfirmation {
                    preview: operation_preview(&operation),
                    operation,
                    work: state.context.work_id.clone(),
                    input,
                    scroll: 0,
                    target_label: state.work_label(state.context.work_id.as_deref()),
                    origin: MutationOrigin::Editor,
                });
                state.notice = t!("agent_center.confirm_prompt").into_owned();
                state.notice_kind = NoticeKind::Attention;
            } else {
                queue(state, jobs, JobKind::Send(operation))?;
            }
        }
    }
    Ok(())
}

fn render(frame: &mut ratatui::Frame<'_>, state: &mut State) {
    renderer::render(frame, state);
}

#[cfg(test)]
mod global_tests {
    use super::*;
    use ratatui::backend::{Backend, TestBackend};

    const A: &str = "11111111-1111-4111-8111-111111111111";
    const B: &str = "22222222-2222-4222-8222-222222222222";
    const PA: &str = "33333333-3333-4333-8333-333333333333";
    const PB: &str = "44444444-4444-4444-8444-444444444444";

    fn state() -> State {
        let mut state = State::new();
        state.task_list = false;
        state.projects = vec![
            json!({"kind":"Project","id":PA,"name":"Harbor","root":"C:\\harbor"}),
            json!({"kind":"Project","id":PB,"name":"Orchard","root":"C:\\orchard"}),
        ];
        state.works = vec![
            json!({"work":{"kind":"Work","id":A,"version":3,"projectId":PA,"lifecycle":"Active","desiredAdvancement":"Advance"},
                "spec":{"goal":"Harbor report","scope":["Actual scope"],"delivery":{"kind":"Report"}}}),
            json!({"work":{"kind":"Work","id":B,"version":4,"projectId":PB,"lifecycle":"Draft"},
                "spec":{"goal":"Orchard notes","scope":["Other actual scope"]}}),
        ];
        for work in &state.works {
            capture_versions(&mut state.context, &json!({"data":work}));
        }
        state
    }
    fn pair(state: &State) -> (String, String) {
        (
            state.context.conversation_id.clone(),
            state.context.console_session_id.clone(),
        )
    }
    fn draw(state: &mut State, width: u16, height: u16) -> (String, Terminal<TestBackend>) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render(frame, state)).unwrap();
        let mut text = String::new();
        for row in terminal
            .backend()
            .buffer()
            .content
            .chunks(width.max(1) as usize)
        {
            let mut column = 0;
            while column < row.len() {
                let symbol = row[column].symbol();
                text.push_str(symbol);
                column += unicode_width::UnicodeWidthStr::width(symbol).max(1);
            }
            text.push('\n');
        }
        (text, terminal)
    }
    fn refresh(state: &mut State) {
        state.receive(Update {
            work: None,
            input: String::new(),
            conversation: None,
            result: Ok(Outcome::Refreshed {
                works: json!({"status":"ok","data":{"items":state.works}}),
                projects: json!({"status":"ok","data":{"items":state.projects}}),
                decisions: json!({"status":"ok","data":{"items":[]}}),
                inbox: json!({"status":"ok","data":{"items":[]}}),
                inbox_versions: state.event_versions.clone(),
            }),
        });
    }

    #[test]
    fn thinking_tracks_submission_response_and_terminal_versions_not_work_activity() {
        let _locale = crate::test_support::lock_locale();
        let mut state = state();
        state.busy = true;
        assert!(!state.thinking());
        state.busy = false;
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        handle_paste(&mut state, "What is the status of my work?");
        submit(&mut state, &jobs).unwrap();
        let JobKind::Send(operation) = receiver.try_recv().unwrap().kind else {
            panic!("expected conversation submission");
        };
        assert!(state.thinking());
        let conversation = state.context.conversation_id.clone();
        let streaming = json!({"kind":"ConversationItem","id":"reply","version":1,
            "conversationId":conversation,"role":"assistant","status":"Streaming","parts":[]});
        state.append(None, &streaming);
        state.settle_mutation(&operation.command_id, &Ok(json!({"status":"ok"})));
        assert!(state.thinking());
        state.chat.replace_draft("Keep my next question".into());
        let mut complete = streaming.clone();
        complete["version"] = json!(2);
        complete["status"] = json!("Complete");
        state.append(None, &complete);
        assert!(!state.thinking());
        state.append(
            None,
            &json!({"kind":"Conversation","id":conversation,"messages":[streaming]}),
        );
        assert!(!state.thinking(), "A stale snapshot cannot revive activity");
        assert_eq!(state.chat.draft, "Keep my next question");
        state.append(
            None,
            &json!({"kind":"ConversationItem","id":"other-reply","version":1,
            "conversationId":"another-window","role":"assistant","status":"Streaming"}),
        );
        assert!(!state.thinking());
    }

    #[test]
    fn thinking_does_not_guess_unknown_sends_and_hides_stale_response_state() {
        let _locale = crate::test_support::lock_locale();
        for result in [
            Err(anyhow::anyhow!("connection closed")),
            Ok(json!({"status":"error","failure":{"code":"OUTCOME_UNKNOWN"}})),
            Ok(json!({"status":"error","failure":{"code":"ASSISTANT_UNAVAILABLE"}})),
        ] {
            let mut state = State::new();
            state.task_list = false;
            let (jobs, mut receiver) = mpsc::unbounded_channel();
            handle_paste(&mut state, "Status please");
            submit(&mut state, &jobs).unwrap();
            let JobKind::Send(operation) = receiver.try_recv().unwrap().kind else {
                panic!("expected conversation submission");
            };
            assert!(state.thinking());
            state.settle_mutation(&operation.command_id, &result);
            assert!(!state.thinking());
        }
        let mut state = State::new();
        state.task_list = false;
        let mut message = json!({"kind":"ConversationItem","id":"reply","version":1,
            "conversationId":state.context.conversation_id,"role":"assistant","status":"Streaming"});
        state.append(None, &message);
        assert!(state.thinking());
        state.event(json!({"type":"stream_error","failure":{"message":"Disconnected"}}));
        assert!(!state.thinking());
        message["status"] = json!("Interrupted");
        message["version"] = json!(2);
        state.append(None, &message);
        refresh(&mut state);
        assert!(!state.stale);
        assert!(!state.thinking());
    }

    #[test]
    fn ordinary_language_without_project_emits_only_explicit_global_conversation_submission() {
        let _locale = crate::test_support::lock_locale();
        let mut state = State::new();
        state.task_list = false;
        let original = pair(&state);
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        handle_paste(
            &mut state,
            "Find the status of both reports and arrange the next steps",
        );
        submit(&mut state, &jobs).unwrap();
        let JobKind::Send(operation) = receiver.try_recv().unwrap().kind else {
            panic!("expected conversation request");
        };
        assert_eq!(operation.method, "conversation.submit");
        assert_eq!(operation.params["context"]["scope"], "Global");
        assert!(operation.params["context"].get("projectId").is_none());
        assert!(operation.params["context"].get("selectedWorkId").is_none());
        assert_eq!(operation.params["conversationId"], original.0);
        assert_eq!(operation.params["context"]["consoleSessionId"], original.1);
        assert_eq!(pair(&state), original);
        assert!(
            !operation.confirmation,
            "language interpretation belongs to the authority, not a fake UI parser"
        );
    }

    #[test]
    fn global_composer_keeps_its_caret_in_compact_and_tiny_viewports() {
        let _locale = crate::test_support::lock_locale();
        let mut state = State::new();
        state.task_list = false;
        for draft in [String::new(), "中e\u{301}👩‍💻 actual draft\n".repeat(8)] {
            state.chat.replace_draft(draft.clone());
            for (width, height) in [(1, 1), (8, 4), (80, 24), (160, 45)] {
                let (_, mut terminal) = draw(&mut state, width, height);
                let input =
                    renderer::regions(&mut state, ratatui::layout::Rect::new(0, 0, width, height))
                        .editor;
                if !input.is_empty() {
                    assert!(input.contains(terminal.backend_mut().get_cursor_position().unwrap()));
                }
                assert_eq!(state.chat.draft, draft);
            }
        }
    }

    #[test]
    fn navigation_keeps_one_global_pair_editor_selection_history_and_read_position() {
        let _locale = crate::test_support::lock_locale();
        let mut state = state();
        let original = pair(&state);
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        handle_paste(&mut state, "中e\u{301}👩‍💻 shared draft");
        handle_key(&mut state, KeyCode::Left, KeyModifiers::SHIFT, &jobs).unwrap();
        let selection = state.chat.editor.selection();
        state.chat.scroll = 3;
        state.chat.follow = false;
        state.chat.messages = (0..30).map(|n| json!({"id":format!("message-{n}"),"role":"human","text":format!("Recorded global message {n}")})).collect();
        state.select(Some(A.into()));
        let selected_a = state.selection();
        state.select(Some(B.into()));
        state.activate(workflow::Choice::Overview, &jobs).unwrap();
        assert!(state.task_list);
        handle_key(&mut state, KeyCode::PageDown, KeyModifiers::NONE, &jobs).unwrap();
        draw(&mut state, 160, 45);
        assert_eq!(state.chat.scroll, 3);
        handle_key(&mut state, KeyCode::Esc, KeyModifiers::NONE, &jobs).unwrap();
        assert!(!state.dashboard);
        state.activate(workflow::Choice::Home, &jobs).unwrap();
        state.restore_selection(selected_a);
        handle_key(&mut state, KeyCode::F(7), KeyModifiers::NONE, &jobs).unwrap();
        handle_key(&mut state, KeyCode::F(7), KeyModifiers::NONE, &jobs).unwrap();
        for width in [8, 80, 160] {
            let (_, mut terminal) = draw(&mut state, width, 45);
            let input =
                renderer::regions(&mut state, ratatui::layout::Rect::new(0, 0, width, 45)).editor;
            assert!(input.contains(terminal.backend_mut().get_cursor_position().unwrap()));
        }
        assert_eq!(state.chat.draft, "中e\u{301}👩‍💻 shared draft");
        assert_eq!(state.chat.editor.selection(), selection);
        assert_eq!(state.chat.messages.len(), 30);
        assert_eq!(state.chat.scroll, 3);
        assert_eq!(pair(&state), original);
        assert!(state.views.values().all(|view| view.draft.is_empty()));
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn new_task_chat_only_contains_correlated_messages() {
        let _locale = crate::test_support::lock_locale();
        let mut state = state();
        let conversation = state.context.conversation_id.clone();
        for _ in 0..20 {
            state.append(
                Some(A.into()),
                &json!({"kind":"Task","id":"task","version":1,"text":"LOW_LEVEL_POLL_NOISE"}),
            );
        }
        state.append(Some(B.into()), &json!({"kind":"Conversation","id":"foreign-conversation",
            "messages":[{"id":"foreign","role":"assistant","conversationId":"foreign-conversation","text":"FOREIGN_CHAT"}]}));
        state.append(None, &json!({"kind":"Conversation","id":conversation,
            "messages":[{"id":"global-human","role":"human","text":"Arrange both reports"},
                {"id":"global-assistant","role":"assistant","parts":[{"text":"Recorded answer"}]}]}));
        state.event(
            json!({"type":"event","eventId":"delta-global","workId":A,"changes":[{
            "subject":{"kind":"ConversationItem","id":"global-assistant","version":2},
            "view":{"conversationId":conversation,"messageId":"global-assistant","partId":"text",
                "chunkIndex":1,"text":" with actual detail"}}]}),
        );
        for width in [80, 160] {
            let (screen, _) = draw(&mut state, width, 45);
            assert!(screen.contains("Global conversation"));
            assert!(
                screen.contains("Arrange both reports")
                    && screen.contains("Recorded answer with actual detail")
            );
            for absent in [
                "LOW_LEVEL_POLL_NOISE",
                "FOREIGN_CHAT",
                "Next responsibility",
                "Task progress",
                "F6 · Work actions",
                "Needs attention:",
                "C:\\harbor",
                A,
                PA,
            ] {
                assert!(
                    !screen.contains(absent),
                    "unexpected default chat surface: {absent}"
                );
            }
        }
        assert_eq!(state.chat.messages.len(), 2);
        state.append(
            None,
            &json!({"kind":"ConversationItem","conversationId":conversation,
            "text":"Malformed unidentifiable message"}),
        );
        assert_eq!(state.chat.messages.len(), 2);
        assert_eq!(
            state.chat.messages[1]["text"],
            "Recorded answer with actual detail"
        );
        state.append(None, &json!({"kind":"Conversation","id":conversation,
            "messages":[{"id":"global-assistant","role":"assistant","parts":[{"text":"Recorded answer with actual detail"}]}]}));
        assert_eq!(
            draw(&mut state, 160, 45)
                .0
                .matches("Recorded answer with actual detail")
                .count(),
            1
        );
    }

    #[test]
    fn global_unknown_outcome_retries_the_original_envelope_after_hint_change_and_refresh() {
        let _locale = crate::test_support::lock_locale();
        let mut state = state();
        let original_pair = pair(&state);
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        state.select(Some(A.into()));
        handle_paste(&mut state, "Arrange the report review");
        submit(&mut state, &jobs).unwrap();
        let sent = receiver.try_recv().unwrap();
        let JobKind::Send(operation) = sent.kind else {
            panic!("expected submission");
        };
        state.receive(Update {
            work: sent.work,
            input: sent.input,
            conversation: sent.conversation,
            result: Ok(Outcome::Mutation {
                command_id: operation.command_id.clone(),
                result: Err(anyhow::anyhow!("receipt lost")),
            }),
        });
        state.select(Some(B.into()));
        refresh(&mut state);
        assert!(state.unresolved_notice().is_some());
        assert_eq!(pair(&state), original_pair);
        submit(&mut state, &jobs).unwrap();
        let retry = receiver.try_recv().unwrap();
        let JobKind::Send(retried) = retry.kind else {
            panic!("expected explicit retry");
        };
        assert_eq!(operation_preview(&retried), operation_preview(&operation));
        assert_eq!(retried.params["context"]["projectId"], PA);
        assert_eq!(retry.work.as_deref(), Some(A));
        assert_eq!(state.context.work_id.as_deref(), Some(B));
    }

    #[test]
    fn old_global_receipt_clears_only_its_unchanged_editor_input_not_a_new_draft() {
        let _locale = crate::test_support::lock_locale();
        for edited in [false, true] {
            let mut state = state();
            let (jobs, mut receiver) = mpsc::unbounded_channel();
            state.select(Some(A.into()));
            handle_paste(&mut state, "Submitted global message");
            submit(&mut state, &jobs).unwrap();
            let job = receiver.try_recv().unwrap();
            let JobKind::Send(operation) = job.kind else {
                panic!("expected submission");
            };
            state.select(Some(B.into()));
            if edited {
                state.chat.replace_draft("New global draft".into());
            }
            state.receive(Update {
                work: job.work,
                input: job.input,
                conversation: job.conversation,
                result: Ok(Outcome::Mutation {
                    command_id: operation.command_id,
                    result: Ok(json!({"status":"ok"})),
                }),
            });
            assert_eq!(
                state.chat.draft,
                if edited { "New global draft" } else { "" }
            );
            assert_eq!(state.context.work_id.as_deref(), Some(B));
        }
    }

    #[test]
    fn global_menu_uncertainty_never_turns_editor_enter_into_a_cancel_retry() {
        let _locale = crate::test_support::lock_locale();
        let mut state = state();
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        state.select(Some(A.into()));
        handle_paste(&mut state, "Unrelated global question");
        let choice = state.actions().items.into_iter().find_map(|(_, choice)| {
            matches!(&choice, workflow::Choice::Control(operation) if operation.params["action"] == "Cancel").then_some(choice)
        }).unwrap();
        state.activate(choice, &jobs).unwrap();
        let cancel = state.pending.as_ref().unwrap().operation.clone();
        handle_key(&mut state, KeyCode::Enter, KeyModifiers::CONTROL, &jobs).unwrap();
        let job = receiver.try_recv().unwrap();
        state.receive(Update {
            work: job.work,
            input: job.input,
            conversation: job.conversation,
            result: Ok(Outcome::Mutation {
                command_id: cancel.command_id.clone(),
                result: Err(anyhow::anyhow!("unknown cancellation receipt")),
            }),
        });
        handle_key(&mut state, KeyCode::Esc, KeyModifiers::NONE, &jobs).unwrap();
        handle_key(&mut state, KeyCode::Enter, KeyModifiers::NONE, &jobs).unwrap();
        let next = receiver.try_recv().unwrap();
        let JobKind::Send(message) = next.kind else {
            panic!("expected global language submission");
        };
        assert_eq!(message.method, "conversation.submit");
        assert_ne!(message.command_id, cancel.command_id);
        state.receive(Update {
            work: next.work,
            input: next.input,
            conversation: next.conversation,
            result: Ok(Outcome::Mutation {
                command_id: message.command_id,
                result: Ok(json!({"status":"ok"})),
            }),
        });
        state
            .activate(
                workflow::Choice::Reconcile(cancel.command_id.clone()),
                &jobs,
            )
            .unwrap();
        handle_key(&mut state, KeyCode::Enter, KeyModifiers::NONE, &jobs).unwrap();
        assert!(receiver.try_recv().is_err());
        handle_key(&mut state, KeyCode::Enter, KeyModifiers::CONTROL, &jobs).unwrap();
        let JobKind::Send(retry) = receiver.try_recv().unwrap().kind else {
            panic!("expected confirmed reconciliation");
        };
        assert_eq!(operation_preview(&retry), operation_preview(&cancel));
    }

    #[test]
    fn background_question_and_fixed_inspection_leave_the_global_editor_untouched() {
        let _locale = crate::test_support::lock_locale();
        let mut state = state();
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        state.select(Some(B.into()));
        handle_paste(&mut state, "Global draft");
        handle_key(&mut state, KeyCode::Left, KeyModifiers::SHIFT, &jobs).unwrap();
        let selection = state.chat.editor.selection();
        let question = json!({"kind":"DecisionRequest","id":"actual-question","workId":A,"version":2,
            "status":"Open","question":"Approve the actual scope?","responseSchema":{"type":"boolean"}});
        state.append(Some(A.into()), &question);
        let (screen, _) = draw(&mut state, 160, 45);
        assert!(screen.contains("Approve the actual scope?") && screen.contains("Harbor report"));
        assert!(!screen.contains("Needs attention:"));
        state
            .activate(workflow::Choice::Question(question, Some(A.into())), &jobs)
            .unwrap();
        assert_eq!(state.form.as_ref().unwrap().fields[0].choice, None);
        handle_key(&mut state, KeyCode::Right, KeyModifiers::NONE, &jobs).unwrap();
        handle_key(&mut state, KeyCode::Enter, KeyModifiers::CONTROL, &jobs).unwrap();
        let answer = &state.pending.as_ref().unwrap().operation;
        assert_eq!(
            answer.if_match,
            vec![json!({"kind":"DecisionRequest","id":"actual-question","version":2})]
        );
        assert_eq!(state.chat.editor.selection(), selection);
        assert_eq!(state.chat.draft, "Global draft");
        handle_key(&mut state, KeyCode::Esc, KeyModifiers::NONE, &jobs).unwrap();
        handle_key(&mut state, KeyCode::Esc, KeyModifiers::NONE, &jobs).unwrap();
        state.select(Some(A.into()));
        let inspection = json!({"work":state.works[0]["work"],"candidate":{"id":"candidate","version":1,"status":"Proposed"},
            "result":{"body":{"summary":"Actual fixed report contents"}}});
        state.receive(Update {
            work: Some(A.into()),
            input: String::new(),
            conversation: None,
            result: Ok(Outcome::Inspected(inspection.clone())),
        });
        assert!(draw(&mut state, 160, 45)
            .0
            .contains("Actual fixed report contents"));
        handle_key(&mut state, KeyCode::Esc, KeyModifiers::NONE, &jobs).unwrap();
        assert_eq!(state.view().unwrap().inspection.as_ref(), Some(&inspection));
        assert_eq!(state.chat.draft, "Global draft");
        assert_eq!(state.chat.editor.selection(), selection);
        assert!(receiver.try_recv().is_err());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_navigation_restores_the_console_and_conversation_pair() {
        let _locale = crate::test_support::lock_locale();
        let mut state = State::legacy();
        state.context.project_id = Some("project-a".into());
        state.select(None);
        let home_a = state.selection();
        state.works = vec![
            json!({"work":{"id":"a","projectId":"project-a"}}),
            json!({"work":{"id":"b","projectId":"project-b"}}),
        ];
        state.select(Some("a".into()));
        let work_a = state.selection();
        state.view_mut().draft = "Keep A's draft".into();
        state.select(Some("b".into()));
        let work_b = state.selection();
        assert_ne!(home_a.console, work_a.console);
        assert_ne!(work_a.console, work_b.console);
        assert_ne!(home_a.conversation, work_a.conversation);
        state.restore_selection(work_a.clone());
        assert_eq!(state.selection(), work_a);
        assert_eq!(state.view().unwrap().draft, "Keep A's draft");
        state.restore_selection(home_a.clone());
        assert_eq!(state.selection(), home_a);
        state.restore_selection(work_b.clone());
        assert_eq!(state.selection(), work_b);
    }

    #[test]
    fn choosing_another_project_leaves_the_old_work_and_binding_intact() {
        let _locale = crate::test_support::lock_locale();
        let mut state = State::legacy();
        state.works = vec![json!({"work":{"id":"a","projectId":"project-a"}})];
        state.select(Some("a".into()));
        let original = state.selection();
        state.view_mut().draft = "Keep A's draft".into();
        state.receive(Update {
            work: Some("a".into()),
            conversation: None,
            input: String::new(),
            result: Ok(Outcome::SelectedProject {
                id: "project-b".into(),
                response: json!({"status":"ok","data":{}}),
            }),
        });
        assert_eq!(state.context.work_id, None);
        assert_eq!(state.context.project_id.as_deref(), Some("project-b"));
        assert_ne!(state.context.console_session_id, original.console);
        assert_eq!(state.return_work.as_ref(), Some(&original));
        state.restore_selection(original.clone());
        assert_eq!(state.selection(), original);
        assert_eq!(state.view().unwrap().draft, "Keep A's draft");
    }

    #[test]
    fn captured_new_intake_restores_its_exact_binding_without_rewriting_the_request() {
        let _locale = crate::test_support::lock_locale();
        let mut state = State::legacy();
        state.context.project_id = Some("project-a".into());
        state.select(Some("a".into()));
        let arguments = vec!["work".into(), "new".into(), "Another goal".into()];
        let Action::Operation(operation) =
            commands::compile(&arguments, &state.context, true).unwrap()
        else {
            panic!("expected new intake");
        };
        let frozen = operation_preview(&operation);
        let command_id = operation.command_id.clone();
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        queue(&mut state, &jobs, JobKind::Send(operation)).unwrap();
        assert!(receiver.try_recv().is_ok());
        let captured = state.mutations[&command_id].selection.clone();
        assert_eq!(
            json!(captured.console),
            frozen["params"]["context"]["consoleSessionId"]
        );
        assert_ne!(captured.console, state.context.console_session_id);
        state.select(Some("b".into()));
        state.restore_selection(captured.clone());
        assert_eq!(state.selection(), captured);
        assert_eq!(
            operation_preview(&state.mutations[&command_id].operation),
            frozen
        );
        assert_eq!(
            state.conversation_consoles[&captured.conversation],
            captured.console
        );
    }

    #[test]
    fn f1_returns_to_existing_global_chat_without_losing_task_or_global_drafts() {
        let _locale = crate::test_support::lock_locale();
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        let mut state = State::new();
        let global = state.global_selection.clone();
        state.chat.replace_draft("Global draft".into());
        state
            .chat
            .messages
            .push(json!({"id":"global-history","role":"human","text":"Keep this conversation"}));
        state.context.global_conversation = false;
        state.works = vec![json!({"work":{"id":"task-a","lifecycle":"Active"}})];
        state.select(Some("task-a".into()));
        state.view_mut().replace_draft("Task draft".into());
        state.view_mut().details_open = true;
        state.focus = Focus::Details;
        state.task_list = false;

        for _ in 0..2 {
            handle_key(&mut state, KeyCode::F(1), KeyModifiers::NONE, &jobs).unwrap();
            assert!(!state.task_list);
            assert!(state.context.global_conversation);
            assert_eq!(state.context.work_id, None);
            assert_eq!(state.context.conversation_id, global.conversation);
            assert_eq!(state.context.console_session_id, global.console);
            assert_eq!(state.chat.draft, "Global draft");
            assert_eq!(state.chat.messages.len(), 1);
            assert_eq!(state.views[&Some("task-a".into())].draft, "Task draft");
            assert_eq!(state.focus, Focus::Composer);
            handle_key(&mut state, KeyCode::F(2), KeyModifiers::NONE, &jobs).unwrap();
            assert!(state.task_list);
        }
        assert!(
            receiver.try_recv().is_err(),
            "Navigation must not start or submit work"
        );
    }

    #[test]
    fn escape_from_initial_tasks_opens_the_empty_global_chat() {
        let _locale = crate::test_support::lock_locale();
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        let mut state = State::new();
        let conversation = state.context.conversation_id.clone();
        assert!(state.task_list);
        handle_key(&mut state, KeyCode::Esc, KeyModifiers::NONE, &jobs).unwrap();
        assert!(!state.task_list);
        assert!(state.context.global_conversation);
        assert_eq!(state.context.conversation_id, conversation);
        assert!(receiver.try_recv().is_err());
        assert_eq!(state.actions().items[0].0, t!("agent_center.chat_global"));
    }

    #[test]
    fn left_moves_the_insertion_point_in_active_and_cancelled_work_drafts() {
        let _locale = crate::test_support::lock_locale();
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        for (work, lifecycle, original, left, expected) in [
            ("b", "Cancelled", "round8-b-draft", 3, "round8-b-drxaft"),
            ("a", "Active", "draft", 2, "draxft"),
        ] {
            let mut state = State::legacy();
            state.works = vec![json!({"work":{"id":work,"lifecycle":lifecycle}})];
            state.select(Some(work.into()));
            for ch in original.chars() {
                handle_key(&mut state, KeyCode::Char(ch), KeyModifiers::NONE, &jobs).unwrap();
            }
            for _ in 0..left {
                handle_key(&mut state, KeyCode::Left, KeyModifiers::NONE, &jobs).unwrap();
            }
            handle_key(&mut state, KeyCode::Char('x'), KeyModifiers::NONE, &jobs).unwrap();
            assert_eq!(state.view().unwrap().draft, expected);
            assert!(draw_state(&mut state, 80, 30).contains(expected));
        }
        assert!(receiver.try_recv().is_err(), "editing must not submit work");
    }

    #[test]
    fn caret_and_selection_survive_work_switches_events_and_old_responses() {
        let _locale = crate::test_support::lock_locale();
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        let mut state = State::legacy();
        state.works = vec![json!({"work":{"id":"a"}}), json!({"work":{"id":"b"}})];
        state.select(Some("a".into()));
        handle_paste(&mut state, "abcdef");
        for _ in 0..2 {
            handle_key(&mut state, KeyCode::Left, KeyModifiers::SHIFT, &jobs).unwrap();
        }
        handle_key(&mut state, KeyCode::F(2), KeyModifiers::NONE, &jobs).unwrap();
        assert!(state.task_list);
        assert!(
            receiver.try_recv().is_err(),
            "Tasks navigation must not implicitly open or continue another task"
        );
        state.receive(Update {
            input: String::new(),
            work: Some("a".into()),
            conversation: None,
            result: Ok(Outcome::SelectedWork {
                id: "b".into(),
                response: json!({"status":"ok"}),
            }),
        });
        state.task_list = false;
        handle_paste(&mut state, "b-draft");
        handle_key(&mut state, KeyCode::Left, KeyModifiers::NONE, &jobs).unwrap();
        let b_cursor = state.view().unwrap().editor.cursor;
        state.select(Some("a".into()));
        for version in 1..50 {
            state.event(attention_event("other-attention", "b", version, "Open"));
            draw_state(&mut state, 80, 30);
        }
        assert_eq!(state.view().unwrap().editor.selection(), 4..6);
        handle_paste(&mut state, "XY");
        state.receive(Update {
            input: "abcdef".into(),
            work: Some("a".into()),
            conversation: None,
            result: Ok(Outcome::ReadResponse(json!({"status":"ok"}))),
        });
        assert_eq!(state.view().unwrap().draft, "abcdXY");
        assert_eq!(state.view().unwrap().editor.cursor, 6);
        state.select(Some("b".into()));
        assert_eq!(state.view().unwrap().draft, "b-draft");
        assert_eq!(state.view().unwrap().editor.cursor, b_cursor);
        state.receive(Update {
            input: "abcdXY".into(),
            work: Some("a".into()),
            conversation: None,
            result: Ok(Outcome::ReadResponse(json!({"status":"ok"}))),
        });
        let a = &state.views[&Some("a".into())];
        assert_eq!(a.draft, "");
        assert_eq!(a.editor.cursor, 0);
        assert_eq!(a.editor.anchor, None);
        assert_eq!(state.view().unwrap().editor.cursor, b_cursor);
    }

    #[test]
    fn navigation_diagnostics_exclude_text_and_preserve_key_kind_and_modifiers() {
        for code in [KeyCode::PageUp, KeyCode::PageDown, KeyCode::F(12)] {
            for kind in [
                KeyEventKind::Press,
                KeyEventKind::Repeat,
                KeyEventKind::Release,
            ] {
                let key = KeyEvent {
                    kind,
                    ..KeyEvent::new(code, KeyModifiers::CONTROL)
                };
                assert_eq!(navigation_key(&Event::Key(key)), Some(key));
            }
        }
        for event in [
            Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Event::Paste("private input".into()),
            Event::Resize(80, 24),
        ] {
            assert!(navigation_key(&event).is_none());
        }
    }

    #[test]
    fn paste_completion_and_confirmation_preserve_editor_contracts() {
        let _locale = crate::test_support::lock_locale();
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        let mut state = State::legacy();
        handle_paste(&mut state, "first\r\nsecond\rtail");
        assert_eq!(state.view().unwrap().draft, "first\nsecond\ntail");
        handle_key(&mut state, KeyCode::Home, KeyModifiers::NONE, &jobs).unwrap();
        handle_key(&mut state, KeyCode::Enter, KeyModifiers::SHIFT, &jobs).unwrap();
        assert_eq!(state.view().unwrap().draft, "first\nsecond\n\ntail");
        handle_key(&mut state, KeyCode::Right, KeyModifiers::SHIFT, &jobs).unwrap();
        let selection = state.view().unwrap().editor.selection();
        let original = state.view().unwrap().draft.clone();
        long_confirmation(&mut state);
        handle_paste(&mut state, "MUST NOT CHANGE FROZEN INPUT");
        handle_key(&mut state, KeyCode::Delete, KeyModifiers::NONE, &jobs).unwrap();
        assert_eq!(state.view().unwrap().draft, original);
        assert_eq!(state.view().unwrap().editor.selection(), selection);
        handle_key(&mut state, KeyCode::Esc, KeyModifiers::NONE, &jobs).unwrap();
        assert_eq!(state.view().unwrap().editor.selection(), selection);
        handle_key(&mut state, KeyCode::Char('a'), KeyModifiers::CONTROL, &jobs).unwrap();
        handle_paste(&mut state, "/wor");
        handle_key(&mut state, KeyCode::Tab, KeyModifiers::NONE, &jobs).unwrap();
        let draft = state.view().unwrap().draft.clone();
        assert!(draft.starts_with("/work"));
        assert_eq!(state.view().unwrap().editor.cursor, draft.len());
        assert_eq!(state.view().unwrap().editor.anchor, None);
        let caret = state.view().unwrap().editor.cursor;
        state.view_mut().follow = false;
        handle_key(&mut state, KeyCode::End, KeyModifiers::CONTROL, &jobs).unwrap();
        assert!(state.view().unwrap().follow);
        assert_eq!(state.view().unwrap().editor.cursor, caret);
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn rendered_caret_tracks_unicode_wrapping_selection_and_resize() {
        use ratatui::backend::Backend;
        let _locale = crate::test_support::lock_locale();
        let (jobs, _receiver) = mpsc::unbounded_channel();
        let mut state = State::legacy();
        handle_paste(&mut state, "\u{4e2d}e\u{301}\u{1f469}\u{200d}\u{1f4bb}tail");
        handle_key(&mut state, KeyCode::Home, KeyModifiers::NONE, &jobs).unwrap();
        handle_key(&mut state, KeyCode::Right, KeyModifiers::NONE, &jobs).unwrap();
        for (width, height) in [(80, 30), (8, 24), (140, 45)] {
            let mut terminal =
                Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| render(frame, &mut state)).unwrap();
            let cursor = terminal.backend_mut().get_cursor_position().unwrap();
            let input =
                renderer::regions(&mut state, ratatui::layout::Rect::new(0, 0, width, height))
                    .editor;
            assert_eq!((cursor.x, cursor.y), (input.x + 2, input.y));
            assert_eq!(
                terminal.backend().buffer().cell(cursor).unwrap().symbol(),
                "e\u{301}"
            );
        }
        handle_key(&mut state, KeyCode::Right, KeyModifiers::SHIFT, &jobs).unwrap();
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(80, 30)).unwrap();
        terminal.draw(|frame| render(frame, &mut state)).unwrap();
        let input = renderer::regions(&mut state, ratatui::layout::Rect::new(0, 0, 80, 30)).editor;
        assert!(terminal
            .backend()
            .buffer()
            .cell((input.x + 2, input.y))
            .unwrap()
            .modifier
            .contains(ratatui::style::Modifier::REVERSED));
        handle_key(&mut state, KeyCode::Char('a'), KeyModifiers::CONTROL, &jobs).unwrap();
        handle_paste(
            &mut state,
            &(0..20)
                .map(|i| format!("line-{i:02}\n"))
                .collect::<String>(),
        );
        assert!(draw_state(&mut state, 80, 30).contains("line-19"));
        handle_key(&mut state, KeyCode::Home, KeyModifiers::CONTROL, &jobs).unwrap();
        let top = draw_state(&mut state, 80, 30);
        assert!(top.contains("line-00"));
        assert!(
            !top.contains("line-19"),
            "input viewport must follow the caret, not the tail"
        );
    }

    fn draw_state(state: &mut State, width: u16, height: u16) -> String {
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, state)).unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    fn long_confirmation(state: &mut State) -> Value {
        state.diagnostics = true;
        let operation = Operation {
            method: "workspace.takeover".into(),
            params: json!({"workspaceId":"workspace-a"}),
            if_match: vec![json!({"kind":"Workspace","id":"workspace-a","version":7})],
            mutation: true,
            confirmation: true,
            command_id: "frozen-command".into(),
        };
        let preview = json!({
            "affectedTasks":(0..50).map(|index| format!("affected-task-{index:03} with a long wrapped description")).collect::<Vec<_>>(),
            "operation":operation_preview(&operation)
        });
        state.receive(Update {
            work: state.context.work_id.clone(),
            input: state.view().unwrap().draft.clone(),
            conversation: None,
            result: Ok(Outcome::StartPreview {
                operation,
                preview: preview.clone(),
                origin: MutationOrigin::Editor,
            }),
        });
        preview
    }

    #[test]
    fn confirmation_preview_never_overscrolls_and_page_up_responds_immediately() {
        let _locale = crate::test_support::lock_locale();
        let mut state = State::legacy();
        state.select(Some("work-a".into()));
        state.view_mut().draft = "/workspace takeover".into();
        let frozen = long_confirmation(&mut state);
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        for (width, height) in [(120, 30), (64, 24), (140, 45)] {
            for _ in 0..80 {
                handle_key(&mut state, KeyCode::PageDown, KeyModifiers::NONE, &jobs).unwrap();
                let text = draw_state(&mut state, width, height);
                assert!(
                    text.contains("affected-task")
                        || text.contains("workspace-a")
                        || text.contains("frozen-command"),
                    "confirmation body became blank at {width}x{height}, offset {}: {text}",
                    state.pending.as_ref().unwrap().scroll
                );
            }
            let bottom = draw_state(&mut state, width, height);
            assert!(
                bottom.contains("frozen-command"),
                "end of the frozen preview must stay visible"
            );
            handle_key(&mut state, KeyCode::PageUp, KeyModifiers::NONE, &jobs).unwrap();
            assert_ne!(
                draw_state(&mut state, width, height),
                bottom,
                "PageUp must not consume accumulated overscroll"
            );
            state.event(attention_event("new-attention", "work-a", 1, "Open"));
            assert_eq!(state.pending.as_ref().unwrap().preview, frozen);
        }
        assert!(
            receiver.try_recv().is_err(),
            "scrolling must not confirm an operation"
        );
        handle_key(&mut state, KeyCode::Enter, KeyModifiers::CONTROL, &jobs).unwrap();
        let job = receiver.try_recv().unwrap();
        let JobKind::Send(operation) = job.kind else {
            panic!("expected frozen confirmation")
        };
        assert_eq!(operation_preview(&operation), frozen["operation"]);
    }

    #[test]
    fn preview_reading_preserves_history_and_new_previews_start_at_the_top() {
        let _locale = crate::test_support::lock_locale();
        let mut state = State::legacy();
        state.diagnostics = true;
        state.view_mut().transcript.push(
            (0..100)
                .map(|i| format!("history-line-{i:03}"))
                .collect::<Vec<_>>()
                .join("\n")
                .into(),
        );
        let (jobs, _receiver) = mpsc::unbounded_channel();
        draw_state(&mut state, 120, 30);
        handle_key(&mut state, KeyCode::PageUp, KeyModifiers::NONE, &jobs).unwrap();
        let history = draw_state(&mut state, 120, 30);
        let notice = state.notice.clone();
        assert!(
            history.contains("history-line-080"),
            "PageUp from latest must not jump to the beginning"
        );
        let scroll = state.view().unwrap().scroll;
        let follow = state.view().unwrap().follow;
        long_confirmation(&mut state);
        assert!(draw_state(&mut state, 120, 30).contains("affected-task-000"));
        for _ in 0..40 {
            handle_key(&mut state, KeyCode::PageDown, KeyModifiers::NONE, &jobs).unwrap();
            draw_state(&mut state, 120, 30);
        }
        handle_key(&mut state, KeyCode::Esc, KeyModifiers::NONE, &jobs).unwrap();
        assert_eq!(state.view().unwrap().scroll, scroll);
        assert_eq!(state.view().unwrap().follow, follow);
        assert_eq!(
            state.notice,
            t!("agent_center.confirm_cancelled").into_owned()
        );
        state.notice = notice;
        assert_eq!(
            draw_state(&mut state, 120, 30),
            history,
            "history content and reading position must be restored"
        );
        long_confirmation(&mut state);
        assert!(draw_state(&mut state, 120, 30).contains("affected-task-000"));
    }

    #[test]
    fn typed_uuid_tail_survives_rendering_completion_and_service_updates() {
        let _locale = crate::test_support::lock_locale();
        let mut state = State::legacy();
        let id = "f19e8252-13c4-4374-a822-b43bb2f2c467";
        state.context.versions.insert(("Work".into(), id.into()), 1);
        let expected = format!("/work use {id}");
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        for (index, character) in expected.chars().enumerate() {
            handle_key(
                &mut state,
                KeyCode::Char(character),
                KeyModifiers::NONE,
                &jobs,
            )
            .unwrap();
            state.event(attention_event(
                "input-attention",
                "other-work",
                index as u64 + 1,
                "Open",
            ));
            draw_state(&mut state, 120, 30);
            assert_eq!(state.view().unwrap().draft, expected[..=index]);
        }
        assert_eq!(state.view().unwrap().draft, expected);
        handle_key(&mut state, KeyCode::Enter, KeyModifiers::NONE, &jobs).unwrap();
        let job = receiver.try_recv().unwrap();
        assert!(matches!(job.kind, JobKind::Send(ref operation)
            if operation.method == "work.open" && operation.params["workId"] == id));
    }

    fn attention_event(id: &str, work: &str, version: u64, status: &str) -> Value {
        json!({"type":"event","eventId":format!("{id}-{version}"),"cursor":format!("store:{version}"),
            "kind":"AttentionChanged","workId":work,"changes":[{
                "subject":{"kind":"AttentionItem","id":id,"version":version},
                "view":{"kind":"AttentionItem","id":id,"workId":work,"version":version,
                    "status":status,"reason":"DecisionRequested","subjectId":format!("decision-{work}")}}]})
    }

    #[test]
    fn scoped_and_empty_inbox_refreshes_preserve_other_work_and_event_tombstones() {
        let _locale = crate::test_support::lock_locale();
        let mut state = State::legacy();
        state.event(attention_event("attention-a", "a", 1, "Open"));
        state.event(attention_event("attention-b", "b", 1, "Open"));
        let refresh = InboxRefresh {
            operation: Operation::read("inbox.list", json!({"workId":"a","limit":100})),
            versions: state.event_versions.clone(),
        };
        let item = state.inbox[0].clone();
        state.observe_inbox(
            &json!({"status":"ok","data":{"items":[item.clone()]}}),
            &refresh,
        );
        assert_eq!(state.inbox.len(), 2);
        state.observe_inbox(&json!({"status":"ok","data":{"items":[]}}), &refresh);
        assert_eq!(state.inbox.len(), 1);
        assert_eq!(state.inbox[0]["workId"], "b");
        state.event(attention_event("attention-a", "a", 2, "Resolved"));
        state.observe_inbox(&json!({"status":"ok","data":{"items":[item]}}), &refresh);
        assert_eq!(state.inbox.len(), 1);
        let global = InboxRefresh {
            operation: Operation::read("inbox.list", json!({"limit":100})),
            versions: state.event_versions.clone(),
        };
        state.event(attention_event("attention-c", "c", 1, "Open"));
        state.observe_inbox(&json!({"status":"ok","data":{"items":[]}}), &global);
        assert_eq!(state.inbox.len(), 1);
        assert_eq!(state.inbox[0]["workId"], "c");
    }

    #[test]
    fn routine_flood_is_silent_and_attention_is_coalesced_without_stealing_b_draft() {
        let _locale = crate::test_support::lock_locale();
        let mut state = State::legacy();
        state.select(Some("b".into()));
        state.view_mut().draft = "unfinished b".into();
        state.view_mut().scroll = 9;
        state.view_mut().follow = false;
        state.notice = "existing notice".into();
        state.event(attention_event("attention-a", "a", 1, "Open"));
        for version in 2..502 {
            state.event(attention_event("attention-a", "a", version, "Open"));
            state.event(json!({"eventId":format!("progress-{version}"),"kind":"ProgressReported",
                "workId":"a","changes":[{"subject":{"kind":"ProgressReport","id":"report","version":version},
                    "view":{"text":"progress"}}]}));
            state.event(json!({"eventId":format!("text-{version}"),"kind":"TextDelta","workId":"a",
                "changes":[{"subject":{"kind":"Conversation","id":"conversation","version":version},
                    "view":{"messageId":"message","partId":"part","chunkIndex":version,"text":"text"}}]}));
        }
        assert_eq!(state.inbox.len(), 1);
        assert_eq!(state.notice, "existing notice");
        assert_eq!(state.context.work_id.as_deref(), Some("b"));
        assert_eq!(state.view().unwrap().draft, "unfinished b");
        assert_eq!(state.view().unwrap().scroll, 9);
        assert!(!state.view().unwrap().follow);
        assert_eq!(state.views[&Some("a".into())].transcript.len(), 200);
        state.event(attention_event("attention-b", "b", 1, "Open"));
        state.event(attention_event("attention-a", "a", 502, "Resolved"));
        assert_eq!(state.inbox.len(), 1);
        assert_eq!(state.inbox[0]["workId"], "b");
    }

    #[test]
    fn answering_a_decision_leaves_b_draft_and_unrelated_attention_intact() {
        let _locale = crate::test_support::lock_locale();
        let mut state = State::legacy();
        state.select(Some("b".into()));
        state.view_mut().draft = "unfinished b".into();
        state.view_mut().scroll = 14;
        state.event(attention_event("attention-a", "a", 1, "Open"));
        state.event(attention_event("attention-b", "b", 1, "Open"));
        state.select(Some("a".into()));
        state
            .context
            .versions
            .insert(("DecisionRequest".into(), "decision-a".into()), 3);
        state.view_mut().draft = "/decision answer decision-a --value true".into();
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        submit(&mut state, &jobs).unwrap();
        let job = receiver.try_recv().unwrap();
        let JobKind::Send(operation) = job.kind else {
            panic!("expected answer")
        };
        assert_eq!(
            operation.params,
            json!({"decisionId":"decision-a","value":true})
        );
        assert_eq!(
            operation.if_match,
            [json!({"kind":"DecisionRequest","id":"decision-a","version":3})]
        );
        state.select(Some("b".into()));
        state.event(attention_event("attention-a", "a", 2, "Resolved"));
        state.receive(Update {
            work: job.work,
            input: job.input,
            conversation: None,
            result: Ok(Outcome::Mutation {
                command_id: operation.command_id,
                result: Ok(json!({"status":"ok","data":{},"subjects":[]})),
            }),
        });
        assert_eq!(state.context.work_id.as_deref(), Some("b"));
        assert_eq!(state.view().unwrap().draft, "unfinished b");
        assert_eq!(state.view().unwrap().scroll, 14);
        assert_eq!(state.inbox.len(), 1);
        assert_eq!(state.inbox[0]["id"], "attention-b");
    }

    #[test]
    fn transfer_preparation_confirmation_retry_and_cancel_keep_frozen_target_and_drafts() {
        let _locale = crate::test_support::lock_locale();
        let mut state = State::legacy();
        state.select(Some("a".into()));
        state
            .context
            .versions
            .insert(("Work".into(), "a".into()), 7);
        let draft = "/workspace handback --summary 'updated input' --resume-affected false";
        state.view_mut().draft = draft.into();
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        submit(&mut state, &jobs).unwrap();
        let job = receiver.try_recv().unwrap();
        let JobKind::PrepareTransfer(target) = job.kind else {
            panic!("expected preparation")
        };
        assert_eq!(target.work_id, "a");
        assert_eq!(target.work_version, Some(7));
        assert_eq!(target.handback, Some(("updated input".into(), false)));
        state.select(Some("b".into()));
        state.view_mut().draft = "unfinished b".into();
        let operation = Operation {
            method: "workspace.handback".into(),
            params: json!({"workspaceId":"workspace-a","summary":"updated input","resumeAffected":false}),
            if_match: vec![json!({"kind":"Workspace","id":"workspace-a","version":8})],
            mutation: true,
            confirmation: true,
            command_id: target.command_id,
        };
        let frozen = operation_preview(&operation);
        state.receive(Update {
            work: job.work,
            input: job.input,
            conversation: None,
            result: Ok(Outcome::StartPreview {
                operation,
                preview: frozen.clone(),
                origin: MutationOrigin::Editor,
            }),
        });
        state.event(attention_event("attention-a", "a", 1, "Open"));
        assert_eq!(state.pending.as_ref().unwrap().preview, frozen);
        handle_key(&mut state, KeyCode::Enter, KeyModifiers::NONE, &jobs).unwrap();
        assert!(receiver.try_recv().is_err());
        handle_key(&mut state, KeyCode::Enter, KeyModifiers::CONTROL, &jobs).unwrap();
        let sent = receiver.try_recv().unwrap();
        let JobKind::Send(sent_operation) = sent.kind else {
            panic!("expected send")
        };
        assert_eq!(operation_preview(&sent_operation), frozen);
        state.receive(Update {
            work: sent.work,
            input: sent.input,
            conversation: None,
            result: Ok(Outcome::Mutation {
                command_id: sent_operation.command_id.clone(),
                result: Err(anyhow::anyhow!("transport failed")),
            }),
        });
        state
            .context
            .versions
            .insert(("Workspace".into(), "workspace-a".into()), 99);
        let exact_command_id = state.pending.as_ref().unwrap().operation.command_id.clone();
        state.pending.as_mut().unwrap().operation.command_id = uuid::Uuid::new_v4().to_string();
        assert!(handle_key(&mut state, KeyCode::Enter, KeyModifiers::CONTROL, &jobs).is_err());
        assert!(receiver.try_recv().is_err());
        state.pending.as_mut().unwrap().operation.command_id = exact_command_id;
        handle_key(&mut state, KeyCode::Enter, KeyModifiers::CONTROL, &jobs).unwrap();
        let retried = receiver.try_recv().unwrap();
        let JobKind::Send(retried_operation) = retried.kind else {
            panic!("expected frozen retry")
        };
        assert_eq!(operation_preview(&retried_operation), frozen);
        state.receive(Update {
            work: retried.work,
            input: retried.input,
            conversation: None,
            result: Ok(Outcome::Mutation {
                command_id: retried_operation.command_id,
                result: Ok(json!({"status":"conflict"})),
            }),
        });
        handle_key(&mut state, KeyCode::Esc, KeyModifiers::NONE, &jobs).unwrap();
        assert!(state.pending.is_none());
        assert!(state.retry.is_none());
        assert_eq!(state.context.work_id.as_deref(), Some("b"));
        assert_eq!(state.view().unwrap().draft, "unfinished b");
        assert_eq!(state.views[&Some("a".into())].draft, draft);
    }

    #[test]
    fn work_switch_restores_independent_drafts_and_reading_positions() {
        let _locale = crate::test_support::lock_locale();
        let mut state = State::legacy();
        state.select(Some("a".into()));
        state.view_mut().draft = "unfinished a".into();
        state.view_mut().scroll = 12;
        state.select(Some("b".into()));
        state.view_mut().draft = "unfinished b".into();
        state.select(Some("a".into()));
        assert_eq!(state.view().unwrap().draft, "unfinished a");
        assert_eq!(state.view().unwrap().scroll, 12);
    }
    #[test]
    fn background_response_cannot_steal_input_or_selection() {
        let _locale = crate::test_support::lock_locale();
        let mut state = State::legacy();
        state.select(Some("a".into()));
        state.view_mut().draft = "new draft".into();
        state.receive(Update {
            work: Some("b".into()),
            conversation: None,
            input: "old draft".into(),
            result: Ok(Outcome::ReadResponse(
                json!({"status":"ok","data":{},"subjects":[]}),
            )),
        });
        assert_eq!(state.context.work_id.as_deref(), Some("a"));
        assert_eq!(state.view().unwrap().draft, "new draft");
    }
    #[test]
    fn errors_and_conflicts_preserve_input() {
        let _locale = crate::test_support::lock_locale();
        let mut state = State::legacy();
        state.view_mut().draft = "/review accept candidate".into();
        state.receive(Update {
            work: None,
            conversation: None,
            input: "/review accept candidate".into(),
            result: Ok(Outcome::ReadResponse(
                json!({"status":"conflict","subjects":[]}),
            )),
        });
        assert_eq!(state.view().unwrap().draft, "/review accept candidate");
    }

    #[test]
    fn events_are_deduplicated_without_stealing_confirmation_or_draft() {
        let _locale = crate::test_support::lock_locale();
        let mut state = State::legacy();
        state.select(Some("a".into()));
        state.view_mut().draft = "answer the question in a".into();
        state.pending = Some(PendingConfirmation {
            operation: Operation::read("work.get", json!({"workId":"a"})),
            preview: json!({"workId":"a","version":2}),
            work: Some("a".into()),
            input: "answer the question in a".into(),
            scroll: 0,
            target_label: "a".into(),
            origin: MutationOrigin::Action,
        });
        let event = json!({"type":"event","eventId":"event-1","cursor":"store:1",
        "kind":"MessageDelta","workId":"b","changes":[{
            "subject":{"kind":"Conversation","id":"conversation","version":1},
            "view":{"messageId":"message","partId":"part","chunkIndex":0,"text":"hello"}
        }]});
        state.event(event.clone());
        state.event(event.clone());
        let mut duplicate_chunk = event;
        duplicate_chunk["eventId"] = json!("event-2");
        state.event(duplicate_chunk);
        assert_eq!(state.views[&Some("b".into())].transcript.len(), 1);
        assert_eq!(state.context.work_id.as_deref(), Some("a"));
        assert_eq!(state.view().unwrap().draft, "answer the question in a");
        assert_eq!(state.pending.as_ref().unwrap().preview["version"], 2);
    }

    #[test]
    fn failed_navigation_preserves_selection_and_command() {
        let _locale = crate::test_support::lock_locale();
        let mut state = State::legacy();
        state.select(Some("a".into()));
        state.view_mut().draft = "/work use missing".into();
        state.receive(Update {
            work: Some("a".into()),
            conversation: None,
            input: "/work use missing".into(),
            result: Ok(Outcome::SelectedWork {
                id: "missing".into(),
                response: json!({"status":"error","subjects":[]}),
            }),
        });
        assert_eq!(state.context.work_id.as_deref(), Some("a"));
        assert_eq!(state.view().unwrap().draft, "/work use missing");
    }

    #[test]
    fn keyboard_switch_preserves_unsent_message_and_restores_target_draft() {
        let _locale = crate::test_support::lock_locale();
        let mut state = State::legacy();
        state.select(Some("b".into()));
        state.view_mut().draft = "draft b".into();
        state.select(Some("a".into()));
        state.view_mut().draft = "draft a".into();
        state.receive(Update { work:Some("a".into()), input:"draft a".into(), conversation:None,
            result:Ok(Outcome::SelectedWork { id:"b".into(),
                response:json!({"status":"ok","subjects":[],"data":{"work":{"id":"b","version":1,"projectId":"p"}}}) }) });
        assert_eq!(state.view().unwrap().draft, "draft b");
        assert_eq!(state.views[&Some("a".into())].draft, "draft a");
    }

    #[test]
    fn renders_service_data_and_exact_confirmation_without_sample_cards() {
        let _locale = crate::test_support::lock_locale();
        let mut state = State::legacy();
        state.works.push(
            json!({"work":{"id":"work-real","lifecycle":"Active","phase":"OBSOLETE_PHASE"},
            "spec":{"goal":"Requested goal"}}),
        );
        state.append(
            None,
            &json!({"status":"pending","operationId":"operation-real"}),
        );
        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, &mut state)).unwrap();
        let screen = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(!screen.contains("operation-real"));
        assert!(screen.contains("Requested goal"));
        assert!(screen.contains("Active"));
        assert!(!screen.contains("OBSOLETE_PHASE"));
        state.diagnostics = true;
        assert!(draw_state(&mut state, 100, 30).contains("operation-real"));
    }

    #[test]
    fn default_notices_help_and_failures_are_readable_without_scrubbing_user_content() {
        let _locale = crate::test_support::lock_locale();
        assert!(
            !status_label(&json!({"status":"pending","operationId":"internal-operation"}))
                .contains("internal-operation")
        );
        assert!(!status_label(
            &json!({"status":"needs_input","inputRequest":{"id":"internal-question"}})
        )
        .contains("internal-question"));
        let mut state = State::legacy();
        let (jobs, _receiver) = mpsc::unbounded_channel();
        state.view_mut().replace_draft("/help work".into());
        submit(&mut state, &jobs).unwrap();
        let help = draw_state(&mut state, 160, 45);
        assert!(help.contains("/work start"));
        assert!(!help.contains("\"command\"") && !help.contains("requestContract"));
        state.append(
            None,
            &json!({"status":"error","failure":{
                "message":"Actual user error: literal-user-id at C:\\Reports\\failed.log",
                "subjects":[{"kind":"Work","id":"internal-work","version":1}]
            }}),
        );
        let failure = draw_state(&mut state, 160, 45);
        assert!(failure.contains("Actual user error: literal-user-id at C:\\Reports\\failed.log"));
        assert!(!failure.contains("internal-work"));
        state.diagnostics = true;
        assert!(draw_state(&mut state, 160, 45).contains("internal-work"));
    }

    #[test]
    fn full_frame_conversation_intake_and_delivery_hide_only_protocol_identity_fields() {
        let _locale = crate::test_support::lock_locale();
        let work_id = "11111111-1111-4111-8111-111111111111";
        let project_id = "22222222-2222-4222-8222-222222222222";
        let conversation_id = "33333333-3333-4333-8333-333333333333";
        let message_id = "44444444-4444-4444-8444-444444444444";
        let question_id = "55555555-5555-4555-8555-555555555555";
        let candidate_id = "66666666-6666-4666-8666-666666666666";
        let user_id = "77777777-7777-4777-8777-777777777777";
        let path = format!("C:\\Reports\\manual-{user_id}.txt");
        let work = json!({"kind":"Work","id":work_id,"version":3,"projectId":project_id,
            "lifecycle":"Active","desiredAdvancement":"Advance","currentCandidateId":candidate_id});
        let candidate = json!({"kind":"DeliveryCandidate","id":candidate_id,"workId":work_id,
            "version":2,"status":"Proposed","destination":{"kind":"Report","reportPaths":[path]},
            "knownLimitations":[]});
        let mut state = State::legacy();
        state.projects =
            vec![json!({"kind":"Project","id":project_id,"name":"Reports","root":"C:\\Reports"})];
        state.receive(Update {
            work:None, input:String::new(), conversation:None,
            result:Ok(Outcome::SelectedWork { id:work_id.into(), response:json!({"status":"ok","data":{
                "work":work,"candidate":candidate,"spec":{"goal":"Readable report goal","delivery":{"kind":"Report"}},
                "taskSummaries":[],"obligations":[]
            }}) }),
        });
        state.append(Some(work_id.into()), &json!({
            "kind":"Conversation","id":conversation_id,"version":2,
            "messages":[{"kind":"ConversationItem","id":message_id,"conversationId":conversation_id,
                "role":"User","status":"Complete","text":format!("Keep user-supplied UUID {user_id} and this actual path: {path}")}],
            "intakeRequests":[{"kind":"IntakeRequest","id":question_id,"conversationId":conversation_id,
                "version":1,"status":"Open","question":"Choose report detail","responseSchema":{"type":"boolean"}}]
        }));
        state.receive(Update {
            work:Some(work_id.into()), input:String::new(), conversation:None,
            result:Ok(Outcome::Inspected(json!({"work":work,"candidate":candidate,
                "result":{"body":{"summary":"Verified report contents"}},"artifacts":[{"localPath":path}]}))),
        });
        let normal = draw_state(&mut state, 200, 90);
        assert!(normal.contains("Readable report goal"));
        assert!(normal.contains("Choose report detail"));
        assert!(normal.contains("Verified report contents"));
        assert!(normal.contains(user_id) && normal.contains(&path));
        for id in [
            work_id,
            project_id,
            conversation_id,
            message_id,
            question_id,
            candidate_id,
        ] {
            assert!(
                !normal.contains(id),
                "protocol identity leaked into the normal frame: {id}"
            );
        }
        let (jobs, _receiver) = mpsc::unbounded_channel();
        handle_key(&mut state, KeyCode::F(12), KeyModifiers::NONE, &jobs).unwrap();
        let diagnostic = draw_state(&mut state, 200, 180);
        for id in [
            work_id,
            project_id,
            conversation_id,
            message_id,
            question_id,
            candidate_id,
        ] {
            assert!(diagnostic.contains(id), "diagnostic identity missing: {id}");
        }
        assert!(diagnostic.contains(user_id) && diagnostic.contains("manual-"));
    }

    #[test]
    fn form_viewport_follows_required_fields_and_wrapped_answers_without_moving_work_draft() {
        let _locale = crate::test_support::lock_locale();
        let mut state = State::legacy();
        let (jobs, _receiver) = mpsc::unbounded_channel();
        handle_paste(&mut state, "untouched work draft");
        handle_key(&mut state, KeyCode::Left, KeyModifiers::SHIFT, &jobs).unwrap();
        let caret = state.view().unwrap().editor.cursor;
        let anchor = state.view().unwrap().editor.anchor;
        let properties = (0..12)
            .map(|index| {
                (
                    format!("field{index:02}"),
                    json!({
                        "type":"string","description":"A long field description. ".repeat(70)
                    }),
                )
            })
            .collect::<serde_json::Map<_, _>>();
        state.form = Some(form::Form::new(json!({
            "kind":"IntakeRequest","id":"question","version":1,"status":"Open",
            "question":"A long question with necessary context. ".repeat(80),
            "responseSchema":{"type":"object","additionalProperties":false,
                "properties":properties,"required":(0..12).map(|index| format!("field{index:02}")).collect::<Vec<_>>()}
        }), None).unwrap());
        for _ in 0..11 {
            handle_key(&mut state, KeyCode::Down, KeyModifiers::NONE, &jobs).unwrap();
        }
        handle_paste(&mut state, "answer-tail");
        for (width, height) in [(80, 20), (48, 18), (120, 24)] {
            let rendered = draw_state(&mut state, width, height);
            assert!(
                rendered.contains("field11"),
                "selected required field clipped: {rendered}"
            );
            assert!(
                rendered.contains("answer-tail"),
                "selected answer clipped: {rendered}"
            );
        }
        handle_paste(
            &mut state,
            &format!(" {} END-OF-ANSWER", "wrapped words ".repeat(100)),
        );
        assert!(draw_state(&mut state, 80, 20).contains("END-OF-ANSWER"));
        handle_key(&mut state, KeyCode::Home, KeyModifiers::CONTROL, &jobs).unwrap();
        assert!(draw_state(&mut state, 80, 20).contains("field11"));
        handle_key(&mut state, KeyCode::PageDown, KeyModifiers::NONE, &jobs).unwrap();
        draw_state(&mut state, 80, 20);
        handle_key(&mut state, KeyCode::Up, KeyModifiers::NONE, &jobs).unwrap();
        assert!(draw_state(&mut state, 80, 20).contains("field10"));
        handle_key(&mut state, KeyCode::Esc, KeyModifiers::NONE, &jobs).unwrap();
        assert_eq!(state.view().unwrap().draft, "untouched work draft");
        assert_eq!(state.view().unwrap().editor.cursor, caret);
        assert_eq!(state.view().unwrap().editor.anchor, anchor);
    }

    #[test]
    fn asynchronous_input_preparation_keeps_original_target_and_draft() {
        let _locale = crate::test_support::lock_locale();
        let mut state = State::legacy();
        state.select(Some("a".into()));
        let submitted = "/workspace takeover --input-json 'transfer file.json'";
        state.view_mut().draft = submitted.into();
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        submit(&mut state, &jobs).unwrap();
        let queued = receiver.try_recv().unwrap();
        let JobKind::Resolve { context, .. } = queued.kind else {
            panic!("expected asynchronous file load");
        };
        assert_eq!(context.work_id.as_deref(), Some("a"));
        state.select(Some("b".into()));
        state.view_mut().draft = "new unsent draft".into();
        state.receive(Update {
            work: Some("a".into()),
            input: submitted.into(),
            conversation: None,
            result: Ok(Outcome::StartPreview {
                operation: Operation::read(
                    "workspace.takeover",
                    json!({"workspaceId":"workspace-a"}),
                ),
                preview: json!({"workspaceId":"workspace-a"}),
                origin: MutationOrigin::Editor,
            }),
        });
        handle_key(&mut state, KeyCode::Enter, KeyModifiers::CONTROL, &jobs).unwrap();
        let confirmation = receiver.try_recv().unwrap();
        assert_eq!(confirmation.work.as_deref(), Some("a"));
        assert_eq!(confirmation.input, submitted);
        state.receive(Update {
            work: confirmation.work,
            input: confirmation.input,
            conversation: None,
            result: Ok(Outcome::ReadResponse(
                json!({"status":"ok","data":{},"subjects":[]}),
            )),
        });
        assert_eq!(state.context.work_id.as_deref(), Some("b"));
        assert_eq!(state.view().unwrap().draft, "new unsent draft");
    }

    #[tokio::test]
    async fn prepared_request_keeps_file_identity_and_routes_intake_before_sending() {
        let _locale = crate::test_support::lock_locale();
        let mut state = State::legacy();
        state.select(Some("other-work".into()));
        state.view_mut().draft = "original request".into();
        let command_id = uuid::Uuid::new_v4().to_string();
        let conversation_id = uuid::Uuid::new_v4().to_string();
        let document = json!({"method":"conversation.submit","commandId":command_id,"ifMatch":[],
            "params":{"conversationId":conversation_id,"clientMessageId":uuid::Uuid::new_v4().to_string(),
                "text":"revise the requested check","declaredIntent":"WorkDiscussion",
                "context":{"consoleSessionId":uuid::Uuid::new_v4().to_string(),"contextVersion":1,
                    "projectId":"project","selectedWorkId":"file-work"}}});
        let outcome = resolve_operation(
            vec![
                "plan".into(),
                "revise".into(),
                "--request-json".into(),
                document.to_string(),
            ],
            state.context.clone(),
            uuid::Uuid::new_v4().to_string(),
        )
        .await
        .unwrap();
        state.receive(Update {
            work: Some("other-work".into()),
            input: "original request".into(),
            conversation: None,
            result: Ok(outcome),
        });
        let (operation, work, input, origin) =
            state.prepared.take().expect("prepared, not yet sent");
        assert_eq!(origin, MutationOrigin::Editor);
        assert_eq!(operation.command_id, command_id);
        assert_eq!(state.view().unwrap().draft, "original request");
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        queue_captured_with_origin(
            &mut state,
            &jobs,
            JobKind::Send(operation),
            work,
            input,
            origin,
        )
        .unwrap();
        let job = receiver.try_recv().unwrap();
        assert_eq!(job.conversation.as_deref(), Some(conversation_id.as_str()));
        assert_eq!(
            state.conversation_targets[&conversation_id].as_deref(),
            Some("file-work")
        );
        assert!(
            matches!(&state.retry, Some((_,_,JobKind::Send(operation))) if operation.command_id == command_id)
        );
        assert_eq!(state.context.work_id.as_deref(), Some("other-work"));
    }

    #[test]
    fn ordinary_apply_keeps_target_draft_and_frozen_confirmation_across_updates() {
        let _locale = crate::test_support::lock_locale();
        let mut state = State::legacy();
        state.select(Some("work-a".into()));
        state
            .context
            .versions
            .insert(("Work".into(), "work-a".into()), 7);
        state
            .context
            .versions
            .insert(("ChangeProposal".into(), "change-a".into()), 2);
        state.view_mut().draft = "/work apply change-a".into();
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        submit(&mut state, &jobs).unwrap();
        let job = receiver.try_recv().unwrap();
        let JobKind::PrepareApply(target) = job.kind else {
            panic!("expected proposal preparation")
        };
        assert_eq!(target.work_id, "work-a");
        state.select(Some("work-b".into()));
        state.view_mut().draft = "new work-b draft".into();
        let operation = Operation {
            method: "work.apply_change".into(),
            params: json!({"proposalId":"change-a","grantProposalId":"frozen-grant"}),
            if_match: vec![
                json!({"kind":"Work","id":"work-a","version":7}),
                json!({"kind":"ChangeProposal","id":"change-a","version":2}),
            ],
            mutation: true,
            confirmation: true,
            command_id: target.command_id,
        };
        let frozen = operation_preview(&operation);
        state.receive(Update {
            work: job.work,
            input: job.input,
            conversation: None,
            result: Ok(Outcome::StartPreview {
                operation,
                preview: frozen.clone(),
                origin: MutationOrigin::Editor,
            }),
        });
        state.observe(
            &json!({"subjects":[{"kind":"Work","id":"work-a","version":99},
            {"kind":"ChangeProposal","id":"change-a","version":99}]}),
        );
        handle_key(&mut state, KeyCode::Enter, KeyModifiers::NONE, &jobs).unwrap();
        assert!(receiver.try_recv().is_err());
        assert!(handle_key(&mut state, KeyCode::Enter, KeyModifiers::CONTROL, &jobs).is_err());
        assert!(receiver.try_recv().is_err());
        let pending = state.pending.as_ref().unwrap();
        assert_eq!(operation_preview(&pending.operation), frozen);
        assert_eq!(pending.work.as_deref(), Some("work-a"));
        assert_eq!(pending.input, "/work apply change-a");
        assert_eq!(state.context.work_id.as_deref(), Some("work-b"));
        assert_eq!(state.view().unwrap().draft, "new work-b draft");
        assert_eq!(
            state.views[&Some("work-a".into())].draft,
            "/work apply change-a"
        );
    }
}
