// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

//! Goal-first board and brief editor for the isolated English presentation fixture.

use super::work_model::{editor, line, panel, BLUE, GREEN, INK, MUTED, PAPER};
use super::*;
use crate::agent_center::demo::scenario::work_board::{
    BLOCKED, CREATE, DONE, PLANNED, RUNNING, START,
};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

pub(super) fn show(state: &mut State) {
    home(state);
    state.task_list = true;
}

pub(super) fn histories(state: &mut State, saved: &Scenario) -> Result<()> {
    state.chat.messages = vec![message("assistant",
        "Manage outcomes, not session names. Your Work Board keeps every goal, completion criterion and status together. N creates a Work; opening one never restarts execution.")];
    for view in state.views.values_mut() {
        view.messages.clear();
        view.records.clear();
    }
    for work in &saved.works {
        push_work(
            state,
            &work.id,
            "assistant",
            format!(
                "Goal: {}\nStatus: {}\nNext: {}\nThis Work keeps its own definition and evidence.",
                work.goal, work.status, work.next_step
            ),
        );
    }
    for event in &saved.events {
        if event.kind == EventKind::UserRequest
            && event
                .input
                .as_deref()
                .is_some_and(|input| input.starts_with(CREATE) || input.starts_with(START))
        {
            let work = saved
                .works
                .iter()
                .find(|work| work.id == event.work_id)
                .context("Board history refers to an unknown Work")?;
            let response = if event
                .input
                .as_deref()
                .is_some_and(|input| input.starts_with(CREATE))
            {
                format!("Created Work: {}. Its goal and done condition are saved. It starts Planned; no execution session has been launched.", work.title)
            } else {
                format!("Started Work: {}. Execution is assigned to this Work; its completion condition is still pending.", work.title)
            };
            state.chat.messages.push(message("assistant", &response));
            push_work(state, &work.id, "coordinator", response);
        }
    }
    Ok(())
}

fn new_form(state: &mut State) -> Result<()> {
    state.form = Some(form::Form::new(
        json!({
            "id":"demo-new-work", "version":1, "kind":"IntakeRequest",
            "question":"Define the outcome you want to manage.",
            "responseSchema":{
                "type":"object","additionalProperties":false,
                "properties":{
                    "goal":{"type":"string","title":"Goal"},
                    "success":{"type":"string","title":"Done when"}
                },
                "required":["goal","success"]
            }
        }),
        None,
    )?);
    state.notice.clear();
    Ok(())
}

pub(super) fn key(
    state: &mut State,
    code: KeyCode,
    modifiers: KeyModifiers,
) -> Result<InputAction> {
    if let Some(form) = state.form.as_mut() {
        match code {
            KeyCode::Esc => state.form = None,
            KeyCode::Tab | KeyCode::BackTab => form.selected = 1 - form.selected,
            KeyCode::Enter if form.selected == 0 => form.selected = 1,
            KeyCode::Enter => {
                let operation = form.operation()?;
                return Ok(InputAction::Submit(format!(
                    "{CREATE}{}",
                    operation.params["value"]
                )));
            }
            KeyCode::Char(character)
                if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                let field = &mut form.fields[form.selected];
                field
                    .editor
                    .insert(&mut field.draft, character.encode_utf8(&mut [0; 4]));
            }
            _ => {
                let field = &mut form.fields[form.selected];
                field.editor.key(&mut field.draft, code, modifiers);
            }
        }
        return Ok(InputAction::None);
    }
    match code {
        KeyCode::F(2) | KeyCode::Esc => show(state),
        KeyCode::F(1) => home(state),
        KeyCode::Char('n' | 'N') if state.task_list || !state.context.global_conversation => {
            new_form(state)?
        }
        KeyCode::Char('s' | 'S') if state.task_list || !state.context.global_conversation => {
            let id = if state.task_list {
                &scenario(state)?
                    .works
                    .get(state.task_index)
                    .context("Select a Work")?
                    .id
            } else {
                state.context.work_id.as_ref().context("Select a Work")?
            };
            return Ok(InputAction::Submit(format!("{START}{id}")));
        }
        KeyCode::Up | KeyCode::Left if state.task_list => {
            state.task_index = state.task_index.saturating_sub(1)
        }
        KeyCode::Down | KeyCode::Right if state.task_list => {
            state.task_index =
                (state.task_index + 1).min(scenario(state)?.works.len().saturating_sub(1));
        }
        KeyCode::Enter if state.task_list => {
            let id = scenario(state)?
                .works
                .get(state.task_index)
                .context("Select a Work")?
                .id
                .clone();
            open_work(state, &id)?;
        }
        KeyCode::Enter if state.context.global_conversation => {
            let input = state.editor_view_mut().draft.trim().to_owned();
            if input.eq_ignore_ascii_case("new work") {
                new_form(state)?;
            } else {
                return Ok(InputAction::Submit(input));
            }
        }
        KeyCode::Char(character)
            if state.context.global_conversation
                && !state.task_list
                && !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            state
                .editor_view_mut()
                .insert(character.encode_utf8(&mut [0; 4]));
        }
        _ if state.context.global_conversation && !state.task_list => {
            let view = state.editor_view_mut();
            view.editor.key(&mut view.draft, code, modifiers);
        }
        _ => {}
    }
    Ok(InputAction::None)
}

