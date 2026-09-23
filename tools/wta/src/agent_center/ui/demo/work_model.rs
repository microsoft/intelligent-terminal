// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

//! Conversation-first presentation of the isolated English Work fixture.

use super::*;
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph, Wrap},
};

pub(super) const INK: Color = Color::Rgb(220, 224, 232);
pub(super) const PAPER: Color = Color::Rgb(18, 21, 27);
pub(super) const MUTED: Color = Color::Rgb(148, 159, 178);
pub(super) const BLUE: Color = Color::Rgb(99, 190, 245);
pub(super) const GREEN: Color = Color::Rgb(141, 208, 158);

pub(super) fn main_input(input: &str) -> Option<bool> {
    match input {
        "Continue fixing issue #4821"
        | "Investigate compatibility"
        | "Add 10K budget"
        | "Review progress"
        | "Is yesterday's fix ready to merge?"
        | "Proceed. Keep the legacy API unchanged." => Some(true),
        "Keep the legacy API unchanged."
        | "Why are you paused?"
        | "Show evidence"
        | "Show work state"
        | "Show budget"
        | "Why did compatibility fail?" => Some(false),
        _ => None,
    }
}

pub(super) fn histories(state: &mut State, saved: &Scenario) -> Result<()> {
    state.chat.messages = vec![message("assistant",
        "I coordinate your Works and their execution. Fix Issue #4821 needs a compatibility investigation. Continue the work you remember.")];
    for view in state.views.values_mut() {
        view.messages.clear();
        view.records.clear();
    }
    push_work(state, FIX, "assistant",
        "I am this Work's execution runtime. Workspace: microsoft/foo / fix-4821. Unit tests passed; shell compatibility failed. You can talk to me here.");
    let mut replay = Scenario::new();
    for event in &saved.events {
        replay.replay_event(event)?;
        if !replay.is_work_model() {
            continue;
        }
        if event.kind == EventKind::BudgetReached {
            push_work(state, FIX, "assistant", format!(
                "{}K cap reached. I paused execution and saved the findings. The legacy shell adapter still needs investigation.",
                replay.budget.limit / 1000));
            state.chat.messages.push(message("assistant", format!(
                "Fix #4821 needs your decision: its runtime reached {}K tokens. Findings saved; acceptance is still pending. Authorize another 10K, or leave the Work paused.",
                replay.budget.limit / 1000)));
        } else if event.kind == EventKind::BudgetConsumed {
            push_work(
                state,
                FIX,
                "activity",
                format!(
                    "Investigating legacy shell compatibility. Recorded usage: {} / {} tokens.",
                    replay.budget.consumed, replay.budget.limit
                ),
            );
        } else if event.kind == EventKind::UserRequest {
            let input = event.input.as_deref().context("Missing Work-model input")?;
            if matches!(input, "Open Work model" | "Open merge story") {
                continue;
            }
            let response = match input {
                "Is yesterday's fix ready to merge?" =>
                    if replay.awaiting_cap() {
                        "Not ready to merge. Implementation is finished and 3 unit tests pass, but the legacy compatibility gate fails.\nBoth results belong to Fix #4821; a completed agent response is not acceptance.\nI recommend continuing with its existing Work agent in microsoft/foo / fix-4821. Preserve the legacy API, keep High priority, and use the displayed 20K cap.\nAwaiting your approval.".into()
                    } else {
                        "Not ready to merge. The same Work still has failing compatibility evidence. An earlier approval or more runtime activity does not change acceptance; a passing recheck is required.".into()
                    },
                "Proceed. Keep the legacy API unchanged." =>
                    "Approved: preserve the legacy API within this Work's 20K cap. I delegated the compatibility investigation to its existing runtime, with the failed check and your instruction attached. Open F4 to talk to that agent.".into(),
                "Why did compatibility fail?" =>
                    "The legacy adapter requires double-quoted arguments; the implementation returns single quotes. The unit tests check the implementation contract, not that delivery gate.\nI have the failing check and your instruction here. I will investigate without silently changing compatibility policy. Acceptance remains blocked until a passing recheck.".into(),
                "Continue fixing issue #4821" =>
                    "Found the same Work and its bound runtime. Compatibility has not passed. Confirm its cap, then ask me to investigate. Opening it does not restart paused execution.".into(),
                "Investigate compatibility" =>
                    "Delegated compatibility investigation to this Work's runtime, within its confirmed cap. I will track the outcome against the acceptance criteria. F4 opens the executing agent.".into(),
                "Add 10K budget" => format!(
                    "Your authorization is recorded. Cap: {}K. The same Work runtime continues with its instruction, findings and conversation intact.",
                    replay.budget.limit / 1000),
                "Review progress" =>
                    "Not accepted yet: unit tests pass, compatibility fails, and delivery is pending. The runtime must provide new passing evidence before this Work can be accepted.".into(),
                "Keep the legacy API unchanged." =>
                    "Understood. I saved that instruction on this Work. I will investigate a compatible fix, not change the legacy API. Existing failing evidence remains until a recheck proves otherwise.".into(),
                "Why are you paused?" => format!(
                    "I reached this Work's {}K cap, not a completed outcome. Findings saved: compatibility fails for the legacy shell adapter. Ask the Main agent to authorize more budget; my context stays here.",
                    replay.budget.limit / 1000),
                "Show evidence" => evidence(&replay.works[0]),
                "Show work state" => work_summary(&replay.works[0]),
                "Show budget" => format!("{} / {} tokens; usage belongs to this Work.", replay.budget.consumed, replay.budget.limit),
                _ => format!("Work token cap confirmed: {}K. Recorded usage retained.", replay.budget.limit / 1000),
            };
            if main_input(input) == Some(true) {
                state.chat.messages.push(message("user", input));
                state.chat.messages.push(message("assistant", response));
                if matches!(
                    input,
                    "Investigate compatibility"
                        | "Add 10K budget"
                        | "Proceed. Keep the legacy API unchanged."
                ) {
                    push_work(
                        state,
                        FIX,
                        "coordinator",
                        if input == "Proceed. Keep the legacy API unchanged." {
                            "Main agent -> Work agent: investigate the attached failed compatibility check. Keep the legacy API unchanged. Work: fix-4821; workspace: microsoft/foo / fix-4821; cap: 20K."
                        } else {
                            "Main agent -> Work agent: investigate compatibility within the authorized Work budget."
                        },
                    );
                    push_work(state, FIX, "assistant",
                        "I am investigating in microsoft/foo / fix-4821. Same Work, same execution binding. I will report findings here.");
                }
            } else {
                push_work(state, FIX, "user", input);
                push_work(state, FIX, "assistant", response);
            }
        }
    }
    Ok(())
}

