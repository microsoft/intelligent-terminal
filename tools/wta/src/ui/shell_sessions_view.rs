//! `/shell-sessions` restore view.
//!
//! Full-pane list of the durable shell sessions Windows Terminal saved on tab
//! close (fetched from master's SQLite store via the `shell_sessions/list` ext
//! method — see `master/shell_sessions_db.rs`). Rendered full-pane like the
//! agent-session view (`agents_view.rs`), reached via
//! [`View::ShellSessions`](crate::app::View::ShellSessions), so chat / input /
//! the AI disclaimer are hidden while choosing a session. Enter restores the
//! highlighted tab (its layout, working directory, and scrollback); Esc
//! dismisses. Key handling lives in `App::handle_key`.

use ratatui::prelude::*;
use ratatui::widgets::{List, ListItem, Paragraph};
use std::time::{Duration, UNIX_EPOCH};
use unicode_width::UnicodeWidthStr;

use crate::session_registry::ShellSessionInfo;
use crate::theme;
use crate::ui::agents_view::relative_age;

// Selected-row accent, matching the agent-session view (`agents_view.rs`): a
// cyan `>` caret + cyan title, rather than a filled highlight bar — so the two
// full-pane lists read identically.
const ACCENT_CYAN: Color = Color::Cyan;
// Muted color for the trailing "last update" timestamp, matching agents_view.
const MUTED_WHITE: Color = Color::Rgb(0x8b, 0x8b, 0x8b);

/// Per-frame render state captured from the [`App`](crate::app::App).
pub struct ShellSessionsViewState<'a> {
    /// Saved sessions, newest first (ordered by master's `list`).
    pub sessions: &'a [ShellSessionInfo],
    /// Highlighted row, an index into `sessions`.
    pub selected: usize,
    /// `Some(name)` while a delete confirmation is pending for the selected
    /// row — the view renders a confirm prompt instead of the normal hint.
    pub confirm_delete: Option<&'a str>,
}

/// Render the restore view flush against the top of `area`, mirroring the
/// agent-session view's chrome-light layout: a title row, the list, then a
/// footer hint. Assumes a non-empty list (the app only enters this view when
/// sessions are present).
pub fn render(frame: &mut Frame, state: ShellSessionsViewState<'_>, area: Rect) {
    // Indent two columns from the pane's left edge, matching agents_view.
    let inner = Rect {
        x: area.x + 2,
        y: area.y,
        width: area.width.saturating_sub(2),
        height: area.height,
    };

    // Rows: [title][blank][list ...][blank][hint]
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title
            Constraint::Length(1), // spacer
            Constraint::Min(1),    // list
            Constraint::Length(1), // spacer
            Constraint::Length(1), // hint
        ])
        .split(inner);

    // TODO(localize): extract these strings to i18n catalog keys.
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Restore shell session",
            theme::INPUT_TEXT.add_modifier(Modifier::BOLD),
        ))),
        chunks[0],
    );

    // Build each row manually (no `highlight_style`): selection is conveyed by
    // a cyan `>` caret + cyan name, exactly like the agent-session view — not a
    // filled highlight bar. A muted "last update" age is right-aligned at the
    // row's trailing edge, matching agents_view. Rows are already newest-first
    // (master's `list` orders by `saved_at DESC`).
    let row_width = chunks[2].width as usize;
    let items: Vec<ListItem> = state
        .sessions
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let selected = i == state.selected;
            // The row awaiting delete confirmation (always the selected one) is
            // shown in red so the target is unmistakable.
            let pending_delete = state.confirm_delete.is_some() && selected;
            let caret = if selected {
                let color = if pending_delete { Color::Red } else { ACCENT_CYAN };
                Span::styled("> ", Style::default().fg(color))
            } else {
                Span::raw("  ")
            };
            let name_style = if pending_delete {
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
            } else if selected {
                Style::default().fg(ACCENT_CYAN)
            } else {
                theme::INPUT_TEXT
            };

            // "last update" age from the saved_at unix timestamp.
            let age = if entry.saved_at > 0 {
                relative_age(UNIX_EPOCH + Duration::from_secs(entry.saved_at as u64))
            } else {
                String::new()
            };

            let name = entry.name.clone();
            let cwd_part = if entry.cwd.is_empty() {
                String::new()
            } else {
                format!("  {}", entry.cwd)
            };

            // Right-align the age: pad between the left content and the age so
            // the timestamp sits flush at the row's trailing edge.
            let left_width = 2 + name.width() + cwd_part.width();
            let age_width = age.width();
            let pad = row_width
                .saturating_sub(left_width)
                .saturating_sub(age_width)
                .max(1);

            let mut spans = vec![caret, Span::styled(name, name_style)];
            if !cwd_part.is_empty() {
                spans.push(Span::styled(cwd_part, theme::DIM));
            }
            if !age.is_empty() {
                spans.push(Span::raw(" ".repeat(pad)));
                spans.push(Span::styled(age, Style::default().fg(MUTED_WHITE)));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    frame.render_widget(List::new(items), chunks[2]);

    // Footer: normally the key hints; while a delete is pending, a confirm
    // prompt (in a warning color) replaces them.
    let hint = match state.confirm_delete {
        Some(name) => Line::from(Span::styled(
            format!("Delete shell session '{}'? Enter = confirm • Esc = cancel", name),
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        )),
        None => Line::from(Span::styled(
            "↑ ↓ move • Enter restore • D delete • Esc cancel",
            theme::DIM,
        )),
    };
    frame.render_widget(Paragraph::new(hint), chunks[4]);
}