fn status_color(status: &str) -> Color {
    match status {
        BLOCKED => Color::LightRed,
        RUNNING => BLUE,
        DONE => GREEN,
        _ => MUTED,
    }
}

fn progress(work: &Work) -> String {
    format!(
        "{} / {} completion criteria met",
        work.steps
            .iter()
            .filter(|step| step.status == StepStatus::Completed)
            .count(),
        work.steps.len()
    )
}

fn card(frame: &mut ratatui::Frame<'_>, area: Rect, work: &Work, selected: bool) {
    let color = status_color(&work.status);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(if selected {
            BLUE
        } else {
            Color::Rgb(57, 67, 83)
        }))
        .title(line(
            format!(
                " {}{} ",
                if selected { "> " } else { "" },
                work.status.to_uppercase()
            ),
            color,
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let sections = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(2),
        Constraint::Min(1),
    ])
    .split(inner);
    frame.render_widget(
        Paragraph::new(line(&work.title, INK))
            .style(Style::new().add_modifier(Modifier::BOLD))
            .wrap(Wrap { trim: false }),
        sections[0],
    );
    frame.render_widget(Paragraph::new(line(progress(work), color)), sections[1]);
    let mut facts = vec![];
    if let Some(blocker) = work.blockers.first() {
        facts.push(line(format!("Blocked: {blocker}"), Color::LightRed));
    }
    facts.push(line(format!("Next: {}", work.next_step), MUTED));
    frame.render_widget(
        Paragraph::new(facts).wrap(Wrap { trim: false }),
        sections[2],
    );
}

fn board(frame: &mut ratatui::Frame<'_>, area: Rect, state: &mut State) {
    let saved = &state.demo.as_ref().expect("board checked").scenario;
    let columns: usize = if area.width >= 100 { 2 } else { 1 };
    let rows = usize::from(area.height / 12).max(1).min(3);
    let capacity = columns * rows;
    state.task_index = state.task_index.min(saved.works.len().saturating_sub(1));
    let first = state.task_index / capacity * capacity;
    let row_areas = Layout::vertical(vec![Constraint::Fill(1); rows]).split(area);
    for (row, row_area) in row_areas.iter().enumerate() {
        let cards = Layout::horizontal(vec![Constraint::Fill(1); columns]).split(*row_area);
        for (column, area) in cards.iter().enumerate() {
            let index = first + row * columns + column;
            if let Some(work) = saved.works.get(index) {
                card(frame, *area, work, index == state.task_index);
            } else if index == saved.works.len() {
                let inner = panel(frame, *area, "+ NEW WORK / N");
                frame.render_widget(
                    Paragraph::new(vec![
                        line("What do you want to get done?", INK),
                        line("", INK),
                        line("Give it a goal and a done condition.", MUTED),
                        line("No session name or agent setup required.", MUTED),
                    ])
                    .wrap(Wrap { trim: false }),
                    inner,
                );
            }
        }
    }
}

fn detail(frame: &mut ratatui::Frame<'_>, area: Rect, state: &mut State) {
    let Some(work) = state.demo.as_ref().and_then(|demo| {
        demo.scenario
            .works
            .iter()
            .find(|work| Some(&work.id) == state.context.work_id.as_ref())
    }) else {
        return;
    };
    let sections = Layout::vertical([Constraint::Length(5), Constraint::Min(1)]).split(area);
    frame.render_widget(
        Paragraph::new(vec![
            line(&work.goal, INK),
            line(
                format!("{}  |  {}", work.status.to_uppercase(), progress(work)),
                status_color(&work.status),
            ),
            line(format!("Next: {}", work.next_step), MUTED),
        ])
        .wrap(Wrap { trim: false }),
        sections[0],
    );
    let columns = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(sections[1]);
    let criteria = panel(frame, columns[0], "DONE WHEN / EVIDENCE");
    let mut lines = vec![];
    for step in &work.steps {
        let (label, color) = match step.status {
            StepStatus::Completed => ("PASS", GREEN),
            StepStatus::Blocked => ("FAIL", Color::LightRed),
            StepStatus::Pending => ("TODO", MUTED),
        };
        lines.push(line(format!("[{label}] {}", step.title), color));
        if let Some(evidence) = work
            .evidence
            .iter()
            .rev()
            .find(|evidence| evidence.step_id == step.id)
        {
            lines.push(line(
                format!("  {} / exit {}", evidence.command, evidence.exit_code),
                MUTED,
            ));
        }
        lines.push(Default::default());
    }
    lines.push(line(format!("Acceptance: {}", work.acceptance), MUTED));
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), criteria);
    let history = panel(frame, columns[1], "WORK MEMORY / EXECUTION DETAILS");
    work_graph::chat(frame, history, state.view_mut(), "Work");
}

