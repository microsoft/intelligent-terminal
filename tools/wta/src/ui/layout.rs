use ratatui::prelude::*;
use crate::app::{App, AppMode, View, DEFAULT_TAB_ID};

use super::{auth, agents_view, chat, command_popup, debug_panel, input, permission, recommendations, setup};

pub fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();

    // Auth mode: show auth screen above the input box
    if app.mode == AppMode::Auth {
        let input_height = {
            let tab = app.current_tab();
            input::input_height(&tab.input, tab.cursor_pos, area.width)
        };
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(input_height),
            ])
            .split(area);
        auth::render(frame, app, chunks[0]);
        input::render(frame, app, chunks[1]);
        return;
    }

    // Setup mode: unified setup wizard (FRE + preflight) with input box
    if app.mode == AppMode::Setup {
        let input_height = {
            let tab = app.current_tab();
            input::input_height(&tab.input, tab.cursor_pos, area.width)
        };
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(input_height),
            ])
            .split(area);
        setup::render(frame, app, chunks[0]);
        input::render(frame, app, chunks[1]);
        return;
    }

    // Agents view (F2) takes over the full pane area; chat / input / debug
    // panel are not drawn in this mode. Per-tab: the active tab's
    // TabSession owns the open state and selection cursor. Disjoint-field
    // borrow (agent_sessions vs. tab_sessions[id]) lets us pass both refs
    // through without going through current_tab_mut() (which would borrow
    // the whole App and conflict with &app.agent_sessions).
    if app.current_tab().current_view == View::Agents {
        let tab_id = app.tab_id.as_deref().unwrap_or(DEFAULT_TAB_ID).to_string();
        let load_state = app.history_load_state;
        let activity_frame = app.activity_frame as usize;
        let cli_filter = app.current_cli_filter();
        let tab = app.tab_sessions.entry(tab_id).or_default();
        agents_view::render(
            frame,
            area,
            &app.agent_sessions,
            &mut tab.agents_list_state,
            load_state,
            activity_frame,
            cli_filter.as_ref(),
        );
        return;
    }

    let (main_area, debug_area) = if app.show_debug_panel {
        let h = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
            .split(area);
        (h[0], Some(h[1]))
    } else {
        (area, None)
    };

    let rec_panel_h = if app.current_tab().turn.recommendations().is_some() {
        app.rec_panel_height(main_area.width)
    } else {
        0
    };
    let perm_panel_h = app.permission_panel_height(main_area.width);
    let input_height = {
        let tab = app.current_tab();
        input::input_height(&tab.input, tab.cursor_pos, main_area.width)
    };

    // Expire the transient hint independently, then decide whether to
    // reserve a row for either the transient hint or the welcome hint.
    let now = std::time::Instant::now();
    let transient_visible = app
        .transient_hint
        .as_ref()
        .map(|(_, deadline)| now < *deadline)
        .unwrap_or(false);
    if !transient_visible {
        app.transient_hint = None;
    }
    let welcome_visible = app.show_welcome_hint
        && app.state == crate::app::ConnectionState::Connected;
    let hint_visible = welcome_visible || transient_visible;
    let hint_h: u16 = if hint_visible { 1 } else { 0 };
    let rec_hint_h: u16 = if app.current_tab().turn.recommendations().is_some() { 1 } else { 0 };

    // The host (Windows Terminal) renders the agent bar in XAML above this
    // pane, so wta uses the full pane area for chat / recommendations / input.
    //
    // Layout: chat sized to its content, rec panel right below, blank
    // filler, optional one-row transient hint, optional one-row rec nav
    // hint (sits directly above the input box whenever recs are visible),
    // input at the bottom. Cap chat at `pane_height - rec - input - hints`
    // so the recommendation card always renders in full — chat_scroll lets
    // the user reach older history if it overflows.
    let chat_content_width = main_area.width.saturating_sub(2); // h_chat 1+1 padding
    let chat_estimate = chat::estimated_block_height(app, chat_content_width);
    let reserved_below = rec_panel_h
        .saturating_add(perm_panel_h)
        .saturating_add(input_height)
        .saturating_add(hint_h)
        .saturating_add(rec_hint_h);
    let chat_max = main_area.height.saturating_sub(reserved_below).max(1);
    let chat_height = chat_estimate.min(chat_max);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(chat_height),
            Constraint::Length(rec_panel_h),
            Constraint::Length(perm_panel_h),
            Constraint::Min(0),
            Constraint::Length(hint_h),
            Constraint::Length(rec_hint_h),
            Constraint::Length(input_height),
        ])
        .split(main_area);

    // Horizontal padding for chat, recommendations, and permission
    let h_chat = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(1), Constraint::Min(0), Constraint::Length(1)])
        .split(chunks[0]);
    let h_rec = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(1), Constraint::Min(0), Constraint::Length(1)])
        .split(chunks[1]);
    let h_perm = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(1), Constraint::Min(0), Constraint::Length(1)])
        .split(chunks[2]);

    chat::render(frame, app, h_chat[1]);
    app.sync_rec_scroll_max(main_area.width);
    recommendations::render(frame, app, h_rec[1]);
    if !app.current_tab().permission.is_empty() {
        permission::render(frame, app, h_perm[1]);
    }

    if hint_visible {
        if welcome_visible {
            let line = Line::from(Span::styled(
                t!("layout.welcome_hint").into_owned(),
                Style::default().fg(Color::DarkGray),
            ));
            frame.render_widget(line, chunks[4]);
        } else if let Some((text, _)) = app.transient_hint.as_ref() {
            let line = Line::from(Span::styled(
                format!("  {}", text),
                Style::default().fg(Color::DarkGray),
            ));
            frame.render_widget(line, chunks[4]);
        }
    }
    if app.current_tab().turn.recommendations().is_some() {
        recommendations::render_hint(frame, chunks[5]);
    }
    input::render(frame, app, chunks[6]);

    if let Some(debug_area) = debug_area {
        debug_panel::render(frame, app, debug_area);
    }

    // Slash-command autocomplete: anchored above the input box. Drawn into
    // the filler row between the perm panel and the hint row.
    if let Some(popup_state) = app.command_popup_state() {
        command_popup::render_popup(frame, popup_state, chunks[3]);
    }

    // `/help` overlay sits on top of everything so the user can always
    // dismiss it with Esc.
    command_popup::render_help_overlay(frame, app, area);
}

