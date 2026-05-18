use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph},
    Frame,
};
use std::time::{SystemTime, UNIX_EPOCH};
use unicode_width::UnicodeWidthStr;

use crate::agent_sessions::{AgentSession, AgentSessionRegistry, AgentStatus, CliSource};
use crate::app::HistoryLoadState;
use crate::ui::shimmer;

// Figma palette — keep these in one place so the row renderer and any
// future status indicators stay in sync with the design tokens.
const ACCENT_CYAN:   Color = Color::Rgb(0x60, 0xcd, 0xff); // Selected-row title / cursor
const ACCENT_GREEN:  Color = Color::Rgb(0x6c, 0xcb, 0x5f); // Active status badge
const ACCENT_YELLOW: Color = Color::Rgb(0xfa, 0xe2, 0x46); // Waiting for input
const ACCENT_RED:    Color = Color::Rgb(0xff, 0x6b, 0x6b); // Error
const SOFT_WHITE:    Color = Color::Rgb(0x8b, 0x8b, 0x8b); // Idle
const MUTED_WHITE:   Color = Color::Rgb(0x8b, 0x8b, 0x8b); // 54% white — timestamp

pub fn render(
    f:    &mut Frame,
    area: Rect,
    reg:  &AgentSessionRegistry,
    list_state: &mut ListState,
    history_load_state: HistoryLoadState,
    activity_frame: usize,
) {
    // No in-TUI header: the "Agent sessions" title lives in the C++ agent
    // bar above this pane (AgentPaneContent::SetSessionsView), so we render
    // the list flush against the top of `area` and don't reserve any space
    // for chrome here.
    let list_area = area;

    let sorted = reg.iter_sorted();
    tracing::debug!(
        target: "agents_render",
        total = sorted.len(),
        first_three = ?sorted.iter().take(3).map(|s| (
            s.key.clone(),
            format!("{:?}", s.status),
            s.title.clone(),
        )).collect::<Vec<_>>(),
        area_w = area.width,
        area_h = area.height,
        load_state = ?history_load_state,
        "rendering agents view"
    );

    // While the lazy history scan is in flight, replace the whole list
    // with a single shimmer-styled loading row. Showing live rows alongside
    // a dim "loading…" hint led users to think the list was complete (only
    // the 1 live session) and dismiss the view before the scan finished.
    if history_load_state == HistoryLoadState::Loading {
        let mut spans: Vec<Span<'static>> = vec![Span::raw("  ")];
        spans.extend(shimmer::shimmer_spans("Loading...", activity_frame));
        let loading = Paragraph::new(Line::from(spans));
        f.render_widget(loading, list_area);
        return;
    }

    let selected = list_state.selected();
    let row_width = list_area.width as usize;
    let rows: Vec<ListItem> = sorted
        .into_iter()
        .enumerate()
        .map(|(i, s)| row_for(s, Some(i) == selected, row_width))
        .collect();

    // No `highlight_style` — selection is conveyed by the `>` prefix and
    // cyan title rendered inside `row_for`, mirroring the Figma cursor
    // rather than a full-row reverse-video bar.
    let list = List::new(rows);
    f.render_stateful_widget(list, list_area, list_state);
}

