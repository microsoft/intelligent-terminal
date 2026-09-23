// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

//! Goal relationships, coordinator chat and independent runtime activity, side by side.

use super::work_model::{editor, line, panel, BLUE, GREEN, INK, MUTED, PAPER};
use super::*;
use crate::agent_center::demo::scenario::work_graph::*;
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Style},
    widgets::{Block, Paragraph, Wrap},
};

pub(super) fn histories(state: &mut State, saved: &Scenario) -> Result<()> {
    state.chat.messages = vec![message("assistant",
        "Tell me the goal and the work it requires. I will retain the relationships, constraints and reported progress.")];
    for view in state.views.values_mut() {
        view.messages.clear();
        view.records.clear();
    }
    let mut replay = Scenario::new();
    for event in &saved.events {
        replay.replay_event(event)?;
        if replay.work_graph.is_none() {
            continue;
        }
        match event.kind {
            EventKind::UserRequest => {
                let input = event.input.as_deref().context("Missing Work graph input")?;
                if input == "Open Work graph" {
                    continue;
                }
                let response = replay.graph_response(input)?;
                if input == EXPLAIN {
                    push_work(state, COMPATIBILITY, "user", input);
                    push_work(state, COMPATIBILITY, "assistant", response);
                } else {
                    state.chat.messages.push(message("user", input));
                    state.chat.messages.push(message("assistant", response));
                }
            }
            EventKind::WorkCreated => push_work(
                state,
                &event.work_id,
                "coordinator",
                format!("Assignment / {}\n{}", event.work_id, event.summary),
            ),
            EventKind::BudgetConsumed | EventKind::BudgetReached => {
                if event.work_id != COMPATIBILITY || replay.graph()?.handoff.is_none() {
                    push_work(
                        state,
                        &event.work_id,
                        "activity",
                        event.summary.replace(" simulated tokens", " tokens"),
                    );
                }
                if event.kind == EventKind::BudgetReached {
                    state.chat.messages.push(message("assistant",
                        format!("{} paused at its own cap. No cap was increased; other Work allocations are unchanged.", event.work_id)));
                }
            }
            EventKind::DemoExecutorUpdate => {
                push_work(state, COMPATIBILITY, "assistant", &event.summary);
                state
                    .chat
                    .messages
                    .push(message("assistant", &event.summary));
            }
            _ => {}
        }
    }
    Ok(())
}

pub(super) fn key(state: &mut State, code: KeyCode) -> Result<Option<InputAction>> {
    match code {
        KeyCode::F(1) => home(state),
        KeyCode::F(4) => open_work(state, COMPATIBILITY)?,
        KeyCode::F(_) => {
            state.notice =
                "Work graph: F1 Main agent; F4 Compatibility agent; Tab suggests a request.".into();
        }
        KeyCode::Tab => {
            let input = if state.context.global_conversation {
                scenario(state)?.next_input()
            } else {
                EXPLAIN
            };
            state.editor_view_mut().replace_draft(input.into());
        }
        _ => return Ok(None),
    }
    Ok(Some(InputAction::None))
}

pub(super) fn chat(frame: &mut ratatui::Frame<'_>, area: Rect, view: &mut WorkView, name: &str) {
    let mut lines = vec![];
    for message in &view.messages {
        let role = match message["role"].as_str() {
            Some("user") => "You",
            Some("coordinator") => "Work assignment",
            Some("activity") => "Runtime checkpoint",
            _ => name,
        };
        lines.push(line(role, BLUE));
        if let Some(text) = message["text"].as_str() {
            lines.extend(text.lines().map(|text| line(text, INK)));
        }
        lines.push(Default::default());
    }
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    let max_scroll = paragraph
        .line_count(area.width.max(1))
        .saturating_sub(usize::from(area.height))
        .min(usize::from(u16::MAX)) as u16;
    view.scroll = if view.follow {
        max_scroll
    } else {
        view.scroll.min(max_scroll)
    };
    frame.render_widget(paragraph.scroll((view.scroll, 0)), area);
}

