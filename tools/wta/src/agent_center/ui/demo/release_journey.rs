// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

//! Stable conversation + Work surface for the isolated English release presentation.

use super::work_model::{editor, line, panel, BLUE, GREEN, INK, MUTED, PAPER};
use super::*;
use crate::agent_center::demo::scenario::release_journey::*;
use ratatui::{
    layout::{Constraint, Layout},
    style::{Color, Style},
    widgets::{Block, Paragraph, Wrap},
};

pub(super) fn histories(state: &mut State, saved: &Scenario) -> Result<()> {
    state.chat.messages.clear();
    let mut replay = Scenario::new();
    for event in &saved.events {
        replay.replay_event(event)?;
        let Some(journey) = &replay.release_journey else {
            continue;
        };
        match event.kind {
            EventKind::UserRequest => {
                let input = event.input.as_deref().context("Missing release input")?;
                if !matches!(input, OPEN | OPEN_PARALLEL) {
                    state.chat.messages.push(message("user", input));
                }
                if input == PROGRESS {
                    state
                        .chat
                        .messages
                        .push(message("assistant", &replay.release_progress()));
                } else if input != OVERVIEW {
                    state
                        .chat
                        .messages
                        .push(message("assistant", journey.input_response(input)));
                }
            }
            EventKind::ReleaseExecutorUpdate => {
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
        KeyCode::F(1) => {
            home(state);
            state.task_list = false;
        }
        KeyCode::F(2) => {
            if scenario(state)?.works.is_empty() {
                state.notice = "Agree on the goal and create the Work first.".into();
            } else {
                state.task_list = true;
            }
        }
        KeyCode::Tab => {
            let input = scenario(state)?.next_input();
            state.chat.replace_draft(input.into());
        }
        KeyCode::F(_) => {
            state.notice = "F1: Main agent and Work | F2: Work overview | Tab: next request".into();
        }
        _ => return Ok(None),
    }
    Ok(Some(InputAction::None))
}

pub(in crate::agent_center::ui) fn render(frame: &mut ratatui::Frame<'_>, state: &mut State) {
    let area = frame.area();
    frame.render_widget(Block::default().style(Style::new().bg(PAPER).fg(INK)), area);
    if area.width < 110 || area.height < 32 {
        frame.render_widget(
            Paragraph::new("Release journey: enlarge to at least 110 x 32."),
            area,
        );
        return;
    }
    let Some(saved) = state.demo.as_ref().map(|demo| &demo.scenario) else {
        return;
    };
    let Some(journey) = &saved.release_journey else {
        return;
    };
    let rows = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(0),
        Constraint::Length(2),
    ])
    .split(area);
    frame.render_widget(
        Paragraph::new(vec![
            line(
                " AGENT CENTER  /  FROM A RELEASE GOAL TO TRACKED WORK",
                BLUE,
            ),
            line(
                " You manage the outcome. The main agent coordinates. Working agents execute.",
                MUTED,
            ),
        ]),
        rows[0],
    );
    let columns =
        Layout::horizontal([Constraint::Percentage(53), Constraint::Percentage(47)]).split(rows[1]);
    let chat = panel(frame, columns[0], "MAIN AGENT / YOUR MANAGEMENT ENTRY");
    let chat_rows = Layout::vertical([Constraint::Min(0), Constraint::Length(6)]).split(chat);
    if journey.diagnosis.is_some() {
        render_portfolio(frame, columns[1], saved);
        if state.task_list {
            let count = saved
                .works
                .iter()
                .filter(|work| work.parent_work_id.is_none())
                .count();
            let mut lines = vec![
                line(format!("ALL WORK / {count} INDEPENDENT GOALS"), BLUE),
                line("", INK),
            ];
            for work in saved
                .works
                .iter()
                .filter(|work| work.parent_work_id.is_none())
            {
                lines.extend([
                    line(&work.title, BLUE),
                    line(&work.status, GREEN),
                    line(&work.next_step, INK),
                    line("", INK),
                ]);
            }
            lines.extend([
                line("YOU: goals, decisions, acceptance.", INK),
                line("SYSTEM: models, sessions, execution, handoffs.", MUTED),
                line("", INK),
                line("Each Work retains its own context and evidence.", INK),
            ]);
            frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), chat);
        } else {
            super::work_graph::chat(frame, chat_rows[0], &mut state.chat, "Main agent");
            editor(
                frame,
                chat_rows[1],
                state,
                "Manage your Work here. Execution continues independently.",
            );
        }
        footer(frame, rows[2], state);
        return;
    }
    let work = panel(
        frame,
        columns[1],
        if state.task_list {
            "MY WORK / GOALS, NOT SESSIONS"
        } else {
            "RELEASE WORK / GOAL + DONE WHEN + STATUS"
        },
    );
    let mut facts = vec![];
    let mut fact = |text: &str, color| facts.push(line(text.to_string(), color));
    if journey.stage == Stage::Welcome {
        fact("START WITH YOUR GOAL", BLUE);
        fact("", INK);
        fact("What outcome do you want to achieve?", INK);
        fact("", INK);
        fact("We will define the Work together.", MUTED);
        fact("No Work created. No sessions started.", MUTED);
    } else if journey.stage == Stage::Clarifying {
        fact("FIRST: AGREE ON THE OUTCOME", BLUE);
        fact("", INK);
        fact("Goal", MUTED);
        fact("Ready fix #4821 for release", INK);
        fact("", INK);
        fact("Proposed completion criteria", BLUE);
        fact("[ ] Unit tests pass", INK);
        fact("[ ] Legacy compatibility passes", INK);
        fact("", INK);
        fact("Constraint to confirm", BLUE);
        fact("Preserve the double-quote contract?", INK);
        fact("", INK);
        fact("No Work created. No sessions started.", MUTED);
    } else {
        fact("Ready fix #4821 for release", BLUE);
        fact("", INK);
        let status = if journey.stage == Stage::Plan {
            "DRAFT / AWAITING YOUR CONFIRMATION"
        } else {
            saved.works[0].status.as_str()
        };
        fact(
            status,
            if journey.stage == Stage::Blocked {
                Color::LightRed
            } else {
                GREEN
            },
        );
        fact("", INK);
        let passed =
            usize::from(journey.unit == Some(0)) + usize::from(journey.compatibility == Some(0));
        fact(&format!("RELEASE CRITERIA: {passed} / 2 PASSED"), BLUE);
        for (title, result) in [
            ("Unit tests", journey.unit),
            ("Legacy compatibility", journey.compatibility),
        ] {
            fact(
                &format!(
                    "{}  {}{}",
                    if result == Some(0) {
                        "[PASS]"
                    } else if result.is_some() {
                        "[FAIL]"
                    } else {
                        "[....]"
                    },
                    title,
                    result
                        .map(|code| format!(" / exit {code}"))
                        .unwrap_or_default()
                        .as_str()
                ),
                if result == Some(0) {
                    GREEN
                } else if result.is_some() {
                    Color::LightRed
                } else {
                    INK
                },
            );
        }
        fact("", INK);
        fact("TWO EXECUTION TASKS / ONE RELEASE GOAL", BLUE);
        for (i, name) in [
            (1, "Implementation + unit tests"),
            (2, "Independent compatibility check"),
        ] {
            fact(name, INK);
            fact(
                if journey.stage == Stage::Plan {
                    "  Planned / agent assigned after approval"
                } else {
                    if i == 1 {
                        "  Implementation agent"
                    } else {
                        "  Verification agent"
                    }
                },
                MUTED,
            );
            if let Some(child) = saved.works.get(i) {
                fact(&format!("  {}", child.status), GREEN);
            }
            fact("", INK);
        }
        fact("WORK MEMORY / AGREED BOUNDARY", BLUE);
        fact("Preserve legacy double quotes.", INK);
        fact("Both checks must pass before release.", INK);
        if journey.attempt == 2 {
            fact("Repair authorized. Prior failure retained.", INK);
        }
        if matches!(journey.stage, Stage::Ready | Stage::Blocked) {
            fact("", INK);
            fact("LATEST CHECK OUTPUT / node --test", BLUE);
            for child in saved.works.iter().skip(1).take(2) {
                if let Some(evidence) = child.evidence.last() {
                    let counts = evidence
                        .summary
                        .lines()
                        .filter(|line| {
                            line.starts_with("# tests ")
                                || line.starts_with("# pass ")
                                || line.starts_with("# fail ")
                        })
                        .collect::<Vec<_>>()
                        .join("  ");
                    fact(
                        &format!(
                            "{}: {counts}",
                            if child.id == "release-implementation" {
                                "Unit"
                            } else {
                                "Legacy"
                            }
                        ),
                        INK,
                    );
                }
            }
        }
        fact("", INK);
        if journey.stage == Stage::Ready {
            fact("NEXT: YOUR RELEASE APPROVAL", GREEN);
            fact("No publish or merge has been performed.", MUTED);
        } else if journey.stage == Stage::Blocked {
            fact("NEEDS YOU: approve the compatible repair", Color::LightRed);
        } else if journey.stage == Stage::Plan {
            fact("Confirm the Work, not session settings.", MUTED);
        } else {
            fact("Progress follows evidence, not replies.", MUTED);
        }
    }
    frame.render_widget(Paragraph::new(facts).wrap(Wrap { trim: false }), work);
    if state.task_list {
        let status = saved
            .works
            .first()
            .map(|work| work.status.as_str())
            .unwrap_or("No Work yet");
        frame.render_widget(
            Paragraph::new(vec![
                line("ALL WORK / 1 RELEASE GOAL", BLUE),
                line("", INK),
                line("Ready fix #4821 for release", INK),
                line(status, GREEN),
                line("Goal, progress, decision and evidence together.", MUTED),
                line("", INK),
                line("THE MANAGEMENT MODEL", BLUE),
                line("", INK),
                line("Before: Human -> Agent sessions -> Work", MUTED),
                line("", INK),
                line("Now: Human -> Work -> Working agents", GREEN),
                line("", INK),
                line("You define the goal and make decisions.", INK),
                line("The main agent coordinates execution.", INK),
                line("Sessions serve the Work.", INK),
            ])
            .wrap(Wrap { trim: false }),
            chat,
        );
    } else {
        super::work_graph::chat(frame, chat_rows[0], &mut state.chat, "Main agent");
        editor(
            frame,
            chat_rows[1],
            state,
            "Tell the main agent what you want to achieve...",
        );
    }
    footer(frame, rows[2], state);
}