pub fn input_cursor_position(app: &App, area: Rect) -> Option<Position> {
    // Agents view / Setup view: no input box, so no cursor.
    if app.current_tab().current_view == View::Agents || app.mode == AppMode::Setup {
        return None;
    }

    // Placeholder state: the input box renders its own static white-bg /
    // black-fg cell at the prompt position (see ui::input::render). Hide
    // the real terminal cursor so WT doesn't overlay its focused-pane
    // block on top — that block fully replaces the cell content and would
    // hide the painted glyph (in unfocused panes WT draws a hollow outline
    // and the cell shows through, which is what we already get for free
    // by hiding the cursor in both focus states).
    if app.current_tab().input.is_empty() {
        return None;
    }

    let main_area = if app.show_debug_panel {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
            .split(area)[0]
    } else {
        area
    };

    let rec_height = if app.current_tab().turn.recommendations().is_some() {
        Constraint::Length(app.rec_panel_height(main_area.width))
    } else {
        Constraint::Length(0)
    };
    let perm_height = Constraint::Length(app.permission_panel_height(main_area.width));
    let input_height = {
        let tab = app.current_tab();
        input::input_height(&tab.input, tab.cursor_pos, main_area.width)
    };

    // Match the constraint layout in `render` — chat / rec / perm / filler /
    // hint / rec_hint / input, so the input chunk is at index 6. Keep both
    // in lockstep or the cursor lands on the wrong line.
    let now = std::time::Instant::now();
    let hint_visible = app
        .transient_hint
        .as_ref()
        .map(|(_, deadline)| now < *deadline)
        .unwrap_or(false);
    let hint_height = if hint_visible {
        Constraint::Length(1)
    } else {
        Constraint::Length(0)
    };
    let rec_hint_height = if app.current_tab().turn.recommendations().is_some() {
        Constraint::Length(1)
    } else {
        Constraint::Length(0)
    };

    let chat_content_width = main_area.width.saturating_sub(2);
    let chat_height = chat::estimated_block_height(app, chat_content_width);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(chat_height),
            rec_height,
            perm_height,
            Constraint::Min(0),
            hint_height,
            rec_hint_height,
            Constraint::Length(input_height),
        ])
        .split(main_area);

    input::cursor_position(app, chunks[6])
}