pub(in crate::agent_center::ui) fn render(frame: &mut ratatui::Frame<'_>, state: &mut State) {
    let area = frame.area();
    frame.render_widget(Block::default().style(Style::new().bg(PAPER).fg(INK)), area);
    if area.width < 110 || area.height < 30 {
        frame.render_widget(
            Paragraph::new(
                "Work graph demo: enlarge the window to 110 x 30 for side-by-side conversations.",
            ),
            area,
        );
        return;
    }
    let Some(saved) = state.demo.as_ref().map(|demo| &demo.scenario) else {
        return;
    };
    let Some(graph) = saved.work_graph.as_ref() else {
        return;
    };
    let global = state.context.global_conversation;
    let rows = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(0),
        Constraint::Length(2),
    ])
    .split(area);
    frame.render_widget(
        Paragraph::new(line(
            " INTELLIGENT TERMINAL  /  Human -> Work -> Agent runtime",
            BLUE,
        )),
        rows[0],
    );
    let columns = Layout::horizontal([
        Constraint::Length(30),
        Constraint::Fill(1),
        Constraint::Fill(1),
    ])
    .split(rows[1]);
    let goal = panel(frame, columns[0], "Saved Work relationships");
    let mut facts = vec![
        line("Deliver fix #4821 safely", INK),
        line(
            format!("Required checks: {}/2", saved.graph_required_passed()),
            GREEN,
        ),
        line("Goal acceptance: PENDING", Color::LightRed),
        Default::default(),
    ];
    for execution in &graph.executions {
        facts.push(line(
            if graph.required.contains(&execution.work_id) {
                "REQUIRED"
            } else {
                "RELATED / non-blocking"
            },
            BLUE,
        ));
        facts.push(line(&execution.work_id, INK));
        facts.push(line(
            format!(
                "{:?} priority | cap {}K",
                execution.priority,
                execution.cap / 1000
            ),
            INK,
        ));
        facts.push(line(
            format!("{} / {} tokens", execution.used, execution.cap),
            INK,
        ));
        facts.push(line(
            execution.status.label(),
            if execution.status == RuntimeStatus::PausedAtCap {
                Color::LightYellow
            } else {
                GREEN
            },
        ));
        facts.push(Default::default());
    }
    if graph.required.is_empty() {
        facts.push(line("Break down the goal with", MUTED));
        facts.push(line("the Main agent.", MUTED));
    }
    frame.render_widget(Paragraph::new(facts), goal);
    let main = panel(frame, columns[1], "Main agent / Coordinate");
    let main_rows = Layout::vertical([Constraint::Min(1), Constraint::Length(5)]).split(main);
    let runtime = panel(frame, columns[2], "Compatibility / Work agent");
    let runtime_rows = Layout::vertical([
        Constraint::Length(8),
        Constraint::Min(1),
        Constraint::Length(5),
    ])
    .split(runtime);
    let execution = graph
        .executions
        .iter()
        .find(|execution| execution.work_id == COMPATIBILITY);
    let mut status = vec![];
    if let Some(execution) = execution {
        status.extend([
            line(
                format!(
                    "{} | checkpoint {}",
                    execution.status.label(),
                    execution.checkpoints
                ),
                GREEN,
            ),
            line(
                if graph.handoff.is_some() {
                    "Local compatibility worker"
                } else {
                    "Copilot / default model"
                },
                MUTED,
            ),
            line("Workspace: demo-compatibility", MUTED),
            line("Runtime: compatibility-executor-1", MUTED),
            line("Criterion: legacy quoting passes", INK),
            line(
                format!(
                    "Assignments: {} | Direct chats: {}",
                    execution.assignment_turns, execution.direct_chat_turns
                ),
                MUTED,
            ),
        ]);
        let evidence = saved
            .works
            .iter()
            .find(|work| work.id == COMPATIBILITY)
            .and_then(|work| work.evidence.last());
        status.push(line(
            match evidence {
                Some(evidence) if evidence.exit_code == 0 => "Check PASSED / acceptance pending",
                Some(_) => "Check FAILED / exit 1; repair needed",
                None => "Independent check: pending",
            },
            Color::LightYellow,
        ));
        if let Some(handoff) = &graph.handoff {
            status.push(line(format!("Decision handoff: {:?}", handoff.stage), BLUE));
        }
    } else {
        status.push(line("An executor will be bound to this Work.", MUTED));
    }
    frame.render_widget(
        Paragraph::new(status).wrap(Wrap { trim: false }),
        runtime_rows[0],
    );
    chat(frame, main_rows[0], &mut state.chat, "Main agent");
    if let Some(view) = state.views.get_mut(&Some(COMPATIBILITY.into())) {
        chat(frame, runtime_rows[1], view, "Work agent");
    }
    let main_input = panel(frame, main_rows[1], "To: Main agent");
    let work_input = panel(frame, runtime_rows[2], "To: Compatibility agent");
    if global {
        editor(frame, main_input, state, "Tab: next management request");
        frame.render_widget(
            Paragraph::new(line("F4: talk directly to this agent", MUTED)),
            work_input,
        );
    } else {
        editor(frame, work_input, state, "Tab: ask about the blocker");
        frame.render_widget(
            Paragraph::new(line("F1: manage without prompting executors", MUTED)),
            main_input,
        );
    }
    frame.render_widget(Paragraph::new(vec![
        line(if state.notice.is_empty() { " F1 Main agent   F4 Work agent   Tab Suggest   Ctrl+Q Close" } else { &state.notice }, BLUE),
        line(" Scripted prototype | Independent contexts and budgets | Simulated tokens, NOT provider quota", MUTED),
    ]), rows[2]);
}
