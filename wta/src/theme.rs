use ratatui::style::{Color, Modifier, Style};

// Colors matching AcpConnection.cpp ANSI codes
pub const USER_PROMPT: Style = Style::new().fg(Color::DarkGray);
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
pub const CARD_BG: Color = Color::Rgb(45, 45, 45);
pub const BUTTON_BG: Color = Color::Rgb(70, 70, 70);
pub const CARD_FILL: Style = Style::new().bg(CARD_BG);
pub const CARD_BORDER: Style = Style::new().fg(Color::DarkGray).bg(CARD_BG);
pub const CARD_BORDER_SELECTED: Style = Style::new().fg(Color::White).bg(CARD_BG);
pub const CARD_CODE: Style = Style::new().fg(Color::White).bg(CARD_BG);
pub const CARD_DESCRIPTION: Style = Style::new()
    .fg(Color::Gray)
    .bg(CARD_BG)
    .add_modifier(Modifier::ITALIC);
pub const BUTTON: Style = Style::new().fg(Color::Gray).bg(BUTTON_BG);
pub const BUTTON_FOCUSED: Style = Style::new()
    .fg(Color::Black)
    .bg(Color::White)
    .add_modifier(Modifier::BOLD);
/// Non-focused button: plain white text on the card bg, no pill. Used for
/// secondary actions in the recommendation card so only the focused button
/// carries the white-pill highlight (matches Figma).
pub const BUTTON_PLAIN: Style = Style::new().fg(Color::White).bg(CARD_BG);
// Chat message dot indicators
pub const DOT_ERROR: Style = Style::new().fg(Color::Red).add_modifier(Modifier::BOLD);
pub const DOT_AGENT: Style = Style::new().fg(Color::DarkGray);
// Notification badge/banner styles
pub const BADGE_CRITICAL: Style = Style::new().fg(Color::Red).add_modifier(Modifier::BOLD);
pub const BADGE_ACTIONABLE: Style = Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD);
pub const BADGE_INFO: Style = Style::new().fg(Color::DarkGray);
pub const BANNER_HINT: Style = Style::new().fg(Color::DarkGray);
// Agent hook event styles
pub const AGENT_EVENT_HEADER: Style = Style::new().fg(Color::Magenta);
pub const AGENT_EVENT_DETAIL: Style = Style::new().fg(Color::DarkGray);
// Input box
pub const INPUT_BG: Color = Color::Black;
pub const INPUT_BORDER: Style = Style::new().fg(Color::Rgb(50, 50, 50));
