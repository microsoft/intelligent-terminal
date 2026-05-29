use ratatui::style::{Color, Modifier, Style};

// Colors matching AcpConnection.cpp ANSI codes
pub const USER_PROMPT: Style = Style::new().fg(Color::DarkGray);
pub const INPUT_TEXT: Style = Style::new().fg(Color::White);
pub const AGENT_TEXT: Style = Style::new().fg(Color::White);
pub const SYSTEM_TEXT: Style = Style::new().fg(Color::Cyan);
pub const TOOL_CALL: Style = Style::new().fg(Color::DarkGray);
pub const PLAN_STYLE: Style = Style::new().fg(Color::Cyan);
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
// Card-style recommendation UI.
// Border color = `#FFF @ 10%` over `#000`: 0×0.9 + 255×0.1 ≈ 26 → #1A1A1A.
pub const CARD_FRAME_COLOR: Color = Color::Rgb(26, 26, 26);
pub const BUTTON_BG: Color = Color::Rgb(70, 70, 70);
pub const CARD_BORDER: Style = Style::new().fg(CARD_FRAME_COLOR);
pub const CARD_BORDER_SELECTED: Style = Style::new().fg(CARD_FRAME_COLOR);
pub const CARD_CODE: Style = Style::new().fg(Color::White);
pub const CARD_DESCRIPTION: Style = Style::new()
    .fg(Color::Gray)
    .add_modifier(Modifier::ITALIC);
pub const BUTTON: Style = Style::new().fg(Color::Gray).bg(BUTTON_BG);
pub const BUTTON_FOCUSED: Style = Style::new()
    .fg(Color::Black)
    .bg(Color::White)
    .add_modifier(Modifier::BOLD);
pub const BUTTON_PLAIN: Style = Style::new().fg(Color::White);
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