fn row_for(s: &AgentSession, selected: bool, row_width: usize) -> ListItem<'static> {
    let title_text  = display_title(s);
    let badge       = status_badge(s);
    let badge_style = badge_style(s);
    let age         = relative_age(s.last_activity_at);

    // Unselected rows: no `.fg(...)` override — fall through to the
    // terminal's default foreground so titles match the surrounding pane
    // text exactly (Color::White is ANSI white #7 and renders noticeably
    // dimmer than the default fg in most schemes, which made the list
    // look faded compared to a normal pane).
    //
    // Only the selection cursor is colored: cyan accent on the keyboard-
    // selected row. The status badge after the title (Active / Waiting for
    // input / Error) carries its own color, independent of selection — so
    // an unselected Working session still gets a cyan "Active" badge but
    // its title stays the default foreground.
    let title_style = if selected {
        Style::default().fg(ACCENT_CYAN)
    } else {
        Style::default()
    };

    // Leftmost column: `>` cursor for the selected row, blank otherwise.
    // Two cells (caret + space) so titles line up regardless of selection.
    let caret = if selected {
        Span::styled("> ", Style::default().fg(ACCENT_CYAN).add_modifier(Modifier::BOLD))
    } else {
        Span::raw("  ")
    };

    let cli_suffix = cli_suffix_for(s, selected);

    // Compose the row by measuring everything except trailing whitespace,
    // then padding to right-align the timestamp at row_width.
    let caret_w  = 2_usize;
    let title_w  = title_text.width();
    let badge_w  = if badge.is_empty() { 0 } else { badge.width() + 2 }; // "  badge"
    let cli_w    = if cli_suffix.is_empty() { 0 } else { cli_suffix.width() + 1 };
    let age_w    = age.width();
    let used     = caret_w + title_w + badge_w + cli_w + age_w;
    let pad      = row_width.saturating_sub(used).max(1);

    let mut spans = vec![
        caret,
        Span::styled(title_text, title_style),
    ];
    if !badge.is_empty() {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(badge, badge_style));
    }
    if !cli_suffix.is_empty() {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            cli_suffix,
            Style::default().fg(MUTED_WHITE).add_modifier(Modifier::DIM),
        ));
    }
    spans.push(Span::raw(" ".repeat(pad)));
    spans.push(Span::styled(age, Style::default().fg(MUTED_WHITE)));

    ListItem::new(Line::from(spans))
}

/// Clean session title for display. Falls back to the working-directory
/// basename when the agent hasn't surfaced a title yet (fresh sessions
/// before the first prompt).
fn display_title(s: &AgentSession) -> String {
    let raw = if s.title.is_empty() { cwd_basename(s) } else { s.title.clone() };
    // Cap at a reasonable width so a long prompt doesn't push the
    // timestamp off-screen on narrow panes. The ratatui List will wrap
    // anything we leave through; the truncation here is purely cosmetic.
    trunc(&raw, 64)
}

fn cwd_basename(s: &AgentSession) -> String {
    s.cwd.file_name().and_then(|n| n.to_str())
        .unwrap_or("?")
        .to_string()
}

/// Inline status text shown next to the title. Empty for Ended / Historical
/// rows — those carry no live state. Idle gets a soft "Idle" tag so the
/// user can tell at a glance that the session is bound to a pane but not
/// actively running a tool.
fn status_badge(s: &AgentSession) -> String {
    match s.status {
        AgentStatus::Working   => "Active".to_string(),
        AgentStatus::Attention => "Waiting for input".to_string(),
        AgentStatus::Error     => "Error".to_string(),
        AgentStatus::Idle      => "Idle".to_string(),
        AgentStatus::Ended | AgentStatus::Historical => String::new(),
    }
}

fn badge_style(s: &AgentSession) -> Style {
    match s.status {
        // "Active" reads as a healthy / running state, so green — leaving
        // cyan as the dedicated "selection cursor" color so the two don't
        // collide visually when a non-selected row is running a tool.
        AgentStatus::Working   => Style::default().fg(ACCENT_GREEN),
        AgentStatus::Attention => Style::default().fg(ACCENT_YELLOW),
        AgentStatus::Error     => Style::default().fg(ACCENT_RED),
        // Idle: muted off-white so it reads as a real status badge but
        // stays visually quieter than the colored Active/Waiting tags.
        AgentStatus::Idle      => Style::default().fg(SOFT_WHITE),
        AgentStatus::Ended | AgentStatus::Historical => Style::default(),
    }
}

/// Show the CLI provider (`copilot`, `claude`, `gemini`) only on the
/// active row or the keyboard-selected row — matches the Figma where the
/// agent icon appears only on the currently-engaged session and avoids
/// cluttering the historical list.
fn cli_suffix_for(s: &AgentSession, selected: bool) -> String {
    let surface = selected || matches!(s.status, AgentStatus::Working | AgentStatus::Attention);
    if !surface { return String::new(); }
    let label = match s.cli_source {
        CliSource::Claude  => "claude",
        CliSource::Copilot => "copilot",
        CliSource::Gemini  => "gemini",
        CliSource::Unknown(_) => return String::new(),
    };
    format!("· {}", label)
}