fn create_form(frame: &mut ratatui::Frame<'_>, area: Rect, state: &mut State) {
    let Some(form) = state.form.as_mut() else {
        return;
    };
    let width = area.width.min(100);
    let height = area.height.min(23);
    let rect = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, rect);
    frame.render_widget(Block::default().style(Style::new().bg(PAPER)), rect);
    let inner = panel(frame, rect, "NEW WORK / DEFINE AN OUTCOME");
    let fields = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(5),
        Constraint::Length(5),
        Constraint::Length(3),
        Constraint::Min(1),
    ])
    .split(inner);
    frame.render_widget(
        Paragraph::new(line("What will you remember this work for?", INK)),
        fields[0],
    );
    for (index, field) in form.fields.iter_mut().enumerate() {
        let focused = index == form.selected;
        let input = panel(
            frame,
            fields[index + 1],
            if index == 0 {
                "GOAL / required, up to 100 characters"
            } else {
                "DONE WHEN / required, up to 240 characters"
            },
        );
        field.editor.width = usize::from(input.width.max(1));
        let layout = field.editor.layout(&field.draft);
        let (row, column) = layout.cursor(field.editor.cursor);
        let scroll = row.saturating_sub(usize::from(input.height).saturating_sub(1));
        frame.render_widget(
            Paragraph::new(layout.lines).scroll((scroll as u16, 0)),
            input,
        );
        if focused && input.width > 0 && input.height > 0 {
            frame.set_cursor_position((
                input.x + column.min(usize::from(input.width - 1)) as u16,
                input.y
                    + row
                        .saturating_sub(scroll)
                        .min(usize::from(input.height - 1)) as u16,
            ));
        }
    }
    frame.render_widget(
        Paragraph::new(vec![
            line(
                "Create -> Planned. Start execution separately when ready.",
                MUTED,
            ),
            line(
                "Tab: switch fields   Enter: next / create   Esc: cancel",
                BLUE,
            ),
        ]),
        fields[3],
    );
    frame.render_widget(
        Paragraph::new(line(&state.notice, Color::LightRed)).wrap(Wrap { trim: false }),
        fields[4],
    );
}

pub(in crate::agent_center::ui) fn render(frame: &mut ratatui::Frame<'_>, state: &mut State) {
    let area = frame.area();
    frame.render_widget(Block::default().style(Style::new().bg(PAPER).fg(INK)), area);
    if area.width < 76 || area.height < 28 {
        frame.render_widget(
            Paragraph::new("Work Board: enlarge the window to at least 76 x 28."),
            area,
        );
        return;
    }
    let rows = Layout::vertical([
        Constraint::Length(6),
        Constraint::Min(1),
        Constraint::Length(4),
    ])
    .split(area);
    let Some(saved) = state.demo.as_ref().map(|demo| &demo.scenario) else {
        return;
    };
    let count = |status| {
        saved
            .works
            .iter()
            .filter(|work| work.status == status)
            .count()
    };
    frame.render_widget(
        Paragraph::new(vec![
            line(" AGENT CENTER / WORK BOARD", BLUE),
            line(" Your goals. Their status. What needs you next.", INK),
            line("", INK),
            line(
                format!(
                    " {} WORKS    {} BLOCKED    {} RUNNING    {} PLANNED    {} DONE",
                    saved.works.len(),
                    count(BLOCKED),
                    count(RUNNING),
                    count(PLANNED),
                    count(DONE)
                ),
                BLUE,
            ),
            line(
                " N: New Work    Enter: Open selected Work    F1: Main agent    F2: All Work",
                MUTED,
            ),
        ]),
        rows[0],
    );
    if state.task_list {
        board(frame, rows[1], state);
    } else if state.context.global_conversation {
        let main = panel(frame, rows[1], "MAIN AGENT / MANAGE WORK");
        let parts = Layout::vertical([Constraint::Min(1), Constraint::Length(5)]).split(main);
        work_graph::chat(frame, parts[0], &mut state.chat, "Main agent");
        let input = panel(frame, parts[1], "REQUEST / new work or show work overview");
        editor(frame, input, state, "new work");
    } else {
        detail(frame, rows[1], state);
    }
    frame.render_widget(
        Paragraph::new(vec![
            line(&state.notice, Color::LightRed),
            line(
                " Arrows Select | Enter Open | N New | S Start | F2 Board | Ctrl+Q Close",
                BLUE,
            ),
            line(
                " Goals, status and evidence stay with Work, not a session.",
                MUTED,
            ),
            line(
                " Work story demo | Scripted execution; no live agents.",
                MUTED,
            ),
        ])
        .wrap(Wrap { trim: false }),
        rows[2],
    );
    create_form(frame, area, state);
}
