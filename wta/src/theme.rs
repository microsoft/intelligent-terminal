use ratatui::style::{Color, Modifier, Style};

// Colors matching AcpConnection.cpp ANSI codes
pub const USER_PROMPT: Style = Style::new().fg(Color::Green);
pub const INPUT_TEXT: Style = Style::new().fg(Color::White);
pub const AGENT_TEXT: Style = Style::new().fg(Color::White);
pub const SYSTEM_TEXT: Style = Style::new().fg(Color::Cyan);
pub const TOOL_CALL: Style = Style::new().fg(Color::DarkGray);
pub const PLAN_STYLE: Style = Style::new().fg(Color::Cyan);
pub const PERMISSION: Style = Style::new().fg(Color::Yellow);
pub const ERROR_STYLE: Style = Style::new().fg(Color::Red);
pub const STATUS_CONNECTED: Style = Style::new().fg(Color::Green);
pub const STATUS_CONNECTING: Style = Style::new().fg(Color::Yellow);
pub const STATUS_DISCONNECTED: Style = Style::new().fg(Color::DarkGray);
pub const STATUS_FAILED: Style = Style::new().fg(Color::Red);
pub const DIM: Style = Style::new().fg(Color::DarkGray);
pub const SELECTED: Style = Style::new()
    .fg(Color::Black)
    .bg(Color::Yellow)
    .add_modifier(Modifier::BOLD);
pub const DEBUG_SENT: Style = Style::new().fg(Color::Green);
pub const DEBUG_RECEIVED: Style = Style::new().fg(Color::Cyan);
pub const RECOMMENDATION_TITLE: Style = Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD);
pub const RECOMMENDATION_DETAIL: Style = Style::new().fg(Color::Gray);