/// Human-readable age, matching the Figma:
///   < 60s   → "just now"
///   < 60m   → "N minute(s) ago"
///   < 24h   → "N hour(s) ago"
///   < 7d    → "N day(s) ago"
///   ≥ 7d    → "Month D, YYYY"   (UTC — close enough for week-old rows)
fn relative_age(t: SystemTime) -> String {
    let now = SystemTime::now();
    let secs = now.duration_since(t).map(|d| d.as_secs()).unwrap_or(0);
    if secs < 60 {
        "just now".to_string()
    } else if secs < 3600 {
        let n = secs / 60;
        format!("{} minute{} ago", n, plural(n))
    } else if secs < 86_400 {
        let n = secs / 3600;
        format!("{} hour{} ago", n, plural(n))
    } else if secs < 7 * 86_400 {
        let n = secs / 86_400;
        format!("{} day{} ago", n, plural(n))
    } else {
        format_calendar_date(t)
    }
}

fn plural(n: u64) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Format a SystemTime as "Month D, YYYY" in UTC. No chrono dep in wta —
/// uses Howard Hinnant's date algorithm (public domain) for the Gregorian
/// conversion. Returns "—" for pre-epoch / unreadable timestamps.
fn format_calendar_date(t: SystemTime) -> String {
    let secs = match t.duration_since(UNIX_EPOCH) {
        Ok(d)  => d.as_secs() as i64,
        Err(_) => return "—".to_string(),
    };
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    format!("{} {}, {}", month_name(m), d, y)
}

/// Civil date from days since the Unix epoch (1970-01-01).
/// Source: Hinnant, "chrono-Compatible Low-Level Date Algorithms".
fn civil_from_days(days: i64) -> (i32, u8, u8) {
    let z   = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = (z - era * 146_097) as u64;                                   // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;      // [0, 399]
    let y   = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);                      // [0, 365]
    let mp  = (5 * doy + 2) / 153;                                          // [0, 11]
    let d   = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let m   = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    let year = (y + if m <= 2 { 1 } else { 0 }) as i32;
    (year, m, d)
}

fn month_name(m: u8) -> &'static str {
    match m {
        1 => "January", 2 => "February", 3 => "March", 4 => "April",
        5 => "May", 6 => "June", 7 => "July", 8 => "August",
        9 => "September", 10 => "October", 11 => "November", 12 => "December",
        _ => "?",
    }
}

fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() }
    else { format!("{}…", s.chars().take(n.saturating_sub(1)).collect::<String>()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn relative_age_just_now_under_a_minute() {
        let t = SystemTime::now() - Duration::from_secs(5);
        assert_eq!(relative_age(t), "just now");
    }

    #[test]
    fn relative_age_singular_and_plural_minutes() {
        let t1 = SystemTime::now() - Duration::from_secs(60);
        assert_eq!(relative_age(t1), "1 minute ago");
        let t2 = SystemTime::now() - Duration::from_secs(180);
        assert_eq!(relative_age(t2), "3 minutes ago");
    }

    #[test]
    fn relative_age_days() {
        let t = SystemTime::now() - Duration::from_secs(3 * 86_400);
        assert_eq!(relative_age(t), "3 days ago");
    }

    #[test]
    fn relative_age_falls_back_to_calendar_date_after_a_week() {
        // 8 days ago — must produce a "Month D, YYYY" string, not "8 days ago".
        let t = SystemTime::now() - Duration::from_secs(8 * 86_400);
        let s = relative_age(t);
        assert!(s.contains(", "), "expected 'Month D, YYYY', got {:?}", s);
        assert!(!s.ends_with("ago"), "expected calendar date, got {:?}", s);
    }

    #[test]
    fn civil_from_days_matches_known_dates() {
        // Unix epoch.
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        // 2026-04-20 → days = 20_563 (verified against `date -u -d 2026-04-20 +%s` / 86400).
        assert_eq!(civil_from_days(20_563), (2026, 4, 20));
        // Leap-day handling: 2024-02-29.
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
    }

    #[test]
    fn format_calendar_date_renders_month_name() {
        let t = UNIX_EPOCH + Duration::from_secs(20_563 * 86_400);
        assert_eq!(format_calendar_date(t), "April 20, 2026");
    }
}
