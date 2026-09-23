// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

use super::*;
use crate::theme;
use crate::ui::{card, composer, conversation};
use projection::{goal, text};
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;
use workflow::{Choice, Menu};

pub(super) struct Regions {
    rail: Rect,
    navigation: Rect,
    header: Rect,
    attention: Rect,
    body: Rect,
    notice: Rect,
    scope: Rect,
    composer: Rect,
    pub editor: Rect,
    completion: Rect,
    bordered: bool,
    keys: Rect,
}

fn rows(area: &mut Rect, height: u16) -> Rect {
    let height = height.min(area.height);
    let result = Rect::new(area.x, area.y, area.width, height);
    area.y += height;
    area.height -= height;
    result
}

pub(super) fn regions(state: &mut State, area: Rect) -> Regions {
    let area = if state.demo.is_some() {
        let width = area.width.min(108);
        let margin = u16::from(area.height >= 32);
        Rect::new(
            area.x + (area.width - width) / 2,
            area.y + margin,
            width,
            area.height.saturating_sub(margin * 2),
        )
    } else {
        area
    };
    let modal = state.pending.is_some()
        || state.menu.is_some()
        || state.form.is_some()
        || state.diagnostics;
    let rail_width = 0;
    let rail = Rect::new(area.x, area.y, rail_width, area.height);
    let mut main = Rect::new(
        area.x + rail_width,
        area.y,
        area.width - rail_width,
        area.height,
    );
    let navigation_height =
        u16::from(main.height >= 4 && state.pending.is_none() && state.form.is_none());
    let navigation = rows(&mut main, navigation_height);
    let bordered = main.height >= 8 && main.width > 2;
    let input_width = composer::layout(main, bordered).text.width.max(1);
    let has_completion = completion(state).is_some();
    let view = state.editor_view_mut();
    view.editor.width = usize::from(input_width);
    let desired =
        view.editor
            .layout(&view.draft)
            .lines
            .len()
            .clamp(1, usize::from(composer::INPUT_MAX_HEIGHT.saturating_sub(2))) as u16;
    let legacy = state.demo.as_ref().is_some_and(|demo| demo.legacy);
    let demo_details = state.demo.is_some() && state.focus == Focus::Details;
    let composer_height =
        if legacy || (state.task_list && !modal) || (state.demo.is_some() && modal) || demo_details
        {
            0
        } else if bordered {
            (desired + 2 + u16::from(has_completion)).min((main.height / 3).max(3))
        } else {
            main.height.min(2)
        };
    let key_height = if main.height >= 10 && modal && state.demo.is_none() {
        2
    } else {
        u16::from(main.height >= 8)
    };
    let scope_height = u16::from(
        !legacy
            && !state.task_list
            && !(state.demo.is_some() && (modal || demo_details || demo::banner(state).is_some()))
            && (main.height >= 6 || (state.thinking() && main.height >= 3)),
    );
    let available = main
        .height
        .saturating_sub(composer_height + key_height + scope_height);
    let header = rows(&mut main, if available >= 3 { 2 } else { 0 });
    let show_notice = state.dashboard
        || state.stale
        || state.unresolved_notice().is_some()
        || state.recovery_failure().is_some()
        || !matches!(state.notice_kind, NoticeKind::Info);
    let notice_height = if !show_notice {
        0
    } else if available >= 6 {
        if state.stale || state.unresolved_notice().is_some() || state.recovery_failure().is_some()
        {
            3
        } else {
            1
        }
    } else {
        u16::from(available > header.height)
    };
    let attention_height = if let Some(banner) = demo::banner(state) {
        Paragraph::new(banner)
            .wrap(Wrap { trim: false })
            .line_count(main.width.max(1))
            .min(6) as u16
    } else if state.task_list || !state.dashboard {
        0
    } else {
        state
            .attention_summary()
            .map(|text| {
                Paragraph::new(text)
                    .wrap(Wrap { trim: false })
                    .line_count(main.width.max(1))
                    .min(2) as u16
            })
            .unwrap_or(0)
            .min(available.saturating_sub(header.height + notice_height + 3))
    };
    let attention = rows(
        &mut main,
        attention_height.min(available.saturating_sub(header.height + notice_height + 3)),
    );
    let notice = rows(&mut main, notice_height);
    let body_height = main
        .height
        .saturating_sub(composer_height + key_height + scope_height);
    let body = rows(&mut main, body_height);
    let scope = rows(&mut main, scope_height);
    let mut composer = rows(&mut main, composer_height);
    let mut editor = composer::layout(composer, bordered).text;
    let completion = if has_completion && editor.height > 1 {
        composer.height -= 1;
        editor = composer::layout(composer, bordered).text;
        Rect::new(editor.x, composer.bottom(), editor.width, 1)
    } else {
        Rect::default()
    };
    let keys = main;
    Regions {
        rail,
        navigation,
        header,
        attention,
        body,
        notice,
        scope,
        composer,
        editor,
        completion,
        bordered,
        keys,
    }
}

fn completion(state: &State) -> Option<String> {
    if state.demo.is_some()
        || state.focus != Focus::Composer
        || state.pending.is_some()
        || state.form.is_some()
        || state.menu.is_some()
    {
        return None;
    }
    let draft = &state.editor_view()?.draft;
    if !draft.trim_start().starts_with('/') {
        return None;
    }
    let suggestions = commands::complete_with_context(draft, &state.context);
    suggestions
        .get(state.completion.min(suggestions.len().saturating_sub(1)))
        .cloned()
}

fn heading(value: impl Into<String>) -> Line<'static> {
    Line::styled(
        value.into(),
        theme::SYSTEM_TEXT.add_modifier(Modifier::BOLD),
    )
}

fn append(lines: &mut Vec<Line<'static>>, value: &str, style: Style) {
    lines.extend(
        value
            .lines()
            .map(|line| Line::styled(line.to_owned(), style)),
    );
}

fn section(lines: &mut Vec<Line<'static>>, title: &str, content: &str) {
    if !content.is_empty() {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        lines.push(heading(title));
        append(lines, content, Style::default());
    }
}

fn clipped(value: &str, width: usize) -> String {
    if value.width() <= width {
        return value.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let mut result = String::new();
    let mut used = 0;
    for grapheme in value.graphemes(true) {
        if used + grapheme.width() > width - 1 {
            break;
        }
        used += grapheme.width();
        result.push_str(grapheme);
    }
    result.push('…');
    result
}

impl State {
    pub(super) fn composer_focused(&self) -> bool {
        !self.task_list
            && !self.demo.as_ref().is_some_and(|demo| demo.legacy)
            && self.pending.is_none()
            && self.menu.is_none()
            && self.form.is_none()
            && self.focus == Focus::Composer
    }

    fn showing_work(&self) -> bool {
        self.context.work_id.is_some()
            && (!self.context.global_conversation
                || self.dashboard_work
                || self.view().is_some_and(|view| view.show_delivery))
    }
    fn attention_targets(&self) -> BTreeMap<String, Option<String>> {
        let mut targets = BTreeMap::new();
        for item in self.inbox.iter().filter(|item| attention_open(item)) {
            if let Some(id) = item["subjectId"]
                .as_str()
                .filter(|id| !id.is_empty())
                .or_else(|| item["id"].as_str().filter(|id| !id.is_empty()))
            {
                targets.insert(id.to_owned(), item["workId"].as_str().map(str::to_owned));
            }
        }
        for (work, view) in &self.views {
            for question in view.records.values().filter(|record| open_question(record)) {
                if let Some(id) = question["id"].as_str().filter(|id| !id.is_empty()) {
                    targets.entry(id.to_owned()).or_insert_with(|| work.clone());
                }
            }
        }
        targets
    }

    fn attention_summary(&self) -> Option<String> {
        let targets = self.attention_targets();
        if targets.is_empty() {
            return None;
        }
        let mut seen = BTreeSet::new();
        let names = targets
            .values()
            .filter(|work| **work != self.context.work_id)
            .chain(
                targets
                    .values()
                    .filter(|work| **work == self.context.work_id),
            )
            .filter(|work| seen.insert((*work).clone()))
            .take(2)
            .map(|work| self.compact_target(work.as_deref()))
            .collect::<Vec<_>>()
            .join(" · ");
        let summary = format!(
            "{} · {names}",
            t!("agent_center.inbox_count", count = targets.len())
        );
        Some(if self.stale {
            t!("agent_center.ui_last_known", status = summary).into_owned()
        } else {
            summary
        })
    }

    pub(super) fn selected_view(&self) -> Option<&Value> {
        if self.task_list {
            return self.tasks().get(self.task_index).copied();
        }
        self.works
            .iter()
            .find(|view| view["work"]["id"].as_str() == self.context.work_id.as_deref())
    }

    fn compact_project(&self, id: Option<&str>) -> String {
        self.projects
            .iter()
            .find(|project| project["id"].as_str() == id)
            .and_then(|project| project["name"].as_str())
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| t!("agent_center.console_unknown_project").into_owned())
    }

    fn compact_goal(&self, view: &Value) -> String {
        let title = goal(view);
        let matching = self
            .works
            .iter()
            .filter(|other| goal(other) == title)
            .collect::<Vec<_>>();
        if matching.len() > 1 {
            let number = matching
                .iter()
                .position(|other| other["work"]["id"] == view["work"]["id"])
                .unwrap_or(0)
                + 1;
            format!("{title} [{number}]")
        } else {
            title
        }
    }

    fn compact_target(&self, id: Option<&str>) -> String {
        self.works
            .iter()
            .find(|view| view["work"]["id"].as_str() == id)
            .map(|view| {
                format!(
                    "{} · {}",
                    self.compact_goal(view),
                    self.compact_project(view["work"]["projectId"].as_str())
                )
            })
            .unwrap_or_else(|| {
                if id.is_none() {
                    if self.context.global_conversation {
                        t!("agent_center.chat_global").into_owned()
                    } else {
                        t!("agent_center.console_home").into_owned()
                    }
                } else {
                    t!("agent_center.console_unknown_work").into_owned()
                }
            })
    }

    fn needs_attention(&self, view: &Value) -> bool {
        let id = view["work"]["id"].as_str();
        view["work"]["lifecycle"] == "Draft"
            || view["candidate"]["status"] == "Proposed"
            || view["obligations"]
                .as_array()
                .is_some_and(|items| !items.is_empty())
            || self.inbox.iter().any(|item| item["workId"].as_str() == id)
            || self.views.iter().any(|(work, view)| {
                work.as_deref() == id && view.records.values().any(open_question)
            })
            || self.mutations.values().any(|mutation| {
                mutation.selection.work.as_deref() == id && mutation.unknown.is_some()
            })
    }

    pub(super) fn visible_actions(&self) -> Menu {
        if self.demo.is_some() {
            return Menu {
                items: vec![],
                selected: 0,
            };
        }
        let mut menu = self.actions();
        if !self.context.global_conversation && !self.dashboard {
            menu.items.retain(|(_, choice)| match choice {
                Choice::Question(_, work) => *work == self.context.work_id,
                Choice::HumanProposal(_) => true,
                Choice::Reconcile(id) => self
                    .mutations
                    .get(id)
                    .is_some_and(|mutation| mutation.selection.work == self.context.work_id),
                _ => false,
            });
            menu.selected = self
                .view()
                .map(|view| view.action_index)
                .unwrap_or(0)
                .min(menu.items.len().saturating_sub(1));
            return menu;
        }
        if self.context.global_conversation && !self.dashboard {
            menu.items.retain(|(_, choice)| {
                if let Choice::HumanProposal(proposal) = choice {
                    if self.deferred_proposals.get(text(proposal, "id")).copied()
                        == proposal["version"].as_u64()
                    {
                        return false;
                    }
                }
                matches!(
                    choice,
                    Choice::Question(..) | Choice::Reconcile(_) | Choice::HumanProposal(_)
                )
            });
            return menu;
        }
        let showing_work = self.showing_work();
        menu.items.retain(|(_, choice)| match choice {
            Choice::Projects => !showing_work && self.home_screen == HomeScreen::Overview,
            Choice::SelectWork(id) if !showing_work => {
                self.home_screen == HomeScreen::Overview
                    || self
                        .works
                        .iter()
                        .find(|view| view["work"]["id"] == *id)
                        .is_some_and(|view| self.needs_attention(view))
            }
            Choice::Question(_, work) => !showing_work || *work == self.context.work_id,
            Choice::Reconcile(_) | Choice::HumanProposal(_) => true,
            Choice::Start(_)
            | Choice::Control(_)
            | Choice::Inspect(_)
            | Choice::Evidence(..)
            | Choice::Accept(..) => showing_work,
            Choice::OpenTab(_) => showing_work,
            Choice::Refresh => self.stale,
            _ => false,
        });
        menu.selected = self
            .view()
            .map(|view| view.action_index)
            .unwrap_or(0)
            .min(menu.items.len().saturating_sub(1));
        menu
    }

    fn action_label(&self, original: &str, choice: &Choice) -> String {
        let current = if !self.context.global_conversation || self.dashboard && self.dashboard_work
        {
            self.context.work_id.as_deref()
        } else {
            None
        };
        match choice {
            Choice::Start(id) if Some(id.as_str()) == current => {
                return t!("agent_center.console_start").into_owned()
            }
            Choice::Control(operation)
                if current.is_some() && operation.params["workId"].as_str() == current =>
            {
                return workflow::method_label(&operation.method, &operation.params);
            }
            Choice::Inspect(id) if Some(id.as_str()) == current => {
                return t!("agent_center.console_inspect").into_owned()
            }
            Choice::Accept(operation, _)
                if current.is_some() && operation.params["workId"].as_str() == current =>
            {
                return t!("agent_center.console_accept").into_owned();
            }
            Choice::Question(question, work) if current.is_some() && work.as_deref() == current => {
                return format!(
                    "{} · {}",
                    t!("agent_center.console_answer"),
                    text(question, "question")
                );
            }
            Choice::Evidence(operation, work, identity)
                if Some(work.as_str()) == current && identity.work == *work =>
            {
                return if operation.params["offset"].is_u64() {
                    t!(
                        "agent_center.console_more_evidence",
                        work = t!("agent_center.console_delivery")
                    )
                    .into_owned()
                } else {
                    t!(
                        "agent_center.console_read_evidence",
                        work = t!("agent_center.console_delivery"),
                        path = text(&operation.params, "relativePath")
                    )
                    .into_owned()
                };
            }
            _ => {}
        }
        match choice {
            Choice::SelectWork(id) => t!(
                "agent_center.console_open",
                work = self.compact_target(Some(id))
            )
            .into_owned(),
            Choice::Start(id) => format!(
                "{} · {}",
                t!("agent_center.console_start"),
                self.compact_target(Some(id))
            ),
            Choice::Control(operation) => format!(
                "{} · {}",
                workflow::method_label(&operation.method, &operation.params),
                self.compact_target(operation.params["workId"].as_str())
            ),
            Choice::Inspect(id) => format!(
                "{} · {}",
                t!("agent_center.console_inspect"),
                self.compact_target(Some(id))
            ),
            Choice::Question(question, work) => t!(
                "agent_center.console_respond",
                work = self.compact_target(work.as_deref()),
                question = text(question, "question")
            )
            .into_owned(),
            // These labels were captured with the fixed inspection or unknown
            // request; do not derive a different target from current selection.
            _ => original.to_owned(),
        }
    }
}