fn footer(frame: &mut ratatui::Frame<'_>, area: ratatui::layout::Rect, state: &State) {
    frame.render_widget(Paragraph::new(vec![
        line(if state.notice.is_empty() { " F1 Main agent  |  F2 Work overview  |  Tab Next request  |  Ctrl+Q Exit" } else { &state.notice }, BLUE),
        line(" Work story demo | Scripted coordination and bindings; real local Node checks; no live agents.", MUTED),
    ]), area);
}

fn render_portfolio(frame: &mut ratatui::Frame<'_>, area: ratatui::layout::Rect, saved: &Scenario) {
    let Some(journey) = &saved.release_journey else {
        return;
    };
    let Some(diagnosis) = &journey.diagnosis else {
        return;
    };
    let count = saved
        .works
        .iter()
        .filter(|work| work.parent_work_id.is_none())
        .count();
    let blocked = saved
        .works
        .iter()
        .filter(|work| work.parent_work_id.is_none() && work.status == "Blocked")
        .count();
    let rows = Layout::vertical([
        Constraint::Length(2),
        Constraint::Fill(1),
        Constraint::Fill(1),
    ])
    .split(area);
    frame.render_widget(
        Paragraph::new(line(
            format!(" ALL WORK / {count} GOALS / {blocked} BLOCKED"),
            BLUE,
        )),
        rows[0],
    );
    let release = panel(frame, rows[1], "WORK A / RELEASE");
    let passed =
        usize::from(journey.unit == Some(0)) + usize::from(journey.compatibility == Some(0));
    let mut lines = vec![
        line("Ready fix #4821 for release", BLUE),
        line(
            &saved.works[0].status,
            if journey.stage == Stage::Blocked {
                Color::LightRed
            } else {
                GREEN
            },
        ),
        line(format!("{passed} / 2 release gates passed"), BLUE),
    ];
    for (name, result) in [
        ("Unit tests", journey.unit),
        ("Compatibility", journey.compatibility),
    ] {
        lines.push(line(
            format!(
                "{name}: {}",
                result
                    .map(|code| format!(
                        "{} / exit {code}",
                        if code == 0 { "PASS" } else { "FAIL" }
                    ))
                    .unwrap_or_else(|| "Pending".into())
            ),
            if result.is_some_and(|code| code != 0) {
                Color::LightRed
            } else {
                INK
            },
        ));
    }
    lines.extend([
        line("", INK),
        line(
            "Execution: implementation + independent verification",
            MUTED,
        ),
        line("Memory: preserve legacy double quotes.", INK),
        line(
            if journey.attempt == 2 {
                "Repair approved; previous failure retained."
            } else {
                "Both checks must pass. No release performed."
            },
            INK,
        ),
        line("", INK),
        line(
            if journey.stage == Stage::Blocked {
                "NEEDS YOU: approve compatible repair"
            } else if journey.stage == Stage::Ready {
                "NEEDS YOU: release approval"
            } else {
                "No action needed; execution is progressing."
            },
            BLUE,
        ),
    ]);
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), release);
    let area = panel(
        frame,
        rows[2],
        if diagnosis.stage == DiagnosisStage::Draft {
            "NEW WORK DRAFT / CONFIRM BEFORE STARTING"
        } else {
            "WORK B / DIAGNOSIS"
        },
    );
    let mut lines = vec![
        line("Explain the startup warning", BLUE),
        line("Done when: cause + safe recommendation.", INK),
        line("Boundary: read-only; do not change files.", MUTED),
        line("", INK),
    ];
    if let Some(work) = saved.works.iter().find(|work| work.id == DIAGNOSIS) {
        lines.push(line(&work.status, GREEN));
        if work.findings.is_empty() {
            lines.push(line("Own execution context. Release task unchanged.", INK));
            lines.push(line(&work.next_step, BLUE));
        } else {
            for finding in &work.findings {
                lines.push(line(finding, INK));
            }
            lines.push(line("No files changed.", MUTED));
            lines.push(line("NEEDS YOU: review diagnosis", BLUE));
        }
    } else {
        lines.push(line("Awaiting your confirmation.", BLUE));
        lines.push(line("No new execution has started.", MUTED));
        lines.push(line("Release execution continues independently.", INK));
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}
