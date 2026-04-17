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
pub const IN_PROGRESS: Style = Style::new()
    .fg(Color::Yellow)
    .add_modifier(Modifier::BOLD)
    .add_modifier(Modifier::ITALIC);
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
// Card-style recommendation UI
pub const CARD_BORDER: Style = Style::new().fg(Color::DarkGray);
pub const CARD_BORDER_SELECTED: Style = Style::new().fg(Color::White);
pub const CARD_CODE: Style = Style::new().fg(Color::White);
pub const BUTTON: Style = Style::new().fg(Color::DarkGray);
pub const BUTTON_FOCUSED: Style = Style::new()
    .fg(Color::Black)
    .bg(Color::White)
    .add_modifier(Modifier::BOLD);
// Chat message dot indicators
pub const DOT_ERROR: Style = Style::new().fg(Color::Red).add_modifier(Modifier::BOLD);
pub const DOT_AGENT: Style = Style::new().fg(Color::Green).add_modifier(Modifier::BOLD);
// Notification badge/banner styles
pub const BADGE_CRITICAL: Style = Style::new().fg(Color::Red).add_modifier(Modifier::BOLD);
pub const BADGE_ACTIONABLE: Style = Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD);
pub const BADGE_INFO: Style = Style::new().fg(Color::DarkGray);
pub const BANNER_HINT: Style = Style::new().fg(Color::DarkGray);
