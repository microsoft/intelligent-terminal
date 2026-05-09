use ratatui::{
    layout::Rect,
    style::{Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState},
    Frame,
};
use std::time::SystemTime;

use crate::agent_sessions::{AgentSession, AgentSessionRegistry, AgentStatus};

pub fn render(
    f:    &mut Frame,
    area: Rect,
    reg:  &AgentSessionRegistry,
    list_state: &mut ListState,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Agents  (F2 / Ctrl+Tab to switch · ↑↓ select · Enter activate · Del remove) ");

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
        "rendering agents view"
    );
    let rows: Vec<ListItem> = sorted.into_iter().map(row_for).collect();
    let list = List::new(rows)
        .block(block)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(list, area, list_state);
}

fn row_for(s: &AgentSession) -> ListItem<'static> {
    let title  = format!(
        "{}-{}-{}",
        short_key(&s.key),
        cli_label(s),
        if s.title.is_empty() { cwd_basename(s) } else { s.title.clone() },
    );
    let status = status_label(s);
    let age    = relative_age(s.last_activity_at);

    let dim = matches!(s.status, AgentStatus::Ended | AgentStatus::Historical);
    let title_style  = if dim { Style::default().dim() } else { Style::default() };
    let status_style = match s.status {
        AgentStatus::Working   => Style::default().yellow(),
        AgentStatus::Attention => Style::default().magenta(),
        AgentStatus::Error     => Style::default().red(),
        _ => Style::default(),
    };

    let line = Line::from(vec![
        Span::styled(format!("{:<48}", trunc(&title, 48)), title_style),
        Span::raw("  "),
        Span::styled(format!("{:<10}", status), status_style),
        Span::raw("  "),
        Span::styled(format!("{:>4}", age), Style::default().dim()),
    ]);
    ListItem::new(line)
}

/// Return the first 8 characters of the session key, stripping the
/// synthetic `pane:` prefix used for placeholder rows.
fn short_key(key: &str) -> String {
    let stripped = key.strip_prefix("pane:").unwrap_or(key);
    stripped.chars().take(8).collect()
}

fn cli_label(s: &AgentSession) -> &'static str {
    use crate::agent_sessions::CliSource::*;
    match s.cli_source {
        Claude  => "claude",
        Copilot => "copilot",
        Gemini  => "gemini",
        _       => "agent",
    }
}

fn cwd_basename(s: &AgentSession) -> String {
    s.cwd.file_name().and_then(|n| n.to_str())
        .unwrap_or("?")
        .to_string()
}

fn status_label(s: &AgentSession) -> &'static str {
    match s.status {
        AgentStatus::Idle       => "IDLE",
        AgentStatus::Working    => "WORKING",
        AgentStatus::Attention  => "ATTENTION",
        AgentStatus::Error      => "ERROR",
        AgentStatus::Ended      => "",
        AgentStatus::Historical => "",
    }
}

fn relative_age(t: SystemTime) -> String {
    let secs = SystemTime::now().duration_since(t).map(|d| d.as_secs()).unwrap_or(0);
    if secs < 60        { format!("{}s",  secs) }
    else if secs < 3600 { format!("{}m",  secs / 60) }
    else if secs < 86400{ format!("{}h",  secs / 3600) }
    else                { format!("{}d",  secs / 86400) }
}

fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() }
    else { format!("{}…", s.chars().take(n.saturating_sub(1)).collect::<String>()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_key_takes_first_eight_chars_of_uuid() {
        assert_eq!(short_key("7d0be6ce-71bc-44a3-98f8-9b976846642e"), "7d0be6ce");
    }

    #[test]
    fn short_key_strips_pane_prefix_then_takes_eight() {
        assert_eq!(short_key("pane:abcdef0123-4567-89"), "abcdef01");
    }

    #[test]
    fn short_key_handles_short_keys_without_padding() {
        // Demo-data style keys are shorter than 8 chars after stripping;
        // we just return whatever we have.
        assert_eq!(short_key("demo"), "demo");
        assert_eq!(short_key(""), "");
    }

    #[test]
    fn short_key_unicode_safe() {
        // Take by char (not byte) so multi-byte chars don't panic.
        assert_eq!(short_key("αβγδεζηθικ"), "αβγδεζηθ");
    }
}