fn open_question(record: &Value) -> bool {
    matches!(text(record, "kind"), "IntakeRequest" | "DecisionRequest")
        && record["status"] == "Open"
}

fn observed_status(state: &State, value: &Value) -> String {
    let status = projection::status(value);
    if state.stale {
        t!("agent_center.ui_last_known", status = status).into_owned()
    } else {
        status
    }
}

fn notice(state: &State) -> String {
    let notice = state.unresolved_notice().unwrap_or_else(|| {
        if let Some(failure) = state.recovery_failure() {
            format!(
                "{}: {}",
                t!("agent_center.status_error"),
                projection::readable(failure)
            )
        } else if state.busy {
            t!("agent_center.sending").into_owned()
        } else if !state.context.global_conversation
            && !state.dashboard
            && matches!(state.notice_kind, NoticeKind::Info)
        {
            String::new()
        } else {
            state.notice.clone()
        }
    });
    if state.stale {
        format!("{}\n{notice}", t!("agent_center.ui_cached_facts"))
    } else {
        notice
    }
}

fn notice_style(state: &State) -> Style {
    if state.stale || state.unresolved_notice().is_some() {
        theme::NOTICE_WARNING
    } else if state.recovery_failure().is_some() {
        theme::NOTICE_ERROR
    } else {
        match state.notice_kind {
            NoticeKind::Info => theme::DIM,
            NoticeKind::Attention => theme::NOTICE_WARNING,
            NoticeKind::Error => theme::NOTICE_ERROR,
        }
    }
}

fn rail(frame: &mut ratatui::Frame<'_>, area: Rect, state: &State) {
    if area.width == 0 {
        return;
    }
    let attention = state.attention_targets();
    let mut lines = vec![
        heading(format!(
            "{} · {}",
            t!("agent_center.title"),
            t!("agent_center.works")
        )),
        Line::raw(t!("agent_center.task_all").into_owned()),
        Line::raw(format!(
            "{}",
            if attention.is_empty() {
                t!("agent_center.ui_attention").into_owned()
            } else {
                t!("agent_center.inbox_count", count = attention.len()).into_owned()
            }
        )),
        Line::raw(t!("agent_center.task_new").into_owned()),
        Line::default(),
    ];
    let capacity = area.height.saturating_sub(6) as usize / 4;
    let selected = state
        .works
        .iter()
        .position(|view| view["work"]["id"].as_str() == state.context.work_id.as_deref())
        .unwrap_or(0);
    let first = selected.saturating_sub(capacity.saturating_sub(1));
    let width = area.width.saturating_sub(3) as usize;
    for view in state.works.iter().skip(first).take(capacity) {
        let active = view["work"]["id"].as_str() == state.context.work_id.as_deref();
        let style = if active {
            theme::SELECTED.add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let title = state.compact_goal(view);
        // Reserve the disambiguator even when a long title is clipped.
        let title = if let Some((name, number)) =
            title.rsplit_once(" [").filter(|_| title.ends_with(']'))
        {
            let suffix = format!(" [{number}");
            format!(
                "{}{}",
                clipped(name, width.saturating_sub(suffix.width())),
                suffix
            )
        } else {
            clipped(&title, width)
        };
        lines.push(Line::styled(
            format!("{} {title}", if active { ">" } else { " " }),
            style,
        ));
        lines.push(Line::styled(
            clipped(
                &state.compact_project(view["work"]["projectId"].as_str()),
                width,
            ),
            theme::DIM,
        ));
        let needs_response = attention
            .values()
            .any(|work| work.as_deref() == view["work"]["id"].as_str());
        let status = if needs_response && state.stale {
            t!(
                "agent_center.ui_last_known",
                status = t!("agent_center.ui_attention")
            )
            .into_owned()
        } else if needs_response {
            t!("agent_center.ui_attention").into_owned()
        } else {
            observed_status(state, &view["work"]["lifecycle"])
        };
        lines.push(Line::styled(
            clipped(&status, width),
            if needs_response {
                theme::NOTICE_WARNING
            } else {
                theme::DIM
            },
        ));
        lines.push(Line::default());
    }
    let block = Block::default()
        .borders(Borders::RIGHT)
        .style(theme::AGENT_TEXT.bg(theme::INPUT_BG))
        .border_style(theme::CARD_BORDER);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(lines), inner);
}

fn messages(state: &State, width: u16) -> Vec<Line<'static>> {
    state
        .view()
        .map(|view| message_lines(view, state.dashboard, width))
        .unwrap_or_default()
}

fn message_lines(view: &WorkView, global: bool, width: u16) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for message in &view.messages {
        if message["role"] == "assistant"
            && text(message, "text").is_empty()
            && message["parts"]
                .as_array()
                .is_none_or(|parts| parts.iter().all(|part| text(part, "text").is_empty()))
        {
            continue;
        }
        let role = text(message, "role").to_ascii_lowercase();
        if !global && !matches!(role.as_str(), "human" | "user" | "assistant") {
            continue;
        }
        let role = match role.as_str() {
            "human" | "user" => conversation::MessageRole::User,
            "assistant" => conversation::MessageRole::Assistant,
            "system" | "service" => conversation::MessageRole::System,
            _ => conversation::MessageRole::Unknown,
        };
        let mut body = text(message, "text").to_owned();
        if let Some(parts) = message["parts"]
            .as_array()
            .filter(|_| message["text"].as_str().is_none())
        {
            for part in parts {
                body.push_str(text(part, "text"));
            }
        }
        lines.extend(
            conversation::message_lines(
                role,
                &body,
                usize::from(width.max(1)),
                message["status"] != "Streaming",
            )
            .into_iter()
            .map(|line| {
                Line::from(
                    line.spans
                        .into_iter()
                        .map(|span| {
                            ratatui::text::Span::styled(span.content.into_owned(), span.style)
                        })
                        .collect::<Vec<_>>(),
                )
                .style(line.style)
            }),
        );
    }
    if !global {
        return lines;
    }
    let records = || {
        view.records.values().filter(|record| {
            !matches!(
                text(record, "kind"),
                "Work"
                    | "Conversation"
                    | "ConversationItem"
                    | "Task"
                    | "Attempt"
                    | "TaskDispatch"
                    | "Workspace"
                    | "IntakeRequest"
                    | "DecisionRequest"
                    | "HumanActionProposal"
            )
        })
    };
    // Following latest activity must not land on an old help panel while a
    // failure's actual message and path are clipped above it.
    for record in records()
        .filter(|record| record.get("failure").is_none())
        .chain(records().filter(|record| record.get("failure").is_some()))
    {
        section(
            &mut lines,
            &t!("agent_center.ui_recorded_facts"),
            &projection::readable(record),
        );
    }
    lines
}

fn delivery(state: &State) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let Some(view) = state.view() else {
        return lines;
    };
    if let Some(inspection) = &view.inspection {
        if let Some(page) = view.evidence.as_ref().filter(|page| {
            InspectionIdentity::from_view(inspection).as_ref() == Some(&page.inspection)
        }) {
            lines.push(heading(t!("agent_center.console_inspect")));
            section(
                &mut lines,
                text(&page.data, "relativePath"),
                &projection::readable(&page.data),
            );
        } else {
            section(
                &mut lines,
                &t!("agent_center.console_inspect"),
                &projection::readable(inspection),
            );
        }
    }
    lines
}

fn details(state: &State) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if state.demo.is_some() {
        match demo::details(state) {
            Ok(text) => append(&mut lines, &text, theme::AGENT_TEXT),
            Err(error) => append(&mut lines, &format!("{error:#}"), theme::NOTICE_WARNING),
        }
        return lines;
    }
    if state.task_list {
        if let Some(work) = state.selected_view() {
            section(
                &mut lines,
                &t!("agent_center.ui_details"),
                &projection::agreement(work),
            );
            section(
                &mut lines,
                &t!("agent_center.task_attention"),
                &projection::readable(&work["obligations"]),
            );
            if let Some(project) = state
                .projects
                .iter()
                .find(|project| project["id"] == work["work"]["projectId"])
            {
                section(
                    &mut lines,
                    &t!("agent_center.ui_source_root"),
                    text(project, "root"),
                );
            }
        }
        return lines;
    }
    section(
        &mut lines,
        &t!("agent_center.ui_recorded_facts"),
        &notice(state),
    );
    if state.stale {
        append(
            &mut lines,
            &t!("agent_center.console_stale"),
            Style::default(),
        );
    }
    if let Some(view) = state.view() {
        for record in view.records.values().filter(|record| {
            record["kind"] != "Conversation"
                && (record["kind"] != "ConversationItem"
                    || !matches!(text(record, "role"), "human" | "user" | "assistant"))
        }) {
            section(
                &mut lines,
                &t!("agent_center.ui_recorded_facts"),
                &projection::readable(record),
            );
        }
    }
    if let Some(work) = state.selected_view() {
        section(
            &mut lines,
            &task_tab::continuation_label(work),
            &projection::readable(&work["continuation"]),
        );
        if let Some(failure) = work.pointer("/continuation/recoveryFailure") {
            section(
                &mut lines,
                &t!("agent_center.status_error"),
                &projection::readable(failure),
            );
        }
        section(
            &mut lines,
            &t!("agent_center.ui_details"),
            &projection::agreement(work),
        );
        section(
            &mut lines,
            &t!("agent_center.ui_attention"),
            &projection::readable(&work["obligations"]),
        );
    }
    if let Some(project) = state
        .projects
        .iter()
        .find(|project| project["id"].as_str() == state.context.project_id.as_deref())
    {
        section(
            &mut lines,
            &t!("agent_center.ui_source_root"),
            text(project, "root"),
        );
    }
    let root = state
        .view()
        .and_then(|view| {
            view.records
                .values()
                .find(|record| {
                    record["kind"] == "Workspace"
                        && record["workId"].as_str() == state.context.work_id.as_deref()
                })
                .and_then(|record| record["localRoot"].as_str())
        })
        .or_else(|| {
            state.selected_view().and_then(|work| {
                work.pointer("/candidate/destination/localRoot")
                    .and_then(Value::as_str)
            })
        });
    section(
        &mut lines,
        &t!("agent_center.ui_execution_root"),
        root.filter(|root| !root.is_empty())
            .unwrap_or(&t!("agent_center.ui_location_unknown")),
    );
    lines.extend(delivery(state));
    if let Some(inspection) = state.view().and_then(|view| view.inspection.as_ref()) {
        section(
            &mut lines,
            &t!("agent_center.console_delivery"),
            &projection::readable(inspection),
        );
    }
    lines
}

fn diagnostic(state: &State) -> String {
    if let Some(pending) = &state.pending {
        let mut value = match pending.preview.clone() {
            Value::Object(object) => object,
            preview => serde_json::Map::from_iter([("preview".into(), preview)]),
        };
        let duplicate_authority = value
            .get("humanActionProposal")
            .and_then(|proposal| proposal.get("preview"))
            .and_then(Value::as_object)
            .is_some_and(|preview| {
                value.len() == preview.len() + 1
                    && preview
                        .iter()
                        .all(|(key, item)| value.get(key) == Some(item))
            });
        if duplicate_authority {
            if let Some(proposal) = value
                .get_mut("humanActionProposal")
                .and_then(Value::as_object_mut)
            {
                proposal.remove("preview");
            }
        }
        if value.get("request").is_none()
            && value.get("method").is_none()
            && value.get("operation").is_none()
        {
            value.insert("request".into(), operation_preview(&pending.operation));
        }
        // Soft wrapping can discard literal whitespace. This copy stays lossless.
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let request = operation_preview(&pending.operation).to_string();
        let mut encoded = String::with_capacity(request.len() * 2);
        for byte in request.bytes() {
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 15)]));
        }
        let mut diagnostic =
            serde_json::Map::from_iter([("requestUtf8Hex".into(), json!(encoded))]);
        diagnostic.extend(value.into_iter().filter(|(key, _)| key != "requestUtf8Hex"));
        return pretty(&Value::Object(diagnostic));
    }
    let work = state
        .view()
        .map(|view| {
            view.transcript
                .iter()
                .chain(view.messages.iter())
                .map(|value| {
                    value
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| pretty(value))
                })
                .collect::<Vec<_>>()
                .join("\n\n")
        })
        .unwrap_or_default();
    if state.context.global_conversation {
        format!(
            "{}\n\n{work}",
            state
                .chat
                .transcript
                .iter()
                .chain(state.chat.messages.iter())
                .map(pretty)
                .collect::<Vec<_>>()
                .join("\n\n")
        )
    } else {
        work
    }
}

fn max_scroll(paragraph: &Paragraph<'_>, area: Rect) -> u16 {
    paragraph
        .line_count(area.width.max(1))
        .saturating_sub(area.height as usize)
        .min(u16::MAX as usize) as u16
}