pub(super) fn key(state: &mut State, code: KeyCode) -> Result<Option<InputAction>> {
    match code {
        KeyCode::F(1) => home(state),
        KeyCode::F(2) | KeyCode::F(4) => open_work(state, FIX)?,
        KeyCode::F(5) if state.menu.is_none() => {
            if state.context.global_conversation {
                open_work(state, FIX)?;
            }
            state.focus = if state.focus == Focus::Details {
                Focus::Composer
            } else {
                Focus::Details
            };
            state.view_mut().details_scroll = 0;
        }
        KeyCode::F(6) => {
            if state.menu.is_some() {
                close_menu(state);
            } else {
                anyhow::ensure!(
                    scenario(state)?.resumed,
                    "Continue the Work before changing its budget"
                );
                let focus = state.focus;
                state
                    .demo
                    .as_mut()
                    .context("Missing demo presentation")?
                    .menu_return_focus = Some(focus);
                state.menu = Some(cap_menu(scenario(state)?)?);
            }
        }
        KeyCode::F(8) | KeyCode::F(7) if state.menu.is_none() => {
            home(state);
            return Ok(Some(InputAction::Submit(
                if code == KeyCode::F(8) {
                    "Investigate compatibility"
                } else {
                    "Add 10K budget"
                }
                .into(),
            )));
        }
        KeyCode::Tab if state.menu.is_none() && state.focus == Focus::Composer => {
            let next = if state.context.global_conversation {
                scenario(state)?.next_input()
            } else if scenario(state)?.is_merge_story() {
                "Why did compatibility fail?"
            } else if scenario(state)?.budget.paused {
                "Why are you paused?"
            } else {
                "Keep the legacy API unchanged."
            };
            state.editor_view_mut().replace_draft(next.into());
        }
        _ => return Ok(None),
    }
    Ok(Some(InputAction::None))
}

pub(super) fn line(text: impl Into<String>, color: Color) -> Line<'static> {
    Line::styled(text.into(), Style::new().fg(color))
}

pub(super) fn panel(frame: &mut ratatui::Frame<'_>, rect: Rect, title: &str) -> Rect {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(Color::Rgb(57, 67, 83)))
        .title(line(format!(" {title} "), BLUE));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    inner
}

