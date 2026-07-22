use crate::app::{App, AppMode, View, DEFAULT_TAB_ID};
use ratatui::prelude::*;

use super::{
    agent_popup, agents_view, auth, chat, command_popup, debug_panel, input, model_popup,
    permission, recommendations, setup,
};

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
            .constraints([Constraint::Min(1), Constraint::Length(input_height)])
            .split(area);
        auth::render(frame, app, chunks[0]);
        input::render(frame, app, chunks[1]);
        return;
    }

    // Setup mode: diagnostic install/sign-in/retry flow with input box.
    if app.mode == AppMode::Setup {
        let input_height = {
            let tab = app.current_tab();
            input::input_height(&tab.input, tab.cursor_pos, area.width)
        };
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(input_height)])
            .split(area);
        setup::render(frame, app, chunks[0]);
        input::render(frame, app, chunks[1]);
        return;
    }

    // agent session view takes over the full pane area; chat / input / debug
    // panel are not drawn in this mode. Per-tab: the active tab's
    // TabSession owns the open state and selection cursor. Disjoint-field
    // borrow (agent_sessions vs. tab_sessions[id]) lets us pass both refs
    // through without going through current_tab_mut() (which would borrow
    // the whole App and conflict with &app.agent_sessions).
    if app.current_tab().current_view == View::Agents {
        let tab_id = app.tab_id.as_deref().unwrap_or(DEFAULT_TAB_ID).to_string();
        let activity_frame = app.activity_frame as usize;
        let cli_filter = app.current_cli_filter();
        let origin_filter = app.sessions_origin_filter;
        let pane_focused = app.pane_focused;
        let tab = app.tab_sessions.entry(tab_id).or_default();
        // Show the loading shimmer while waiting on the very first
        // `session/list` response from master (empty placeholder snapshot +
        // refetch in flight) OR while an F5 rescan is in flight — so F5 gives
        // visible feedback even when the list already has rows. A normal 5s
        // poll keeps `rescan_in_flight` false and therefore does not flash it.
        let show_loading = tab.agents_view.refetch_in_flight
            && (tab
                .agents_view
                .snapshot
                .as_deref()
                .map(|s| s.is_empty())
                .unwrap_or(false)
                || tab.agents_view.rescan_in_flight);
        agents_view::render(
            frame,
            area,
            &app.agent_sessions,
            tab.agents_view.snapshot.as_deref(),
            &mut tab.agents_list_state,
            activity_frame,
            cli_filter.as_ref(),
            origin_filter,
            show_loading,
            pane_focused,
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
    let welcome_visible =
        app.show_welcome_hint && app.state == crate::app::ConnectionState::Connected;
    let hint_visible = welcome_visible || transient_visible;
    let hint_h: u16 = if hint_visible { 1 } else { 0 };
    let rec_hint_h: u16 = if app.current_tab().turn.recommendations().is_some() {
        1
    } else {
        0
    };

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
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(chunks[0]);
    let h_rec = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(chunks[1]);
    let h_perm = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(chunks[2]);

    chat::render(frame, app, h_chat[1]);
    app.sync_rec_scroll_max(main_area.width);
    recommendations::render(frame, app, h_rec[1]);
    if !app.current_tab().permission.is_empty() {
        permission::render(frame, app, h_perm[1]);
    }

    if hint_visible {
        // The hint is a single non-wrapping row; in a narrow pane the raw
        // string overruns the width and ratatui clips it mid-token. Truncate
        // with an ellipsis instead so it always reads as a deliberate, if
        // shortened, line rather than a chopped-off fragment (issue #126).
        let hint_width = chunks[4].width as usize;
        if welcome_visible {
            let line = Line::from(Span::styled(
                truncate_to_width(&t!("layout.welcome_hint"), hint_width),
                Style::default().fg(Color::DarkGray),
            ));
            frame.render_widget(line, chunks[4]);
        } else if let Some((text, _)) = app.transient_hint.as_ref() {
            let line = Line::from(Span::styled(
                truncate_to_width(&format!("  {}", text), hint_width),
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

    // Slash-command autocomplete: pinned directly above the input box
    // (`chunks[6]`). Anchoring to the input box rather than the filler row
    // keeps the popup glued to the input regardless of how much empty space
    // sits above it — otherwise a short chat leaves a tall filler and the
    // popup floats far up the pane (worst in side-by-side layouts).
    if let Some(popup_state) = app.command_popup_state() {
        command_popup::render_popup(frame, popup_state, chunks[6]);
    }

    // `/model` picker modal: same anchor as the autocomplete popup. The two
    // are mutually exclusive — opening the picker clears the input, so the
    // command popup isn't visible while it's up.
    if let Some(model_state) = app.model_popup_state() {
        model_popup::render_popup(frame, model_state, chunks[6]);
    }

    if let Some(agent_state) = app.agent_popup_state() {
        agent_popup::render_popup(frame, agent_state, chunks[6]);
    }

    // `/help` overlay sits on top of everything so the user can always
    // dismiss it with Esc.
    command_popup::render_help_overlay(frame, app, area);
}

/// Truncate `s` so its rendered (display-cell) width fits in `max` columns,
/// appending a single-cell ellipsis when anything was dropped. Width-aware
/// (not char-count) so localized hints containing wide CJK glyphs are clipped
/// at the right column instead of overrunning the pane. The returned string is
/// guaranteed to have a display width of at most `max`.
fn truncate_to_width(s: &str, max: usize) -> String {
    use unicode_width::UnicodeWidthChar;

    let total: usize = s
        .chars()
        .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
        .sum();
    if total <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }

    // Reserve one cell for the ellipsis glyph (width 1).
    let budget = max - 1;
    let mut out = String::new();
    let mut width = 0usize;
    for c in s.chars() {
        let cw = UnicodeWidthChar::width(c).unwrap_or(0);
        if width + cw > budget {
            break;
        }
        out.push(c);
        width += cw;
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::truncate_to_width;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn shorter_than_max_is_unchanged() {
        assert_eq!(truncate_to_width("hello", 10), "hello");
        assert_eq!(truncate_to_width("hello", 5), "hello");
    }

    #[test]
    fn longer_gets_ellipsis_and_fits() {
        let out = truncate_to_width("hello world", 5);
        assert_eq!(out, "hell…");
        assert!(UnicodeWidthStr::width(out.as_str()) <= 5);
    }

    #[test]
    fn zero_width_is_empty() {
        assert_eq!(truncate_to_width("hello", 0), "");
    }

    #[test]
    fn wide_glyphs_never_overrun() {
        // CJK glyphs are 2 cells each; result must still fit the budget.
        let out = truncate_to_width("你好世界你好", 5);
        assert!(UnicodeWidthStr::width(out.as_str()) <= 5);
        assert!(out.ends_with('…'));
    }
}