fn read_panel(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    lines: Vec<Line<'static>>,
    state: &mut State,
    detail: bool,
) {
    if area.is_empty() {
        return;
    }
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    let max = max_scroll(&paragraph, area);
    let view = if detail {
        state.view_mut()
    } else {
        state.reading_view_mut()
    };
    let scroll = if detail {
        view.details_scroll = view.details_scroll.min(max);
        view.details_scroll
    } else if view.show_delivery {
        view.delivery_scroll = view.delivery_scroll.min(max);
        view.delivery_scroll
    } else {
        view.scroll = if view.follow {
            max
        } else {
            view.scroll.min(max)
        };
        view.scroll
    };
    frame.render_widget(paragraph.scroll((scroll, 0)), area);
}

fn action_lines(state: &State, menu: &Menu, compact: bool) -> (Vec<Line<'static>>, usize, usize) {
    let mut lines = Vec::new();
    let mut selected = (0, 0);
    for (index, (label, choice)) in menu.items.iter().enumerate() {
        let start = lines.len();
        let focused =
            (state.focus == Focus::Actions || state.menu.is_some()) && menu.selected == index;
        let label = if compact {
            state.action_label(label, choice)
        } else {
            label.clone()
        };
        lines.push(Line::styled(
            format!("{} {label}", if focused { ">" } else { " " }),
            if focused {
                theme::SELECTED.add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            },
        ));
        if compact {
            if let Choice::SelectWork(id) = choice {
                if let Some(view) = state.works.iter().find(|view| view["work"]["id"] == *id) {
                    append(
                        &mut lines,
                        &observed_status(state, &view["work"]["lifecycle"]),
                        theme::DIM,
                    );
                    if !state.stale {
                        append(&mut lines, &projection::next(view), theme::DIM);
                    }
                    let mut reasons = BTreeSet::new();
                    for item in state
                        .inbox
                        .iter()
                        .filter(|item| item["workId"] == *id)
                        .chain(view["obligations"].as_array().into_iter().flatten())
                    {
                        let reason = projection::readable(item);
                        if !reason.is_empty() && reasons.insert(reason.clone()) {
                            append(&mut lines, &reason, theme::NOTICE_WARNING);
                        }
                        if reasons.len() == 2 {
                            break;
                        }
                    }
                    lines.push(Line::default());
                }
            }
        }
        if menu.selected == index {
            selected = (start, lines.len());
        }
    }
    (lines, selected.0, selected.1)
}

fn action_panel(
    frame: &mut ratatui::Frame<'_>,
    mut area: Rect,
    state: &mut State,
    menu: &Menu,
    overlay: bool,
) {
    if area.is_empty() {
        return;
    }
    frame.render_widget(
        Block::default().style(theme::AGENT_TEXT.bg(theme::INPUT_BG)),
        area,
    );
    let title = if overlay && state.demo.is_some() {
        demo::menu_title(state, menu)
    } else if overlay {
        t!("agent_center.console_menu_help").into_owned()
    } else if state.context.global_conversation && !state.dashboard
        || !state.showing_work() && state.home_screen == HomeScreen::Attention
    {
        format!("F6 · {}", t!("agent_center.ui_attention"))
    } else {
        format!("F6 · {}", t!("agent_center.ui_actions"))
    };
    frame.render_widget(Paragraph::new(heading(title)), rows(&mut area, 1));
    let (lines, start, end) = action_lines(state, menu, !overlay);
    if lines.is_empty() {
        frame.render_widget(
            Paragraph::new(if state.stale {
                t!("agent_center.ui_cached_facts").into_owned()
            } else if state.busy {
                t!("agent_center.sending").into_owned()
            } else {
                t!("agent_center.ui_empty_attention").into_owned()
            })
            .wrap(Wrap { trim: false }),
            area,
        );
        return;
    }
    let width = area.width.max(1);
    let first = Paragraph::new(Text::from(lines[..start].to_vec()))
        .wrap(Wrap { trim: false })
        .line_count(width);
    let last = Paragraph::new(Text::from(lines[..end].to_vec()))
        .wrap(Wrap { trim: false })
        .line_count(width);
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    let scroll = if overlay || state.focus == Focus::Actions || menu.selected > 0 {
        // Show the beginning of an over-height item, rather than clipping its
        // selected label in favor of the trailing description.
        first
            .min(last.saturating_sub(area.height as usize))
            .min(u16::MAX as usize) as u16
    } else {
        0
    };
    frame.render_widget(paragraph.scroll((scroll, 0)), area);
}

fn modal(frame: &mut ratatui::Frame<'_>, area: Rect, state: &mut State) {
    if state.demo.is_some() {
        if let Some(menu) = &state.menu {
            frame.render_widget(Clear, area);
            let (lines, start, end) = action_lines(state, menu, false);
            let width = area.width.min(78);
            let text_width = width.saturating_sub(2).max(1);
            let paragraph = Paragraph::new(lines.clone()).wrap(Wrap { trim: false });
            let height = paragraph
                .line_count(text_width)
                .saturating_add(2)
                .min(usize::from(area.height)) as u16;
            let panel = Rect::new(area.x + (area.width - width) / 2, area.y, width, height);
            let block = Block::default()
                .borders(Borders::ALL)
                .style(theme::AGENT_TEXT.bg(theme::INPUT_BG))
                .border_style(theme::CARD_BORDER);
            let inner = block.inner(panel);
            frame.render_widget(block, panel);
            let first = Paragraph::new(lines[..start].to_vec())
                .wrap(Wrap { trim: false })
                .line_count(text_width);
            let last = Paragraph::new(lines[..end].to_vec())
                .wrap(Wrap { trim: false })
                .line_count(text_width);
            let scroll = first
                .min(last.saturating_sub(usize::from(inner.height)))
                .min(usize::from(u16::MAX)) as u16;
            frame.render_widget(paragraph.scroll((scroll, 0)), inner);
            return;
        }
    }
    if let Some(menu) = state.menu.take() {
        // Keep menu focus explicit while drawing its selection.
        state.focus = Focus::Actions;
        action_panel(frame, area, state, &menu, true);
        state.focus = Focus::Composer;
        state.menu = Some(menu);
        return;
    }
    let title = if state.diagnostics {
        t!("agent_center.console_diagnostics")
    } else if state.inline_approval() {
        t!("agent_center.approval_title")
    } else if state.pending.is_some() {
        t!("agent_center.confirm_title")
    } else {
        t!("agent_center.console_answer")
    };
    let block = card::block(theme::CARD_BORDER)
        .title_style(theme::SYSTEM_TEXT)
        .title(title.into_owned());
    let inner = block.inner(area);
    if state.pending.is_none() && !state.diagnostics {
        let target = state
            .form
            .as_ref()
            .map(|form| state.work_label(form.work.as_deref()));
        if let Some((form, target)) = state.form.as_mut().zip(target.as_deref()) {
            let scroll = form.viewport(target, inner.width, inner.height);
            let (layout, focus) = form.layout(target, inner.width);
            let (row, column) = layout.cursor(focus);
            frame.render_widget(block, area);
            frame.render_widget(Paragraph::new(layout.lines).scroll((scroll, 0)), inner);
            if form.fields[form.selected].choices.is_empty()
                && !inner.is_empty()
                && row >= scroll as usize
                && row - (scroll as usize) < inner.height as usize
            {
                frame.set_cursor_position((
                    inner.x + column as u16,
                    inner.y + (row - scroll as usize) as u16,
                ));
            }
            return;
        }
    }
    let body = if state.diagnostics {
        diagnostic(state)
    } else if let Some(pending) = &state.pending {
        let body =
            projection::confirmation(&pending.preview, &pending.operation, &pending.target_label);
        let authority = workflow::proposal_authority_details(&pending.preview);
        if authority.is_empty() {
            body
        } else {
            format!("{body}\n\n{authority}")
        }
    } else {
        String::new()
    };
    let paragraph = Paragraph::new(body).wrap(Wrap { trim: false });
    let max = max_scroll(&paragraph, inner);
    let scroll = if let Some(pending) = state.pending.as_mut() {
        pending.scroll = pending.scroll.min(max);
        pending.scroll
    } else {
        let view = state.view_mut();
        view.scroll = if view.follow {
            max
        } else {
            view.scroll.min(max)
        };
        view.scroll
    };
    frame.render_widget(block, area);
    frame.render_widget(paragraph.scroll((scroll, 0)), inner);
}

fn body(frame: &mut ratatui::Frame<'_>, mut area: Rect, state: &mut State) {
    if area.is_empty() {
        return;
    }
    let next = if state.stale {
        t!("agent_center.ui_cached_facts").into_owned()
    } else if state.unresolved_notice().is_some() {
        t!("agent_center.console_unknown").into_owned()
    } else if state
        .view()
        .is_some_and(|view| view.records.values().any(open_question))
    {
        t!("agent_center.console_next_attention").into_owned()
    } else if let Some(view) = state.selected_view().filter(|_| state.showing_work()) {
        projection::next(view)
    } else if state.showing_work() {
        t!("agent_center.console_unknown").into_owned()
    } else if state.home_screen == HomeScreen::Overview {
        t!("agent_center.ui_new_goal").into_owned()
    } else {
        t!("agent_center.ui_attention").into_owned()
    };
    let mut next_lines = vec![heading(t!("agent_center.ui_next")), Line::raw(next)];
    if state.view().is_some_and(|view| view.show_delivery) {
        next_lines.push(Line::raw(format!(
            "Esc · {}",
            t!(
                "agent_center.console_return",
                work = state.compact_target(state.context.work_id.as_deref())
            )
        )));
    }
    if let Some(tasks) = state
        .selected_view()
        .filter(|_| state.showing_work())
        .and_then(|view| view["taskSummaries"].as_array())
        .filter(|tasks| !tasks.is_empty())
    {
        next_lines.push(heading(t!("agent_center.console_progress")));
        for task in tasks.iter().take(2) {
            next_lines.push(Line::raw(format!(
                "{} · {}",
                text(task, "objective"),
                observed_status(
                    state,
                    task.get("state")
                        .or_else(|| task.get("status"))
                        .unwrap_or(&Value::Null)
                )
            )));
        }
    }
    let next = Paragraph::new(next_lines).wrap(Wrap { trim: false });
    let height = (next.line_count(area.width.max(1)) as u16)
        .min(6)
        .min(area.height / 3);
    frame.render_widget(next, rows(&mut area, height));
    let owned_menu;
    let menu = if let Some(menu) = state.cards.take() {
        owned_menu = menu;
        &owned_menu
    } else {
        owned_menu = state.visible_actions();
        &owned_menu
    };
    let mut conversation = if state.context.global_conversation {
        Vec::new()
    } else {
        messages(state, area.width)
    };
    let inspected = delivery(state);
    if state.view().is_some_and(|view| view.show_delivery) && !inspected.is_empty() {
        conversation = inspected;
    } else if let Some(work) = state.selected_view().filter(|view| {
        state.showing_work()
            && (state.context.global_conversation || view["work"]["lifecycle"] == "Draft")
    }) {
        let mut brief = Vec::new();
        section(
            &mut brief,
            &t!("agent_center.ui_details"),
            &projection::agreement(work),
        );
        if !conversation.is_empty() {
            brief.push(Line::default());
            brief.extend(conversation);
        }
        conversation = brief;
    }
    if state.showing_work() && conversation.is_empty() {
        conversation.push(Line::styled(
            if state.stale || state.busy || state.selected_view().is_none() {
                t!("agent_center.console_unknown").into_owned()
            } else {
                t!("agent_center.ui_no_conversation").into_owned()
            },
            theme::DIM,
        ));
    } else if state.works.is_empty() && conversation.is_empty() {
        conversation.push(Line::raw(if state.stale {
            t!("agent_center.ui_cached_facts").into_owned()
        } else if state.busy {
            t!("agent_center.sending").into_owned()
        } else {
            t!("agent_center.ui_no_works").into_owned()
        }));
    }
    let action_height = if !state.showing_work() {
        if conversation.is_empty() {
            area.height
        } else {
            area.height.saturating_sub((area.height / 2).max(1))
        }
    } else if menu.items.is_empty() {
        0
    } else {
        (area.height / 3).max(2).min(8).min(area.height)
    };
    let content_height = area.height.saturating_sub(action_height);
    let content = rows(&mut area, content_height);
    read_panel(frame, content, conversation, state, false);
    action_panel(frame, area, state, menu, false);
    if state.focus == Focus::Actions {
        state.cards = Some(owned_menu);
    }
}

fn approval_buttons(
    frame: &mut ratatui::Frame<'_>,
    mut area: Rect,
    method: &str,
    selected: Option<usize>,
) {
    let labels = [
        if method == "work.start" {
            t!("agent_center.approval_start")
        } else if method == "work.continue" {
            t!("agent_center.task_continue")
        } else if method == "work.claim_executor" {
            t!("agent_center.task_claim_executor")
        } else {
            t!("agent_center.approval_accept")
        },
        t!("agent_center.approval_edit"),
        t!("agent_center.approval_defer"),
    ];
    for (index, label) in labels.into_iter().enumerate() {
        card::render_buttons(
            frame,
            rows(&mut area, 1),
            &[format!("[{label}]")],
            (selected == Some(index)).then_some(0),
        );
    }
}

fn inline_approval(frame: &mut ratatui::Frame<'_>, mut area: Rect, state: &mut State) {
    let chat_height = if state.approval_expanded {
        0
    } else {
        area.height / 3
    };
    let chat = rows(&mut area, chat_height);
    read_panel(
        frame,
        chat,
        state
            .editor_view()
            .map(|view| message_lines(view, state.context.global_conversation, chat.width))
            .unwrap_or_default(),
        state,
        false,
    );
    let preview_height = area.height.saturating_sub(3);
    let preview = rows(&mut area, preview_height);
    modal(frame, preview, state);
    if let Some(pending) = &state.pending {
        approval_buttons(
            frame,
            area,
            &pending.operation.method,
            Some(state.approval_choice),
        );
    }
}