pub(in crate::agent_center::ui) fn render(frame: &mut ratatui::Frame<'_>, state: &mut State) {
    let area = frame.area();
    frame.render_widget(Block::default().style(Style::new().bg(PAPER).fg(INK)), area);
    if area.width < 76 || area.height < 22 {
        frame.render_widget(
            Paragraph::new("Work story demo\nEnlarge the window to view conversations (76 x 22)."),
            area,
        );
        return;
    }
    let Some(saved) = state.demo.as_ref().map(|demo| &demo.scenario) else {
        return;
    };
    let Some(work) = saved.works.first() else {
        return;
    };
    let Some(definition) = work.definition.as_ref() else {
        return;
    };
    let global = state.context.global_conversation;
    let running = saved
        .clock
        .as_ref()
        .is_some_and(|clock| clock.budget_due_ms.is_some());
    let runtime_status = if saved.budget.paused {
        "Paused at token cap"
    } else if running {
        "Running"
    } else {
        "Ready"
    };
    let rows = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(0),
        Constraint::Length(2),
    ])
    .split(area);
    frame.render_widget(
        Paragraph::new(line(
            " INTELLIGENT TERMINAL  /  Main agent -> Work -> Execution",
            BLUE,
        )),
        rows[0],
    );
    let nav_width = if area.width >= 120 { 20 } else { 0 };
    let context_width = if area.width >= 120 { 32 } else { 28 };
    let columns = Layout::horizontal([
        Constraint::Length(nav_width),
        Constraint::Min(30),
        Constraint::Length(context_width),
    ])
    .split(rows[1]);
    if nav_width > 0 {
        let nav = panel(frame, columns[0], "Navigate");
        frame.render_widget(
            Paragraph::new(vec![
                line(
                    if global {
                        "> Main agent"
                    } else {
                        "  Main agent"
                    },
                    BLUE,
                ),
                line("  F1 Coordinate", MUTED),
                Line::default(),
                line("WORKS", MUTED),
                Line::default(),
                line(if global { "  Fix #4821" } else { "> Fix #4821" }, INK),
                line("  F4 Work agent", MUTED),
                Line::default(),
                line(
                    if saved.budget.paused {
                        "  Needs budget"
                    } else if running {
                        "  Running"
                    } else {
                        "  Ready"
                    },
                    GREEN,
                ),
                Line::default(),
                line("One Work.", MUTED),
                line("One bound runtime.", MUTED),
            ]),
            nav,
        );
    }
    let context = panel(frame, columns[2], "Work context");
    let mut facts = vec![
        line(&work.title, INK),
        line("ID: fix-4821", MUTED),
        Line::default(),
        line("Workspace", BLUE),
        line(&definition.workspace, INK),
        Line::default(),
        line("Agent runtime", BLUE),
        line("Copilot / default model", INK),
        line(runtime_status, GREEN),
        line("Bound to this Work", MUTED),
        Line::default(),
        line(format!("Priority: {}", definition.priority), INK),
        line(
            format!("Token cap: {}K tokens", saved.budget.limit / 1000),
            INK,
        ),
        line(
            format!("{} / {} tokens", saved.budget.consumed, saved.budget.limit),
            INK,
        ),
        Line::default(),
        line("Acceptance criteria", BLUE),
        line("PASS  Unit tests", GREEN),
        line("FAIL  Shell compatibility", INK),
        line("PENDING  Delivery", MUTED),
        line("Acceptance: Pending", INK),
    ];
    if saved.is_merge_story() {
        facts.insert(2, line("Merge: BLOCKED", Color::LightRed));
    }
    if definition.execution_instruction.is_some() {
        facts.extend([
            Line::default(),
            line("Instruction saved", BLUE),
            line("Keep legacy API unchanged.", INK),
        ]);
    }
    if saved.budget.paused {
        facts.extend([Line::default(), line("Findings saved", GREEN)]);
    }
    frame.render_widget(Paragraph::new(facts).wrap(Wrap { trim: false }), context);
    let main = panel(
        frame,
        columns[1],
        if global {
            "Main agent / Coordinate"
        } else {
            "Fix #4821 / Work agent"
        },
    );
    let sections = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(1),
        Constraint::Length(5),
    ])
    .split(main);
    frame.render_widget(
        Paragraph::new(vec![
            line(
                if global {
                    "Coordinates Work; delegates execution."
                } else {
                    "Agent runtime: Copilot / default model"
                },
                BLUE,
            ),
            line(
                if global {
                    format!("Fix #4821 runtime: {runtime_status}")
                } else {
                    format!("{runtime_status} | Same execution binding | Simulated")
                },
                GREEN,
            ),
        ])
        .wrap(Wrap { trim: false }),
        sections[0],
    );
    let mut text = vec![];
    let modal = state.menu.is_some();
    let details = state.focus == Focus::Details;
    if let Some(menu) = &state.menu {
        text.push(line("Token cap / Work control", BLUE));
        text.push(line("Changing the cap never resets usage.", MUTED));
        text.push(Line::default());
        for (index, (item, _)) in menu.items.iter().enumerate() {
            text.push(line(
                format!("{} {item}", if index == menu.selected { ">" } else { " " }),
                INK,
            ));
            text.push(Line::default());
        }
    } else if details {
        text.push(line(
            "Acceptance criteria / Recorded fixture evidence",
            BLUE,
        ));
        text.push(Line::default());
        for (criterion, step) in definition.acceptance_criteria.iter().zip([
            "unit-tests",
            "compatibility",
            "documentation",
        ]) {
            text.push(line(criterion, INK));
            if let Some(evidence) = work.evidence.iter().rev().find(|item| item.step_id == step) {
                text.push(line(
                    format!("{} | exitCode: {}", evidence.command, evidence.exit_code),
                    MUTED,
                ));
            } else {
                text.push(line("PENDING", MUTED));
            }
            text.push(Line::default());
        }
        text.push(line("Agent finished != Work accepted.", BLUE));
    } else {
        let view = if global {
            &state.chat
        } else {
            state
                .views
                .get(&state.context.work_id)
                .unwrap_or(&state.chat)
        };
        for message in &view.messages {
            let role = message["role"].as_str().unwrap_or("assistant");
            let (name, color) = match role {
                "user" => ("You", INK),
                "coordinator" => ("Delegation", BLUE),
                "activity" => ("Runtime activity / simulated", MUTED),
                _ if global => ("Main agent", BLUE),
                _ => ("Work agent", GREEN),
            };
            text.push(Line::styled(
                name,
                Style::new().fg(color).add_modifier(Modifier::BOLD),
            ));
            if let Some(body) = message["text"].as_str() {
                text.extend(body.lines().map(|value| line(value, INK)));
            }
            text.push(Line::default());
        }
    }
    let paragraph = Paragraph::new(text).wrap(Wrap { trim: false });
    let max_scroll = paragraph
        .line_count(sections[1].width)
        .saturating_sub(usize::from(sections[1].height))
        .min(usize::from(u16::MAX)) as u16;
    let scroll = if modal {
        0
    } else if details {
        let view = state.view_mut();
        view.details_scroll = view.details_scroll.min(max_scroll);
        view.details_scroll
    } else {
        let view = state.reading_view_mut();
        if view.follow {
            view.scroll = max_scroll;
        }
        view.scroll = view.scroll.min(max_scroll);
        view.scroll
    };
    frame.render_widget(paragraph.scroll((scroll, 0)), sections[1]);
    let input = panel(
        frame,
        sections[2],
        if global {
            "To: Main agent"
        } else {
            "To: Fix #4821 / Work agent"
        },
    );
    if modal || details {
        frame.render_widget(
            Paragraph::new(line("Esc: return to conversation", MUTED)),
            input,
        );
    } else {
        editor(
            frame,
            input,
            state,
            if global {
                "Tab: next coordinator request"
            } else {
                "Tab: talk to this Work agent"
            },
        );
    }
    frame.render_widget(
        Paragraph::new(vec![
            line(
                if state.notice.is_empty() {
                    " F1 Main agent  F4 Work agent  F5 Evidence  F6 Cap  Tab Suggest  Ctrl+Q Close"
                } else {
                    &state.notice
                },
                if state.notice.is_empty() {
                    MUTED
                } else {
                    Color::LightRed
                },
            ),
            line(
                " Work story demo | Scripted agents, execution and tokens | No live provider calls",
                MUTED,
            ),
        ]),
        rows[2],
    );
}

pub(super) fn editor(frame: &mut ratatui::Frame<'_>, input: Rect, state: &mut State, hint: &str) {
    let view = state.editor_view_mut();
    view.editor.width = usize::from(input.width.max(1));
    let layout = view.editor.layout(&view.draft);
    let (row, column) = layout.cursor(view.editor.cursor);
    let scroll = row.saturating_sub(usize::from(input.height).saturating_sub(1));
    let mut lines: Vec<_> = layout
        .lines
        .into_iter()
        .skip(scroll)
        .take(usize::from(input.height))
        .collect();
    for row in &mut lines {
        for span in &mut row.spans {
            span.style = span.style.fg(INK);
        }
    }
    if view.draft.is_empty() {
        lines = vec![line(hint, MUTED)];
    }
    frame.render_widget(Paragraph::new(lines), input);
    if !input.is_empty() {
        frame.set_cursor_position((
            input.x + (column as u16).min(input.width - 1),
            input.y + (row - scroll) as u16,
        ));
    }
}