fn approval_offer(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    state: &State,
    proposal: &Value,
    menu: &Menu,
) {
    let title = format!(
        "{} {}/{} · {}",
        t!("agent_center.approval_title"),
        menu.selected + 1,
        menu.items.len(),
        if state.focus == Focus::Actions {
            t!("agent_center.ui_actions_hint")
        } else {
            t!("agent_center.approval_open_hint")
        }
    );
    let block = card::block(theme::CARD_BORDER);
    let mut area = if area.width >= card::CARD_MIN_SIZE && area.height >= card::CARD_MIN_SIZE {
        let inner = block.inner(area);
        frame.render_widget(block, area);
        inner
    } else {
        area
    };
    frame.render_widget(Paragraph::new(heading(title)), rows(&mut area, 1));
    let facts_height = area.height.saturating_sub(3);
    let facts = rows(&mut area, facts_height);
    frame.render_widget(
        Paragraph::new(format!(
            "{}\n{}\n{}",
            super::workflow::method_label(
                text(&proposal["request"], "method"),
                &proposal["request"]["params"],
            ),
            state.human_proposal_target(proposal),
            text(&proposal["preview"]["project"], "root")
        ))
        .wrap(Wrap { trim: false }),
        facts,
    );
    approval_buttons(frame, area, text(&proposal["request"], "method"), None);
}

fn global_chat(frame: &mut ratatui::Frame<'_>, mut area: Rect, state: &mut State) {
    if area.is_empty() {
        return;
    }
    if state.demo.is_none() && !state.context.global_conversation && area.height >= 4 {
        let next = if state.stale {
            t!("agent_center.ui_cached_facts").into_owned()
        } else if state.unresolved_notice().is_some() {
            format!("F4 · {}", t!("setup.option.retry_detection"))
        } else if state.can_claim_executor() {
            format!("F4 · {}", t!("agent_center.task_claim_executor"))
        } else if state
            .view()
            .is_some_and(|view| view.records.values().any(open_question))
        {
            format!("F6 · {}", t!("agent_center.console_answer"))
        } else if let Some(view) = state.selected_view() {
            match text(&view["continuation"], "state") {
                "NeedsRecovery" | "Paused" | "Ready" => {
                    format!("F4 · {}", t!("agent_center.task_continue"))
                }
                "Unavailable" if view["continuation"]["canRestartSession"] == true => {
                    format!("F4 · {}", t!("agent_center.task_new_session"))
                }
                "Unavailable" => format!("F5 · {}", t!("agent_center.ui_details")),
                "WaitingForInput" => t!("agent_center.console_next_attention").into_owned(),
                _ => projection::next(view),
            }
        } else {
            t!("agent_center.console_unknown").into_owned()
        };
        frame.render_widget(
            Paragraph::new(clipped(
                &format!("{} · {next}", t!("agent_center.ui_next")),
                usize::from(area.width),
            ))
            .style(theme::DIM),
            rows(&mut area, 1),
        );
    }
    let menu = state
        .cards
        .take()
        .unwrap_or_else(|| state.visible_actions());
    let proposal = menu
        .items
        .get(menu.selected)
        .and_then(|(_, choice)| match choice {
            Choice::HumanProposal(proposal) => Some(proposal),
            _ => None,
        });
    let card_height = if menu.items.is_empty() {
        0
    } else if proposal.is_some() {
        area.height.saturating_sub(3).min(8)
    } else {
        (area.height / 3).max(2).min(8).min(area.height)
    };
    let content_height = area.height.saturating_sub(card_height);
    let content = rows(&mut area, content_height);
    let mut lines = state
        .editor_view()
        .map(|view| message_lines(view, state.context.global_conversation, content.width))
        .unwrap_or_default();
    if lines.is_empty() {
        lines.push(Line::styled(
            t!("agent_center.chat_welcome").into_owned(),
            theme::DIM,
        ));
    }
    read_panel(frame, content, lines, state, false);
    if card_height > 0 {
        if let Some(proposal) = proposal {
            approval_offer(frame, area, state, proposal, &menu);
        } else {
            action_panel(frame, area, state, &menu, false);
        }
    }
    if state.focus == Focus::Actions {
        state.cards = Some(menu);
    }
}

fn task_list(frame: &mut ratatui::Frame<'_>, area: Rect, state: &mut State) {
    if state.demo.is_some() {
        demo_work_list(frame, area, state);
        return;
    }
    state.task_index = state.task_index.min(state.tasks().len().saturating_sub(1));
    let tasks = state.tasks();
    let capacity = usize::from(area.height / 4).max(1);
    let first = state.task_index.saturating_sub(capacity - 1);
    let mut lines = Vec::new();
    for (index, task) in tasks.iter().enumerate().skip(first).take(capacity) {
        let selected = index == state.task_index;
        lines.push(Line::styled(
            format!(
                "{} {}",
                if selected { "›" } else { " " },
                state.compact_goal(task)
            ),
            if selected {
                theme::SELECTED
            } else {
                theme::AGENT_TEXT
            },
        ));
        let status = task_tab::continuation_label(task);
        lines.push(Line::styled(
            if state.stale {
                t!("agent_center.ui_last_known", status = status).into_owned()
            } else {
                status
            },
            theme::DIM,
        ));
        let updated = task["continuation"]["updatedAt"]
            .as_str()
            .or_else(|| task["work"]["updatedAt"].as_str())
            .or_else(|| task["work"]["createdAt"].as_str())
            .unwrap_or("");
        lines.push(Line::styled(
            t!("agent_center.task_updated", time = updated).into_owned(),
            theme::DIM,
        ));
        lines.push(Line::default());
    }
    if tasks.is_empty() {
        lines.push(Line::styled(
            t!("agent_center.ui_no_works").into_owned(),
            theme::DIM,
        ));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

fn demo_card_fields(work: &Value) -> [(&'static str, String); 6] {
    let work = &work["demo"];
    let first = |key: &str| {
        work[key]
            .as_array()
            .and_then(|values| values.first())
            .and_then(Value::as_str)
            .unwrap_or("")
    };
    let status = match text(work, "status") {
        "Compatibility test failed" => "Compatibility failed",
        "Compatibility recheck queued" => "Recheck queued",
        "Compatibility failure acknowledged" => "Failure acknowledged",
        other => other,
    };
    let steps = work["steps"].as_array();
    let total = steps.map_or(0, Vec::len);
    let complete = steps.map_or(0, |steps| {
        steps
            .iter()
            .filter(|step| step["status"] == "completed")
            .count()
    });
    let blockers = match first("blockers") {
        "" => "None",
        value if value.starts_with("Compatibility test failed") => "Compatibility failed",
        value if value.starts_with("Budget reached") => "Token cap reached",
        value => value,
    };
    let decisions = match first("decisions") {
        "" => "None",
        value if value.starts_with("B:") => "B: Fix compatibility",
        value if value.starts_with("A:") => "A: Acknowledge failure",
        value if value.starts_with("C:") => "C: Review rollback",
        value => value,
    };
    let deliverables = match first("deliverables") {
        "" => "None",
        value if value.starts_with("Recorded sample patch") => "Patch + test results",
        value if value.starts_with("Versioned schema 2.3") => "Schema 2.3 + notes",
        value => value,
    };
    [
        ("Status", status.into()),
        ("Progress", format!("{complete}/{total} checks")),
        ("Blockers", blockers.into()),
        ("Decisions", decisions.into()),
        ("Deliverables", deliverables.into()),
        ("Acceptance", "Pending".into()),
    ]
}

fn demo_work_list(frame: &mut ratatui::Frame<'_>, area: Rect, state: &mut State) {
    state.task_index = state.task_index.min(state.works.len().saturating_sub(1));
    let mut lines = Vec::new();
    let mut selected_start = 0;
    let mut selected_end = 0;
    for (index, work) in state.works.iter().enumerate() {
        let selected = index == state.task_index;
        if selected {
            selected_start = Paragraph::new(lines.clone())
                .wrap(Wrap { trim: false })
                .line_count(area.width.max(1));
        }
        let mut title = format!(
            "{} {}",
            if selected { ">" } else { " " },
            state.compact_goal(work)
        );
        if let Some(parent) = work["demo"]["parentWorkId"].as_str() {
            let parent = state
                .works
                .iter()
                .find(|work| work["work"]["id"] == parent)
                .map(|work| state.compact_goal(work))
                .unwrap_or_else(|| parent.to_owned());
            title.push_str(&format!("  (from {parent})"));
        }
        lines.push(Line::styled(
            title,
            if selected {
                theme::SELECTED
            } else {
                theme::AGENT_TEXT.add_modifier(Modifier::BOLD)
            },
        ));
        let fields = demo_card_fields(work);
        if area.width >= 72 {
            let width = usize::from(area.width.saturating_sub(4) / 2);
            for pair in fields.chunks(2) {
                let left = clipped(&format!("{}: {}", pair[0].0, pair[0].1), width);
                let right = clipped(&format!("{}: {}", pair[1].0, pair[1].1), width);
                let padding = width.saturating_sub(left.width());
                lines.push(Line::from(vec![
                    Span::raw(format!("  {left}{}", " ".repeat(padding + 2))),
                    Span::raw(right),
                ]));
            }
        } else {
            for (label, value) in fields {
                lines.push(Line::from(format!("  {label}: {value}")));
            }
        }
        if selected {
            selected_end = Paragraph::new(lines.clone())
                .wrap(Wrap { trim: false })
                .line_count(area.width.max(1));
        }
        lines.push(Line::default());
    }
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    let Some(demo) = state.demo.as_mut() else {
        return;
    };
    let height = usize::from(area.height);
    if demo.follow_selection {
        let offset = usize::from(demo.overview_scroll);
        if selected_start < offset || selected_end.saturating_sub(selected_start) > height {
            demo.overview_scroll = selected_start.min(usize::from(u16::MAX)) as u16;
        } else if selected_end > offset + height {
            demo.overview_scroll = selected_end
                .saturating_sub(height)
                .min(usize::from(u16::MAX)) as u16;
        }
        demo.follow_selection = false;
    }
    demo.overview_scroll = demo.overview_scroll.min(max_scroll(&paragraph, area));
    frame.render_widget(paragraph.scroll((demo.overview_scroll, 0)), area);
}

fn legacy_sessions(frame: &mut ratatui::Frame<'_>, mut area: Rect, state: &mut State) {
    let Some(demo) = &state.demo else { return };
    let tabs = demo::LEGACY_TABS
        .iter()
        .enumerate()
        .map(|(index, title)| {
            Span::styled(
                if index == demo.legacy_index {
                    format!(" [{title}] ")
                } else {
                    format!("  {title}  ")
                },
                if index == demo.legacy_index {
                    theme::SELECTED
                } else {
                    theme::DIM
                },
            )
        })
        .collect::<Vec<_>>();
    let paragraph = Paragraph::new(Line::from(tabs)).wrap(Wrap { trim: false });
    let height = paragraph
        .line_count(area.width.max(1))
        .min(usize::from(area.height)) as u16;
    frame.render_widget(paragraph, rows(&mut area, height));
    rows(&mut area, 1);
    global_chat(frame, area, state);
}

pub(super) fn render(frame: &mut ratatui::Frame<'_>, state: &mut State) {
    if state
        .demo
        .as_ref()
        .is_some_and(|demo| demo.scenario.release_journey.is_some())
    {
        demo::render_release_journey(frame, state);
        return;
    }
    if state
        .demo
        .as_ref()
        .is_some_and(|demo| demo.scenario.work_board.is_some())
    {
        demo::render_work_board(frame, state);
        return;
    }
    if state
        .demo
        .as_ref()
        .is_some_and(|demo| demo.scenario.work_graph.is_some())
    {
        demo::render_work_graph(frame, state);
        return;
    }
    if state
        .demo
        .as_ref()
        .is_some_and(|demo| demo.scenario.is_work_model())
    {
        demo::render_work_model(frame, state);
        return;
    }
    if state.demo.is_some() {
        frame.render_widget(Clear, frame.area());
    }
    frame.render_widget(
        Block::default().style(theme::AGENT_TEXT.bg(theme::INPUT_BG)),
        frame.area(),
    );
    let regions = regions(state, frame.area());
    rail(frame, regions.rail, state);
    let chat_width = regions.navigation.width / 2;
    let chat_area = Rect::new(
        regions.navigation.x,
        regions.navigation.y,
        chat_width,
        regions.navigation.height,
    );
    let tasks_area = Rect::new(
        chat_area.right(),
        regions.navigation.y,
        regions.navigation.width - chat_width,
        regions.navigation.height,
    );
    let is_global_chat = state.context.global_conversation && !state.task_list && !state.dashboard;
    let navigation_disabled = state.pending.is_some() || state.form.is_some();
    for (area, label, selected) in [
        (
            chat_area,
            format!("F1 {}", t!("agent_center.chat_global")),
            is_global_chat,
        ),
        (
            tasks_area,
            format!(
                "F2 {}",
                if state.demo.is_some() {
                    "Work Overview".into()
                } else {
                    t!("agent_center.tasks_title").into_owned()
                }
            ),
            !is_global_chat,
        ),
    ] {
        frame.render_widget(
            Paragraph::new(clipped(&label, usize::from(area.width))).style(
                if navigation_disabled {
                    theme::DIM
                } else if selected && !(state.demo.is_some() && state.menu.is_some()) {
                    theme::SELECTED
                } else {
                    theme::AGENT_TEXT
                },
            ),
            area,
        );
    }
    let title = if let Some(menu) = state.menu.as_ref().filter(|_| state.demo.is_some()) {
        demo::menu_title(state, menu)
    } else if state.demo.is_some() && state.view().is_some_and(|view| view.details_open) {
        let owner = state
            .selected_view()
            .map(|view| state.compact_goal(view))
            .unwrap_or_else(|| "Global conversation".into());
        format!("{owner} | Evidence")
    } else if let Some(demo) = state.demo.as_ref().filter(|demo| demo.legacy) {
        format!(
            "Historical agent sessions | {}",
            demo::LEGACY_TABS[demo.legacy_index]
        )
    } else if state.task_list {
        if state.demo.is_some() {
            "Work Overview".into()
        } else {
            t!("agent_center.tasks_title").into_owned()
        }
    } else {
        state
            .selected_view()
            .filter(|_| {
                state.showing_work() && (!state.context.global_conversation || state.dashboard)
            })
            .map(|view| {
                let title = state.compact_goal(view);
                if state.demo.is_some() {
                    format!("{title} | Work chat")
                } else {
                    title
                }
            })
            .unwrap_or_else(|| {
                if state.context.global_conversation && !state.dashboard {
                    t!("agent_center.chat_global").into_owned()
                } else if state.showing_work() {
                    t!("agent_center.console_unknown_work").into_owned()
                } else if state.home_screen == HomeScreen::Attention {
                    t!("agent_center.ui_attention").into_owned()
                } else {
                    t!("agent_center.ui_overview").into_owned()
                }
            })
    };
    let status = state
        .selected_view()
        .filter(|_| state.showing_work())
        .map(|view| {
            let status = task_tab::continuation_label(view);
            if state.stale {
                t!("agent_center.ui_last_known", status = status).into_owned()
            } else {
                status
            }
        })
        .unwrap_or_default();
    let subtitle = if let Some(demo) = &state.demo {
        let scene =
            if state.task_list && demo.scenario.works.len() == 3 && demo.scenario.budget.reached {
                8
            } else if demo.scenario.resumed
                && demo.scenario.exchange.is_none()
                && !demo.scenario.branch_confirmation_pending
                && state.view().is_some_and(|view| view.details_open)
            {
                3
            } else {
                demo.scenario.scene
            };
        format!(
            "Work story demo | Simulated data | {}",
            t!("agent_center.demo_scene", scene = scene)
        )
    } else if state.task_list {
        format!(
            "{} · {}",
            if state.home_screen == HomeScreen::Attention {
                t!("agent_center.task_attention")
            } else {
                t!("agent_center.task_all")
            },
            state.tasks().len()
        )
    } else if state.context.global_conversation {
        let hint = if state.context.work_id.is_some() {
            Some(state.compact_target(state.context.work_id.as_deref()))
        } else {
            state
                .context
                .project_id
                .as_deref()
                .map(|id| state.compact_project(Some(id)))
        };
        hint.map(|hint| t!("agent_center.chat_context_hint", context = hint).into_owned())
            .unwrap_or_default()
    } else {
        format!(
            "{status} · {}",
            state.compact_project(state.context.project_id.as_deref())
        )
    };
    frame.render_widget(
        Paragraph::new(vec![
            heading(clipped(
                &title.replace(['\r', '\n'], " "),
                usize::from(regions.header.width),
            )),
            Line::styled(subtitle, theme::DIM),
        ]),
        regions.header,
    );
    frame.render_widget(
        Paragraph::new(notice(state))
            .wrap(Wrap { trim: false })
            .style(notice_style(state)),
        regions.notice,
    );
    if let Some(attention) =
        demo::banner(state).or_else(|| state.attention_summary().filter(|_| !state.task_list))
    {
        frame.render_widget(
            Paragraph::new(attention)
                .wrap(Wrap { trim: false })
                .style(theme::NOTICE_WARNING.add_modifier(Modifier::BOLD)),
            regions.attention,
        );
    }
    if state.demo.as_ref().is_some_and(|demo| demo.legacy) {
        legacy_sessions(frame, regions.body, state);
    } else if state.inline_approval() {
        inline_approval(frame, regions.body, state);
    } else if state.pending.is_some()
        || state.form.is_some()
        || state.menu.is_some()
        || state.diagnostics
    {
        modal(frame, regions.body, state);
    } else if state.view().is_some_and(|view| view.details_open) {
        let mut content = regions.body;
        if state.demo.is_none() && frame.area().width >= 132 {
            let detail = Rect::new(
                content.right().saturating_sub(34),
                content.y,
                34,
                content.height,
            );
            content.width = content.width.saturating_sub(35);
            if !state.task_list && !state.dashboard {
                global_chat(frame, content, state);
            } else if state.task_list && state.demo.is_some() {
                task_list(frame, content, state);
            } else {
                body(frame, content, state);
            }
            frame.render_widget(
                Block::default()
                    .borders(Borders::LEFT)
                    .style(theme::AGENT_TEXT.bg(theme::INPUT_BG))
                    .border_style(theme::CARD_BORDER),
                Rect::new(detail.x - 1, detail.y, detail.width + 1, detail.height),
            );
            let lines = details(state);
            read_panel(frame, detail, lines, state, true);
        } else {
            frame.render_widget(
                Block::default().style(theme::AGENT_TEXT.bg(theme::INPUT_BG)),
                content,
            );
            let lines = details(state);
            read_panel(frame, content, lines, state, true);
        }
    } else if state.task_list {
        task_list(frame, regions.body, state);
    } else if !state.dashboard && !state.view().is_some_and(|view| view.show_delivery) {
        global_chat(frame, regions.body, state);
    } else {
        body(frame, regions.body, state);
    }
    let scope = if let Some(demo) = &state.demo {
        demo::next_hint(&demo.scenario)
    } else if state.context.global_conversation {
        t!("agent_center.chat_global").into_owned()
    } else if state.context.work_id.is_some() {
        String::new()
    } else {
        t!(
            "agent_center.ui_composer_new",
            project = state.compact_project(state.context.project_id.as_deref())
        )
        .into_owned()
    };
    let queued = (!state.context.global_conversation && !state.task_list && !state.stale)
        .then(|| state.selected_view())
        .flatten()
        .and_then(|view| view["continuation"]["pendingInputCount"].as_u64())
        .filter(|count| *count > 0)
        .map(|count| t!("agent_center.task_queued_messages", count = count).into_owned());
    if state.thinking() {
        let mut spans =
            crate::ui::shimmer::shimmer_spans(&t!("chat.activity_thinking"), state.activity_frame);
        if let Some(queued) = queued {
            spans.push(Span::styled(format!(" · {queued}"), theme::DIM));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), regions.scope);
    } else {
        frame.render_widget(
            Paragraph::new(queued.unwrap_or(scope)).style(theme::DIM),
            regions.scope,
        );
    }
    let focused = state.composer_focused();
    let placeholder = if state.stale {
        t!("input.placeholder.disconnected")
    } else {
        t!("input.placeholder.connected")
    };
    let view = state.editor_view_mut();
    let layout = view.editor.layout(&view.draft);
    let (row, column) = layout.cursor(view.editor.cursor);
    let height = usize::from(regions.editor.height).max(1);
    let scroll = row.saturating_sub(height - 1);
    let mut input = layout
        .lines
        .into_iter()
        .skip(scroll)
        .take(height)
        .collect::<Vec<_>>();
    if view.draft.is_empty() {
        input = vec![Line::styled(placeholder.into_owned(), theme::DIM)];
    }
    composer::render(
        frame,
        regions.composer,
        regions.bordered,
        focused,
        input,
        scroll,
    );
    if let Some(hint) = completion(state) {
        frame.render_widget(Paragraph::new(hint).style(theme::DIM), regions.completion);
    }
    if focused && !regions.editor.is_empty() {
        let cursor = (
            regions.editor.x + column as u16,
            regions.editor.y + (row - scroll) as u16,
        );
        if let Some(cell) = frame.buffer_mut().cell_mut(cursor) {
            cell.set_style(theme::INPUT_TEXT.add_modifier(Modifier::REVERSED));
        }
        frame.set_cursor_position(cursor);
    }
    let keys = if state.demo.as_ref().is_some_and(|demo| demo.legacy) {
        "Left/Right Sessions   F1 Work chat   F2 Works   Ctrl+Q Close".into()
    } else if state.demo.is_some() && state.menu.is_some() {
        "Up/Down Select | Enter Confirm | Esc Cancel".into()
    } else if state.demo.is_some() && state.focus == Focus::Details {
        format!(
            "F5 Back   F7 Expand/Compact   {}   PgUp/PgDn Scroll",
            demo::f6_hint(state)
        )
        .into()
    } else if state.demo.is_some() && state.task_list {
        format!(
            "Up/Down Select   Enter Open   F4 Actions   F5 Evidence   {}",
            demo::f6_hint(state)
        )
        .into()
    } else if state.demo.is_some() {
        format!(
            "Enter Send   F4 Actions   F5 Evidence   {}   PgUp/PgDn Scroll",
            demo::f6_hint(state)
        )
        .into()
    } else if state.inline_approval() {
        t!("agent_center.approval_keys")
    } else if state.pending.is_some() {
        t!("agent_center.confirm_prompt")
    } else if state.form.is_some() {
        t!("agent_center.console_form_help")
    } else if state.menu.is_some() {
        t!("agent_center.console_menu_help")
    } else if state.diagnostics && state.focus != Focus::Composer {
        t!("agent_center.console_hint")
    } else {
        match state.focus {
            Focus::Composer if state.task_list => t!("agent_center.task_keys"),
            Focus::Composer => t!("agent_center.task_list_keys"),
            Focus::Actions => t!("agent_center.ui_actions_hint"),
            Focus::Details => t!("agent_center.ui_details_hint"),
        }
    };
    frame.render_widget(
        Paragraph::new(keys.into_owned())
            .wrap(Wrap { trim: false })
            .style(theme::DIM),
        regions.keys,
    );
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::{Backend, TestBackend};

    const A: &str = "11111111-1111-4111-8111-111111111111";
    const B: &str = "22222222-2222-4222-8222-222222222222";
    const PROJECT: &str = "33333333-3333-4333-8333-333333333333";
    const MESSAGE: &str = "44444444-4444-4444-8444-444444444444";
    const USER_TEXT: &str = "99999999-9999-4999-8999-999999999999";

    fn fixture() -> State {
        let mut state = State::legacy();
        state.notice.clear();
        state.projects = vec![json!({"id":PROJECT,"name":"Harbor","root":"C:\\source-only"})];
        state.works = vec![
            json!({"work":{"kind":"Work","id":A,"version":3,"projectId":PROJECT,
                "lifecycle":"Active","desiredAdvancement":"Advance"},
                "spec":{"goal":"Actual report goal","scope":["Read recorded build logs"],
                    "criteria":[{"description":"Cite observations","evidenceRule":"Recorded source"}],
                    "delivery":{"kind":"Report"}},
                "taskSummaries":[{"objective":"Compare build records","state":"Running"}],"obligations":[]}),
            json!({"work":{"kind":"Work","id":B,"version":2,"projectId":PROJECT,
                "lifecycle":"Draft"},"spec":{"goal":"Second real goal"}}),
        ];
        for view in &state.works {
            capture_versions(&mut state.context, &json!({"data":view}));
        }
        state.select(Some(A.into()));
        state.append(Some(A.into()), &json!({"kind":"Conversation","id":"conversation-system",
            "messages":[
                {"kind":"ConversationItem","id":MESSAGE,"conversationId":"conversation-system","role":"human","status":"Complete","text":format!("Keep {USER_TEXT} in C:\\reports\\human.txt")},
                {"kind":"ConversationItem","id":"assistant-system","conversationId":"conversation-system","role":"assistant","status":"Streaming","parts":[{"text":"Recorded assistant response"}]}
            ]}));
        state
    }

    fn draw(state: &mut State, width: u16, height: u16) -> (String, Terminal<TestBackend>) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render(frame, state)).unwrap();
        let mut text = String::new();
        for row in terminal
            .backend()
            .buffer()
            .content
            .chunks(usize::from(width).max(1))
        {
            let mut column = 0;
            while column < row.len() {
                let symbol = row[column].symbol();
                text.push_str(symbol);
                // A wide grapheme owns its continuation cells; those blank
                // cells are not additional spaces in the displayed text.
                column += symbol.width().max(1);
            }
            text.push('\n');
        }
        (text, terminal)
    }

    #[test]
    fn executor_queue_is_visible_only_for_its_current_work_and_claim_is_explicit() {
        let _locale = crate::test_support::lock_locale();
        rust_i18n::set_locale("en-US");
        let mut state = fixture();
        state.dashboard = false;
        state.works[0]["continuation"] = json!({
            "state":"Running","pendingInputCount":2,
            "activeResponses":[{"messageId":"assistant-system","deadlineUtc":"2999-01-01T00:00:00Z"}]
        });
        let queued = t!("agent_center.task_queued_messages", count = 2).into_owned();
        let (screen, _) = draw(&mut state, 120, 24);
        assert!(screen.contains(&queued), "{screen}");
        assert!(
            screen.contains(t!("chat.activity_thinking").as_ref()),
            "{screen}"
        );
        state.context.global_conversation = true;
        let (screen, _) = draw(&mut state, 120, 24);
        assert!(!screen.contains(&queued), "{screen}");
        state.context.global_conversation = false;
        state.works[0]["continuation"] = json!({"state":"Ready","canClaimExecutor":true});
        let (screen, _) = draw(&mut state, 120, 24);
        assert!(
            screen.contains(t!("agent_center.task_claim_executor").as_ref()),
            "{screen}"
        );
    }

    #[test]
    fn chat_and_tasks_navigation_is_visible_in_every_main_view() {
        let _locale = crate::test_support::lock_locale();
        let (jobs, _) = tokio::sync::mpsc::unbounded_channel();
        for locale in ["en-US", "zh-CN"] {
            rust_i18n::set_locale(locale);
            let mut state = State::new();
            for view in 0..3 {
                if view == 1 {
                    handle_key(&mut state, KeyCode::F(1), KeyModifiers::NONE, &jobs).unwrap();
                } else if view == 2 {
                    state = fixture();
                    state.dashboard = false;
                }
                for width in [40, 80, 160] {
                    let (text, _) = draw(&mut state, width, 24);
                    let navigation = text.lines().next().unwrap();
                    assert!(navigation.contains("F1"), "{navigation}");
                    assert!(navigation.contains("F2"), "{navigation}");
                    if width >= 80 {
                        assert!(navigation.contains(t!("agent_center.chat_global").as_ref()));
                        assert!(navigation.contains(t!("agent_center.tasks_title").as_ref()));
                    }
                }
            }
        }
    }

    #[test]
    fn recovery_failure_stays_visible_outside_a_long_historical_chat() {
        let _locale = crate::test_support::lock_locale();
        let mut state = fixture();
        state.dashboard = false;
        let message = "Old execution has no verified settlement record.";
        state.works[0]["continuation"] = json!({
            "state":"NeedsRecovery","activeResponses":[],
            "recoveryFailure":{"code":"OUTCOME_UNKNOWN","message":message}
        });
        for index in 0..60 {
            state.view_mut().messages.push(json!({
                "id":format!("history-{index}"),"role":"assistant","status":"Complete",
                "text":format!("Historical message {index}")
            }));
        }
        state.view_mut().follow = true;
        state
            .view_mut()
            .replace_draft("Do not discard this draft".into());
        state.set_response_notice(&json!({"status":"ok"}));
        state.busy = true;
        let (text, _) = draw(&mut state, 100, 24);
        assert!(text.contains(message), "{text}");
        assert!(!text.contains(t!("chat.activity_thinking").as_ref()));
        assert_eq!(notice_style(&state), theme::NOTICE_ERROR);
        assert_eq!(
            regions(&mut state, Rect::new(0, 0, 100, 24)).notice.height,
            3
        );
        assert_eq!(state.view().unwrap().draft, "Do not discard this draft");
        state.works[0]["continuation"] = json!({"state":"Ready","activeResponses":[]});
        assert!(!notice(&state).contains(message));
    }

    #[test]
    fn task_chat_keeps_real_replies_visible_amid_internal_events_and_moves_facts_to_details() {
        let _locale = crate::test_support::lock_locale();
        let mut state = fixture();
        state.dashboard = false;
        state.works[0]["continuation"] = json!({"state":"Ready"});
        state.view_mut().messages[1]["text"] = json!("Recorded assistant response");
        for index in 0..300 {
            state.append(
                Some(A.into()),
                &json!({
                    "kind":"Observation","id":format!("internal-{index}"),
                    "summary":"WorkContinued internal observation","status":"Pending"
                }),
            );
        }
        state.append(
            Some(A.into()),
            &json!({
                "kind":"ConversationItem","id":"service-message","role":"service",
                "text":"Internal service bookkeeping"
            }),
        );
        state.view_mut().follow = true;
        let (screen, _) = draw(&mut state, 100, 24);
        assert!(screen.contains("Recorded assistant response"), "{screen}");
        assert_eq!(screen.matches("Recorded assistant response").count(), 1);
        assert!(screen.contains(USER_TEXT));
        assert!(!screen.contains("WorkContinued"));
        assert!(!screen.contains("Internal service bookkeeping"));
        assert!(!screen.contains(t!("agent_center.ui_recorded_facts").as_ref()));
        let technical = details(&state)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(technical.contains("WorkContinued internal observation"));
        assert!(technical.contains("Internal service bookkeeping"));
        let (jobs, _) = mpsc::unbounded_channel();
        handle_key(&mut state, KeyCode::F(5), KeyModifiers::NONE, &jobs).unwrap();
        assert!(state.view().unwrap().details_open);
        assert_eq!(state.focus, Focus::Details);
    }

    #[test]
    fn task_chat_has_one_compact_heading_actual_state_and_no_unrelated_attention_or_success() {
        let _locale = crate::test_support::lock_locale();
        let mut state = fixture();
        state.dashboard = false;
        state.works[0]["spec"]["goal"] = json!("Long task heading ".repeat(30));
        state.works[0]["continuation"] = json!({"state":"NeedsRecovery","reason":"PROTOCOL_INCOMPLETE",
            "recoveryFailure":{"code":"PROTOCOL_INCOMPLETE","message":"Original session recovery failed."}});
        state.inbox = vec![json!({"id":"foreign-attention","workId":B,"status":"Open"})];
        state.set_response_notice(&json!({"status":"ok"}));
        let (screen, _) = draw(&mut state, 100, 24);
        assert_eq!(
            screen
                .lines()
                .filter(|line| line.contains("Long task heading"))
                .count(),
            1,
            "{screen}"
        );
        assert!(screen.contains(t!("agent_center.task_state_recovery").as_ref()));
        assert!(screen.contains("Original session recovery failed."));
        assert!(screen.contains(&format!("F4 · {}", t!("agent_center.task_continue"))));
        assert!(!screen.contains("PROTOCOL_INCOMPLETE"));
        assert!(!screen.contains(t!("agent_center.status_ok").as_ref()));
        assert!(!screen.contains("Needs attention:"));
        assert!(state.visible_actions().items.is_empty());
        let (jobs, _) = mpsc::unbounded_channel();
        handle_key(&mut state, KeyCode::F(6), KeyModifiers::NONE, &jobs).unwrap();
        assert_eq!(state.focus, Focus::Composer);
        handle_key(&mut state, KeyCode::F(4), KeyModifiers::NONE, &jobs).unwrap();
        let menu = state.menu.as_ref().unwrap();
        assert!(menu
            .items
            .iter()
            .any(|(_, choice)| { matches!(choice, Choice::OpenTab(id) if id == A) }));
        assert!(menu.items.iter().any(|(_, choice)| {
            matches!(choice, Choice::Control(operation)
                if operation.method == "work.continue" && operation.params["workId"] == A
                    && operation.params.get("restartSession").is_none())
        }));
    }

    #[test]
    fn task_questions_stay_actionable_without_expanding_routine_work_controls() {
        let _locale = crate::test_support::lock_locale();
        let mut state = fixture();
        state.dashboard = false;
        state.works[0]["continuation"] = json!({"state":"WaitingForInput"});
        for (id, work, question) in [
            ("own-question", A, "Which report should I use?"),
            ("foreign-question", B, "Unrelated question"),
        ] {
            state.append(
                Some(work.into()),
                &json!({
                    "kind":"DecisionRequest","id":id,"workId":work,"status":"Open",
                    "version":1,"question":question,"responseSchema":{"type":"string"}
                }),
            );
        }
        let menu = state.visible_actions();
        assert_eq!(menu.items.len(), 1);
        assert!(
            matches!(&menu.items[0].1, Choice::Question(question, Some(work))
            if work == A && question["id"] == "own-question")
        );
        let (screen, _) = draw(&mut state, 100, 24);
        assert!(screen.contains("Which report should I use?"), "{screen}");
        assert!(!screen.contains("Unrelated question"));
        let (jobs, _) = mpsc::unbounded_channel();
        state.view_mut().replace_draft("Unsent task draft".into());
        handle_key(&mut state, KeyCode::F(6), KeyModifiers::NONE, &jobs).unwrap();
        handle_key(&mut state, KeyCode::Enter, KeyModifiers::NONE, &jobs).unwrap();
        assert_eq!(state.form.as_ref().unwrap().question["id"], "own-question");
        assert_eq!(state.view().unwrap().draft, "Unsent task draft");
    }

    #[test]
    fn task_delta_without_a_snapshot_is_still_an_assistant_reply() {
        let _locale = crate::test_support::lock_locale();
        let mut state = fixture();
        state.dashboard = false;
        state.append(
            Some(A.into()),
            &json!({
                "messageId":"late-snapshot","text":"Live assistant text"
            }),
        );
        let (screen, _) = draw(&mut state, 100, 24);
        assert!(screen.contains("● Live assistant text"), "{screen}");
    }

    #[test]
    fn thinking_animates_above_input_without_moving_editor_or_adding_empty_chat_rows() {
        let _locale = crate::test_support::lock_locale();
        let mut state = State::new();
        state.task_list = false;
        handle_paste(&mut state, "Keep this draft");
        let (jobs, _) = mpsc::unbounded_channel();
        handle_key(&mut state, KeyCode::Left, KeyModifiers::SHIFT, &jobs).unwrap();
        let selection = state.chat.editor.selection();
        state
            .chat
            .messages
            .push(json!({"id":"response","role":"assistant",
            "status":"Streaming","parts":[]}));
        assert!(message_lines(&state.chat, true, 80).is_empty());
        for width in [1, 2, 8, 80, 160] {
            for height in [1, 3, 8, 24, 45] {
                let (_, mut terminal) = draw(&mut state, width, height);
                let areas = regions(&mut state, Rect::new(0, 0, width, height));
                assert!(areas
                    .editor
                    .contains(terminal.backend_mut().get_cursor_position().unwrap()));
                if height >= 3 {
                    assert_eq!(areas.scope.bottom(), areas.composer.y);
                    assert_eq!(areas.scope.height, 1);
                    let expected = t!("chat.activity_thinking");
                    let row =
                        terminal.backend().buffer().content[usize::from(areas.scope.y * width)
                            ..usize::from((areas.scope.y + 1) * width)]
                            .iter()
                            .map(|cell| cell.symbol())
                            .collect::<String>();
                    assert!(row.starts_with(expected.chars().next().unwrap()));
                    if width >= 80 {
                        assert!(row.contains(expected.as_ref()));
                    }
                }
                assert_eq!(state.chat.draft, "Keep this draft");
                assert_eq!(state.chat.editor.selection(), selection);
            }
        }
        let (_, initial) = draw(&mut state, 80, 24);
        state.activity_frame = 6;
        let (_, animated) = draw(&mut state, 80, 24);
        assert_ne!(initial.backend().buffer(), animated.backend().buffer());
        let before = regions(&mut state, Rect::new(0, 0, 80, 24));
        state.chat.messages[0]["status"] = json!("Complete");
        let (idle, _) = draw(&mut state, 80, 24);
        assert!(!idle.contains(t!("chat.activity_thinking").as_ref()));
        let after = regions(&mut state, Rect::new(0, 0, 80, 24));
        assert_eq!(before.editor, after.editor);
        assert_eq!(before.body, after.body);
    }

    #[test]
    fn adaptive_frames_keep_real_grapheme_caret_inside_wide_compact_and_tiny_composers() {
        let _locale = crate::test_support::lock_locale();
        let mut state = fixture();
        handle_paste(&mut state, "中e\u{301}👩‍💻tail");
        for width in [1, 2, 8, 80, 89, 90, 132, 160] {
            for height in [1, 3, 8, 24, 45] {
                let (_, mut terminal) = draw(&mut state, width, height);
                let areas = regions(&mut state, Rect::new(0, 0, width, height));
                assert_eq!(
                    areas.rail.width, 0,
                    "Task navigation uses one footer, never a duplicate rail"
                );
                let cursor = terminal.backend_mut().get_cursor_position().unwrap();
                assert!(cursor.x < width && cursor.y < height);
                assert!(areas.editor.contains(cursor));
                assert_eq!(state.view().unwrap().draft, "中e\u{301}👩‍💻tail");
            }
        }
    }

    #[test]
    fn full_frames_separate_real_roles_and_suppress_only_protocol_identity() {
        let _locale = crate::test_support::lock_locale();
        let mut state = fixture();
        for width in [80, 120, 160] {
            let (screen, terminal) = draw(&mut state, width, 45);
            assert!(
                screen.contains("Actual report goal")
                    && screen.contains("Recorded assistant response")
            );
            assert!(screen.contains(USER_TEXT) && screen.contains("C:\\reports\\human.txt"));
            assert!(screen.contains("> Keep") && screen.contains("● Recorded assistant response"));
            assert!(
                !screen.contains("C:\\source-only"),
                "the rail must not repeat full project roots"
            );
            for id in [
                A,
                B,
                PROJECT,
                MESSAGE,
                "conversation-system",
                "assistant-system",
            ] {
                assert!(!screen.contains(id), "system identity leaked: {id}");
            }
            assert!(terminal
                .backend()
                .buffer()
                .content
                .iter()
                .any(|cell| cell.modifier.contains(Modifier::BOLD)));
        }
        state.diagnostics = true;
        let (screen, _) = draw(&mut state, 160, 70);
        assert!(screen.contains(MESSAGE) && screen.contains("conversation-system"));
    }

    #[test]
    fn diagnostic_wire_copy_survives_wrapping_without_changing_literal_content() {
        let _locale = crate::test_support::lock_locale();
        let mut state = State::new();
        state.diagnostics = true;
        let operation = Operation {
            method: "project.configure".into(),
            params: json!({
                "root":format!("C:\\{}\\approved", "a long directory ".repeat(12)),
                "literal":"  leading  middle  trailing \n\t中👩‍💻\\\" "
            }),
            if_match: vec![json!({"kind":"Work","id":A,"version":7})],
            mutation: true,
            confirmation: true,
            command_id: "exact-frozen-command".into(),
        };
        let expected = operation_preview(&operation);
        let authority =
            json!({"grant":{"limits":{"concurrency":1}},"project":{"root":"C:\\project"}});
        let mut duplicated = authority.clone();
        duplicated["humanActionProposal"] =
            json!({"id":"actual-proposal","version":3,"request":expected,"preview":authority});
        let mut different = duplicated.clone();
        different["humanActionProposal"]["preview"]["grant"]["limits"]["concurrency"] = json!(2);
        for (preview, omit_duplicate) in [
            (json!({}), false),
            (
                json!({"operation":expected,"requestUtf8Hex":"forged"}),
                false,
            ),
            (json!("Readable preview"), false),
            (duplicated, true),
            (different, false),
        ] {
            state.pending = Some(PendingConfirmation {
                operation: operation.clone(),
                preview: preview.clone(),
                work: None,
                input: String::new(),
                scroll: 0,
                target_label: "Captured action".into(),
                origin: MutationOrigin::Action,
            });
            let diagnostic: Value = serde_json::from_str(&diagnostic(&state)).unwrap();
            assert_eq!(state.pending.as_ref().unwrap().preview, preview);
            if omit_duplicate {
                assert!(diagnostic["humanActionProposal"].get("preview").is_none());
                assert_eq!(diagnostic["humanActionProposal"]["id"], "actual-proposal");
                assert_eq!(diagnostic["humanActionProposal"]["version"], 3);
                assert_eq!(diagnostic["humanActionProposal"]["request"], expected);
                assert_eq!(diagnostic["grant"], preview["grant"]);
                assert_eq!(diagnostic["project"], preview["project"]);
            } else if preview.get("humanActionProposal").is_some() {
                assert_eq!(
                    diagnostic["humanActionProposal"],
                    preview["humanActionProposal"]
                );
            }
            let encoded = diagnostic["requestUtf8Hex"].as_str().unwrap();
            for width in [48, 80, 160] {
                let (screen, _) = draw(&mut state, width, 240);
                let field = screen.split_once("\"requestUtf8Hex\":").unwrap().1;
                let physical = field.split('"').nth(1).unwrap();
                let captured = physical
                    .lines()
                    .map(|line| line.trim_matches([' ', '│']))
                    .collect::<String>();
                assert_eq!(captured, encoded, "{width}: {screen}");
                let bytes: Vec<u8> = captured
                    .as_bytes()
                    .chunks_exact(2)
                    .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
                    .collect();
                assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), expected);
                assert_eq!(
                    operation_preview(&state.pending.as_ref().unwrap().operation),
                    expected
                );
            }
        }
    }

    #[test]
    fn action_focus_is_not_editor_focus_and_cancel_keeps_exact_guard_and_nonce() {
        let _locale = crate::test_support::lock_locale();
        let mut state = fixture();
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        handle_paste(&mut state, "independent draft");
        handle_key(&mut state, KeyCode::Left, KeyModifiers::SHIFT, &jobs).unwrap();
        let selection = state.view().unwrap().editor.selection();
        handle_key(&mut state, KeyCode::F(6), KeyModifiers::NONE, &jobs).unwrap();
        handle_paste(&mut state, "must not enter the draft");
        handle_key(&mut state, KeyCode::Char('x'), KeyModifiers::NONE, &jobs).unwrap();
        handle_key(&mut state, KeyCode::F(12), KeyModifiers::NONE, &jobs).unwrap();
        handle_key(&mut state, KeyCode::Enter, KeyModifiers::NONE, &jobs).unwrap();
        handle_key(&mut state, KeyCode::Char('x'), KeyModifiers::NONE, &jobs).unwrap();
        assert!(
            receiver.try_recv().is_err(),
            "diagnostics must not turn action focus into editor submission"
        );
        handle_key(&mut state, KeyCode::F(12), KeyModifiers::NONE, &jobs).unwrap();
        let cancel_index = state.cards.as_ref().unwrap().items.iter().position(|(_, choice)| {
            matches!(choice, Choice::Control(operation) if operation.params["action"] == "Cancel")
        }).unwrap();
        for _ in 0..cancel_index {
            handle_key(&mut state, KeyCode::Down, KeyModifiers::NONE, &jobs).unwrap();
        }
        let operation = match &state.cards.as_ref().unwrap().items[cancel_index].1 {
            Choice::Control(operation) => operation.clone(),
            _ => panic!("expected captured cancel"),
        };
        assert!(draw(&mut state, 80, 24).0.contains("Cancel work"));
        handle_key(&mut state, KeyCode::Enter, KeyModifiers::NONE, &jobs).unwrap();
        handle_key(&mut state, KeyCode::F(5), KeyModifiers::NONE, &jobs).unwrap();
        handle_key(&mut state, KeyCode::Enter, KeyModifiers::NONE, &jobs).unwrap();
        assert!(receiver.try_recv().is_err());
        assert_eq!(
            state.pending.as_ref().unwrap().operation.command_id,
            operation.command_id
        );
        assert_eq!(
            state.pending.as_ref().unwrap().operation.if_match,
            operation.if_match
        );
        assert_eq!(state.pending.as_ref().unwrap().work.as_deref(), Some(A));
        assert_eq!(state.view().unwrap().draft, "independent draft");
        assert_eq!(state.view().unwrap().editor.selection(), selection);
        handle_key(&mut state, KeyCode::Enter, KeyModifiers::CONTROL, &jobs).unwrap();
        let job = receiver.try_recv().unwrap();
        assert!(matches!(job.kind, JobKind::Send(ref sent)
            if sent.command_id == operation.command_id && sent.if_match == operation.if_match
            && sent.params["workId"] == A && sent.params["action"] == "Cancel"));
    }

    #[test]
    fn details_are_read_only_and_preserve_work_drafts_carets_and_read_positions() {
        let _locale = crate::test_support::lock_locale();
        let mut state = fixture();
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        handle_paste(&mut state, "A selection");
        handle_key(&mut state, KeyCode::Left, KeyModifiers::SHIFT, &jobs).unwrap();
        let caret = state.view().unwrap().editor.cursor;
        let anchor = state.view().unwrap().editor.anchor;
        state.view_mut().scroll = 4;
        state.view_mut().follow = false;
        handle_key(&mut state, KeyCode::F(5), KeyModifiers::NONE, &jobs).unwrap();
        handle_key(&mut state, KeyCode::PageDown, KeyModifiers::NONE, &jobs).unwrap();
        handle_key(&mut state, KeyCode::Enter, KeyModifiers::NONE, &jobs).unwrap();
        handle_paste(&mut state, "not a message");
        let detail_scroll = state.view().unwrap().details_scroll;
        assert!(detail_scroll > 0);
        assert_eq!(state.view().unwrap().scroll, 4);
        handle_key(&mut state, KeyCode::Esc, KeyModifiers::NONE, &jobs).unwrap();
        state.select(Some(B.into()));
        handle_paste(&mut state, "B selection");
        state.select(Some(A.into()));
        assert_eq!(state.view().unwrap().draft, "A selection");
        assert_eq!(state.view().unwrap().editor.cursor, caret);
        assert_eq!(state.view().unwrap().editor.anchor, anchor);
        assert_eq!(state.view().unwrap().details_scroll, detail_scroll);
        assert_eq!(state.views[&Some(B.into())].draft, "B selection");
        assert!(
            receiver.try_recv().is_err(),
            "details and navigation must not mutate authority"
        );
    }

    #[test]
    fn details_distinguish_project_source_from_reported_managed_execution_location() {
        let _locale = crate::test_support::lock_locale();
        let mut state = fixture();
        state.view_mut().records.insert(
            "workspace".into(),
            json!({"kind":"Workspace",
            "workId":A,"localRoot":"C:\\managed-actual","id":"workspace-system"}),
        );
        state.view_mut().details_open = true;
        state.focus = Focus::Details;
        let (screen, _) = draw(&mut state, 100, 70);
        assert!(screen.contains("Project source location") && screen.contains("C:\\source-only"));
        assert!(
            screen.contains("Managed execution location") && screen.contains("C:\\managed-actual")
        );
        assert!(!screen.contains("workspace-system"));
        state.view_mut().records.remove("workspace");
        let (screen, _) = draw(&mut state, 100, 70);
        assert!(screen.contains("No execution location has been reported."));
        assert_eq!(screen.matches("C:\\source-only").count(), 1);
    }

    #[test]
    fn overview_attention_and_duplicate_navigation_use_real_captured_scopes() {
        let _locale = crate::test_support::lock_locale();
        let mut state = fixture();
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        let original = state.selection();
        state.view_mut().replace_draft("Keep task draft".into());
        state.activate(Choice::Overview, &jobs).unwrap();
        assert_eq!(state.selection(), original);
        assert!(state.task_list);
        state.works[1]["spec"]["goal"] = state.works[0]["spec"]["goal"].clone();
        let (wide, _) = draw(&mut state, 160, 45);
        assert!(wide.contains("[1]") && wide.contains("[2]"));
        state.activate(Choice::Attention, &jobs).unwrap();
        assert_eq!(state.home_screen, HomeScreen::Attention);
        assert_eq!(state.selection(), original);
        assert_eq!(state.view().unwrap().draft, "Keep task draft");
        assert!(state.tasks().iter().any(|view| view["work"]["id"] == B));
        assert!(!state.tasks().iter().any(|view| view["work"]["id"] == A));
        handle_key(&mut state, KeyCode::Enter, KeyModifiers::NONE, &jobs).unwrap();
        assert!(
            matches!(receiver.try_recv().unwrap().kind, JobKind::Send(ref operation)
                if operation.method == "work.open" && operation.params["workId"] == B)
        );
    }

    #[test]
    fn busy_stale_and_unknown_frames_do_not_claim_no_requests_or_current_execution() {
        let _locale = crate::test_support::lock_locale();
        let mut state = State::legacy();
        state.home_screen = HomeScreen::Attention;
        state.busy = true;
        let (busy, _) = draw(&mut state, 80, 24);
        assert!(busy.contains("Sending the captured request"));
        assert!(!busy.contains("No open requests"));
        state.stale = true;
        state.busy = false;
        state.notice = "EOF: service stream stopped".into();
        let (stale, _) = draw(&mut state, 80, 24);
        assert!(stale.contains("Cached facts") && stale.contains("EOF: service stream stopped"));
        assert!(!stale.contains("No open requests"));
        state = fixture();
        let operation = state
            .actions()
            .items
            .into_iter()
            .find_map(|(_, choice)| match choice {
                Choice::Control(operation) => Some(operation),
                _ => None,
            })
            .unwrap();
        state.mutations.insert(
            operation.command_id.clone(),
            PendingMutation {
                operation,
                input: String::new(),
                selection: state.selection(),
                preview: Value::Null,
                target_label: state.work_label(Some(A)),
                unknown: Some("receipt lost".into()),
                origin: MutationOrigin::Action,
                requires_confirmation: true,
            },
        );
        state.notice = t!("agent_center.status_ok").into_owned();
        let (unknown, _) = draw(&mut state, 160, 45);
        assert!(unknown.contains("receipt lost") || unknown.contains("Unknown"));
        assert!(!unknown.contains("Request succeeded."));
        assert!(!unknown.contains("Completed — human acceptance recorded"));
        state = State::legacy();
        state.select(Some("unavailable-system-work".into()));
        let (unknown, _) = draw(&mut state, 80, 24);
        assert!(unknown.contains("Work details unavailable"));
        assert!(!unknown.contains("What would you like to achieve?"));
        assert!(!unknown.contains("unavailable-system-work"));
    }

    #[test]
    fn typed_form_shares_grapheme_layout_caret_and_selection_at_small_height() {
        let _locale = crate::test_support::lock_locale();
        let mut state = fixture();
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        let question = json!({"kind":"IntakeRequest","id":"request-system","version":2,
            "conversationId":"conversation-system","status":"Open","question":"Recorded question ".repeat(30),
            "responseSchema":{"type":"string","description":"Recorded description ".repeat(50)}});
        state
            .activate(Choice::Question(question, Some(A.into())), &jobs)
            .unwrap();
        handle_paste(&mut state, "中e\u{301}tail");
        handle_key(&mut state, KeyCode::Left, KeyModifiers::SHIFT, &jobs).unwrap();
        for width in [40, 80, 120] {
            let (screen, mut terminal) = draw(&mut state, width, 12);
            let cursor = terminal.backend_mut().get_cursor_position().unwrap();
            assert!(cursor.x < width && cursor.y < 12);
            assert_eq!(
                terminal.backend().buffer().cell(cursor).unwrap().symbol(),
                "l"
            );
            assert!(terminal
                .backend()
                .buffer()
                .cell(cursor)
                .unwrap()
                .modifier
                .contains(Modifier::REVERSED));
            assert!(screen.contains("中e\u{301}tail"));
            assert!(!screen.contains("request-system"));
        }
        assert!(receiver.try_recv().is_err());
        state.form = None;
        let error = state
            .activate(
                Choice::Question(
                    json!({"kind":"IntakeRequest","id":"unsupported",
            "version":1,"responseSchema":{"type":"array","items":{"type":"object"}}}),
                    Some(A.into()),
                ),
                &jobs,
            )
            .unwrap_err();
        state.notice = error.to_string();
        assert!(draw(&mut state, 160, 40)
            .0
            .contains("schema the simple form does not support"));
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn delivery_return_preserves_fixed_inspection_and_conversation_read_position() {
        let _locale = crate::test_support::lock_locale();
        let mut state = fixture();
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        state.view_mut().scroll = 7;
        state.view_mut().follow = false;
        let inspection = json!({"work":state.works[0]["work"],
            "candidate":{"id":"candidate-system","version":1,"status":"Proposed"},
            "result":{"body":{"summary":"Actual fixed delivery"}}});
        state.receive(Update {
            work: Some(A.into()),
            input: String::new(),
            conversation: None,
            result: Ok(Outcome::Inspected(inspection.clone())),
        });
        assert!(draw(&mut state, 100, 35)
            .0
            .contains("Actual fixed delivery"));
        handle_key(&mut state, KeyCode::Esc, KeyModifiers::NONE, &jobs).unwrap();
        assert!(!state.view().unwrap().show_delivery);
        assert_eq!(state.view().unwrap().scroll, 7);
        assert_eq!(state.view().unwrap().inspection.as_ref(), Some(&inspection));
        state.view_mut().scroll = 0;
        let (screen, _) = draw(&mut state, 100, 35);
        assert!(screen.contains("Recorded assistant response"));
        assert!(!screen.contains("candidate-system"));
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn inspected_delivery_failure_details_keep_the_actual_error_and_path_accessible() {
        let _locale = crate::test_support::lock_locale();
        let mut state = fixture();
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        let inspection = json!({"work":state.works[0]["work"],
            "candidate":{"id":"fixed-candidate","version":1,"status":"Proposed"},
            "result":{"body":{"summary":"Actual inspected report"}}});
        state.view_mut().inspection = Some(inspection.clone());
        state.view_mut().show_delivery = true;
        let message = "Evidence read failed: access denied at C:\\reports\\actual.log";
        state.append(
            Some(A.into()),
            &json!({"status":"error","failure":{
            "code":"ACCESS_DENIED","message":message,"subjects":[{"id":"artifact-system"}]}}),
        );
        state.notice = t!("agent_center.status_error").into_owned();
        handle_key(&mut state, KeyCode::F(5), KeyModifiers::NONE, &jobs).unwrap();
        let (screen, _) = draw(&mut state, 100, 45);
        assert!(screen.contains(message));
        assert!(!screen.contains("artifact-system"));
        assert_eq!(state.view().unwrap().inspection.as_ref(), Some(&inspection));
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn foreign_attention_is_visible_coalesced_and_opens_its_actual_question_without_touching_draft()
    {
        let _locale = crate::test_support::lock_locale();
        let mut state = fixture();
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        state.select(Some(B.into()));
        handle_paste(&mut state, "B draft stays here");
        handle_key(&mut state, KeyCode::Left, KeyModifiers::SHIFT, &jobs).unwrap();
        let selection = state.view().unwrap().editor.selection();
        let update = |version, question_status, attention_status| {
            json!({
            "type":"event","eventId":format!("attention-batch-{version}"),"cursor":format!("store:{version}"),
            "kind":"DecisionRequested","workId":A,"changes":[
                {"subject":{"kind":"DecisionRequest","id":"decision-a","version":version},
                 "view":{"kind":"DecisionRequest","id":"decision-a","workId":A,"version":version,
                    "status":question_status,"question":"Approve the recorded coverage?","responseSchema":{"type":"boolean"}}},
                {"subject":{"kind":"AttentionItem","id":"attention-a","version":version},
                 "view":{"kind":"AttentionItem","id":"attention-a","workId":A,"version":version,
                    "status":attention_status,"subjectId":"decision-a","reason":"Review the coverage decision"}}
            ]})
        };
        state.event(update(1, "Open", "Open"));
        state.event(update(1, "Open", "Open"));
        assert_eq!(
            state.attention_targets().len(),
            1,
            "one decision, not its duplicate event and attention wrapper"
        );
        for width in [80, 120, 160] {
            let (screen, _) = draw(&mut state, width, 24);
            assert!(screen.contains("Needs attention: 1"));
            assert!(screen.contains("Actual report goal"));
            assert!(screen.contains("Second real goal"));
            assert_eq!(state.context.work_id.as_deref(), Some(B));
            assert_eq!(state.focus, Focus::Composer);
            assert_eq!(state.view().unwrap().draft, "B draft stays here");
            assert_eq!(state.view().unwrap().editor.selection(), selection);
        }
        handle_key(&mut state, KeyCode::F(4), KeyModifiers::NONE, &jobs).unwrap();
        let attention = state
            .menu
            .as_ref()
            .unwrap()
            .items
            .iter()
            .position(|(_, choice)| matches!(choice, Choice::Attention))
            .unwrap();
        for _ in 0..attention {
            handle_key(&mut state, KeyCode::Down, KeyModifiers::NONE, &jobs).unwrap();
        }
        handle_key(&mut state, KeyCode::Enter, KeyModifiers::NONE, &jobs).unwrap();
        handle_key(&mut state, KeyCode::F(4), KeyModifiers::NONE, &jobs).unwrap();
        let question = state
            .menu
            .as_ref()
            .unwrap()
            .items
            .iter()
            .position(|(_, choice)| {
                matches!(choice, Choice::Question(question, work)
                if question["id"] == "decision-a" && work.as_deref() == Some(A))
            })
            .unwrap();
        for _ in 0..question {
            handle_key(&mut state, KeyCode::Down, KeyModifiers::NONE, &jobs).unwrap();
        }
        handle_key(&mut state, KeyCode::Enter, KeyModifiers::NONE, &jobs).unwrap();
        let form = state.form.as_ref().unwrap();
        assert_eq!(form.work.as_deref(), Some(A));
        assert_eq!(form.question["id"], "decision-a");
        assert_eq!(form.fields[0].choice, None);
        assert_eq!(state.views[&Some(B.into())].draft, "B draft stays here");
        assert_eq!(state.views[&Some(B.into())].editor.selection(), selection);
        assert!(receiver.try_recv().is_err());
        state.event(update(2, "Answered", "Resolved"));
        assert!(state.attention_targets().is_empty());
    }

    #[test]
    fn completion_has_a_real_row_and_tracks_keyboard_choice_without_hiding_the_caret() {
        let _locale = crate::test_support::lock_locale();
        let mut state = fixture();
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        handle_paste(&mut state, "/wo");
        let suggestions = commands::complete_with_context("/wo", &state.context);
        assert!(suggestions.len() > 1);
        let (screen, mut terminal) = draw(&mut state, 80, 24);
        let area = regions(&mut state, Rect::new(0, 0, 80, 24));
        assert_eq!(area.completion.height, 1);
        assert_eq!(area.completion.y, area.composer.bottom());
        assert!(area.editor.bottom() < area.completion.y);
        assert_eq!(
            terminal
                .backend()
                .buffer()
                .cell((area.composer.x, area.completion.y - 1))
                .unwrap()
                .symbol(),
            "└"
        );
        assert!(screen.contains(&suggestions[0]));
        assert!(area
            .editor
            .contains(terminal.backend_mut().get_cursor_position().unwrap()));
        assert_eq!(state.view().unwrap().draft, "/wo");
        handle_key(&mut state, KeyCode::Down, KeyModifiers::NONE, &jobs).unwrap();
        assert!(draw(&mut state, 80, 24).0.contains(&suggestions[1]));
        handle_key(&mut state, KeyCode::Tab, KeyModifiers::NONE, &jobs).unwrap();
        assert_eq!(state.view().unwrap().draft, format!("{} ", suggestions[1]));
        handle_key(&mut state, KeyCode::Char('x'), KeyModifiers::NONE, &jobs).unwrap();
        assert_eq!(state.view().unwrap().draft, format!("{} x", suggestions[1]));
        state.view_mut().replace_draft("/work ".into());
        for width in [8, 80, 160] {
            let (_, mut terminal) = draw(&mut state, width, 24);
            let area = regions(&mut state, Rect::new(0, 0, width, 24));
            assert_eq!(area.completion.height, 1);
            let cursor = terminal.backend_mut().get_cursor_position().unwrap();
            assert!(area.editor.contains(cursor));
            assert!(!area.completion.contains(cursor));
        }
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn draft_main_body_shows_the_authoritative_brief_without_opening_details() {
        let _locale = crate::test_support::lock_locale();
        let mut state = fixture();
        state.works[0]["work"]["lifecycle"] = json!("Draft");
        state.works[0]["work"]["desiredAdvancement"] = Value::Null;
        state.works[0]["spec"]["exclusions"] = json!(["Do not modify source files"]);
        state.works[0]["taskSummaries"] = json!([]);
        state.view_mut().messages.clear();
        let (screen, _) = draw(&mut state, 80, 45);
        for actual in [
            "Read recorded build logs",
            "Do not modify source files",
            "Cite observations",
            "Recorded source",
            "Captured report files",
        ] {
            assert!(
                screen.contains(actual),
                "authoritative brief field missing: {actual}"
            );
        }
        assert!(!screen.contains("No messages have been recorded"));
        assert!(!screen.contains(A) && !screen.contains(PROJECT));
        assert!(!state.view().unwrap().details_open);
    }

    #[test]
    fn short_inline_actions_do_not_shorten_foreign_targets_or_frozen_confirmation() {
        let _locale = crate::test_support::lock_locale();
        let mut state = fixture();
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        let (full_label, choice) = state.actions().items.into_iter().find(|(_, choice)|
            matches!(choice, Choice::Control(operation) if operation.params["action"] == "Cancel")).unwrap();
        assert_eq!(
            state.action_label(&full_label, &choice),
            t!("agent_center.console_cancel")
        );
        assert!(
            full_label.contains("Actual report goal") && full_label.contains("C:\\source-only")
        );
        state.select(Some(B.into()));
        assert!(state
            .action_label(&full_label, &choice)
            .contains("Actual report goal"));
        state.select(Some(A.into()));
        let operation = match &choice {
            Choice::Control(operation) => operation.clone(),
            _ => unreachable!(),
        };
        state.menu = Some(state.actions());
        assert!(draw(&mut state, 160, 45).0.contains(&full_label));
        state.activate(choice, &jobs).unwrap();
        let pending = state.pending.as_ref().unwrap();
        assert_eq!(pending.operation.command_id, operation.command_id);
        assert_eq!(pending.operation.if_match, operation.if_match);
        assert_eq!(pending.target_label, state.work_label(Some(A)));
        let (screen, _) = draw(&mut state, 160, 60);
        assert!(screen.contains("Actual report goal") && screen.contains("C:\\source-only"));
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn presentation_follows_agent_pane_theme_and_selection_without_an_independent_palette() {
        let _locale = crate::test_support::lock_locale();
        let mut state = fixture();
        state.set_response_notice(&json!({"status":"ok"}));
        let (_, terminal) = draw(&mut state, 120, 45);
        let areas = regions(&mut state, Rect::new(0, 0, 120, 45));
        assert_eq!(
            Some(
                terminal
                    .backend()
                    .buffer()
                    .cell((areas.notice.x, areas.notice.y))
                    .unwrap()
                    .fg
            ),
            theme::DIM.fg
        );
        assert!(terminal
            .backend()
            .buffer()
            .content
            .iter()
            .all(|cell| cell.bg == theme::INPUT_BG));
        assert_eq!(
            Some(
                terminal
                    .backend()
                    .buffer()
                    .cell((areas.composer.x, areas.composer.y))
                    .unwrap()
                    .fg
            ),
            theme::INPUT_BORDER_FOCUSED.fg
        );
        let (jobs, _receiver) = mpsc::unbounded_channel();
        handle_key(&mut state, KeyCode::F(6), KeyModifiers::NONE, &jobs).unwrap();
        let pause = state.cards.as_ref().unwrap().items.iter().position(|(_, choice)| {
            matches!(choice, Choice::Control(operation) if operation.params["action"] == "Hold")
        }).unwrap();
        for _ in 0..pause {
            handle_key(&mut state, KeyCode::Down, KeyModifiers::NONE, &jobs).unwrap();
        }
        let (_, terminal) = draw(&mut state, 80, 24);
        let row = terminal
            .backend()
            .buffer()
            .content
            .chunks(80)
            .find(|row| {
                row.iter()
                    .map(|cell| cell.symbol())
                    .collect::<String>()
                    .contains("> Pause work")
            })
            .unwrap();
        let marker = row.iter().find(|cell| cell.symbol() == ">").unwrap();
        assert_eq!(Some(marker.fg), theme::SELECTED.fg);
        assert_eq!(row[79].bg, theme::INPUT_BG);

        let areas = regions(&mut state, Rect::new(0, 0, 80, 24));
        assert_eq!(
            Some(
                terminal
                    .backend()
                    .buffer()
                    .cell((areas.composer.x, areas.composer.y))
                    .unwrap()
                    .fg
            ),
            theme::INPUT_BORDER.fg,
            "the editor must not claim focus while actions own the keyboard"
        );
    }

    #[test]
    fn notice_color_uses_protocol_state_not_localized_text_prefixes() {
        let _locale = crate::test_support::lock_locale();
        let mut state = fixture();
        for (status, style) in [
            ("ok", theme::DIM),
            ("pending", theme::NOTICE_WARNING),
            ("needs_input", theme::NOTICE_WARNING),
            ("error", theme::NOTICE_ERROR),
            ("conflict", theme::NOTICE_ERROR),
            ("unsupported", theme::NOTICE_ERROR),
        ] {
            state.set_response_notice(&json!({"status":status}));
            state.notice = "Identical notice text".into();
            let (_, terminal) = draw(&mut state, 80, 24);
            let areas = regions(&mut state, Rect::new(0, 0, 80, 24));
            assert_eq!(
                Some(
                    terminal
                        .backend()
                        .buffer()
                        .cell((areas.notice.x, areas.notice.y))
                        .unwrap()
                        .fg
                ),
                style.fg
            );
        }
        state.stale = true;
        assert_eq!(notice_style(&state), theme::NOTICE_WARNING);
    }

    #[test]
    fn conversation_projection_uses_shared_message_rendering_without_losing_literal_content() {
        let _locale = crate::test_support::lock_locale();
        let mut state = State::new();
        let prompt = "Keep literal C:\\Code\\human.txt\n\u{4e2d}e\u{301} unchanged";
        let answer = "Streaming response\nwith its next line";
        state.chat.messages = vec![
            json!({"role":"human","text":prompt,"status":"Complete"}),
            json!({"role":"assistant","text":answer,"parts":[{"text":answer}],"status":"Streaming"}),
        ];
        for width in [8, 24, 80, 160] {
            let mut expected =
                conversation::message_lines(conversation::MessageRole::User, prompt, width, true);
            expected.extend(conversation::message_lines(
                conversation::MessageRole::Assistant,
                answer,
                width,
                false,
            ));
            assert_eq!(message_lines(&state.chat, true, width as u16), expected);
        }
        state.chat.messages[1]
            .as_object_mut()
            .unwrap()
            .remove("text");
        state.chat.messages[1]["parts"] =
            json!([{"text":"Streaming "},{"text":"response\nwith its next line"}]);
        assert_eq!(message_lines(&state.chat, true, 80), {
            let mut expected =
                conversation::message_lines(conversation::MessageRole::User, prompt, 80, true);
            expected.extend(conversation::message_lines(
                conversation::MessageRole::Assistant,
                answer,
                80,
                false,
            ));
            expected
        });
    }

    #[test]
    fn composer_uses_shared_layout_prefix_and_painted_grapheme_caret() {
        let _locale = crate::test_support::lock_locale();
        let mut state = State::new();
        state.task_list = false;
        handle_paste(&mut state, "ab\u{4e2d}e\u{301}");
        let (jobs, _) = mpsc::unbounded_channel();
        handle_key(&mut state, KeyCode::Left, KeyModifiers::NONE, &jobs).unwrap();
        let (_, terminal) = draw(&mut state, 80, 24);
        let areas = regions(&mut state, Rect::new(0, 0, 80, 24));
        assert_eq!(
            areas.editor,
            composer::layout(areas.composer, areas.bordered).text
        );
        let buffer = terminal.backend().buffer();
        assert_eq!(
            buffer
                .cell((areas.editor.x - 2, areas.editor.y))
                .unwrap()
                .symbol(),
            ">"
        );
        let caret = buffer.cell((areas.editor.x + 4, areas.editor.y)).unwrap();
        assert_eq!(caret.symbol(), "e\u{301}");
        assert!(caret.modifier.contains(Modifier::REVERSED));
        assert_eq!(state.chat.draft, "ab\u{4e2d}e\u{301}");
    }

    #[test]
    fn approval_controls_use_the_shared_button_renderer_without_changing_actions() {
        let _locale = crate::test_support::lock_locale();
        let mut actual = Terminal::new(TestBackend::new(50, 3)).unwrap();
        actual
            .draw(|frame| approval_buttons(frame, frame.area(), "work.start", Some(1)))
            .unwrap();
        let labels = [
            format!("[{}]", t!("agent_center.approval_start")),
            format!("[{}]", t!("agent_center.approval_edit")),
            format!("[{}]", t!("agent_center.approval_defer")),
        ];
        let mut expected = Terminal::new(TestBackend::new(50, 3)).unwrap();
        expected
            .draw(|frame| {
                for (index, label) in labels.iter().enumerate() {
                    card::render_buttons(
                        frame,
                        Rect::new(0, index as u16, 50, 1),
                        std::slice::from_ref(label),
                        (index == 1).then_some(0),
                    );
                }
            })
            .unwrap();
        assert_eq!(actual.backend().buffer(), expected.backend().buffer());
    }
}
