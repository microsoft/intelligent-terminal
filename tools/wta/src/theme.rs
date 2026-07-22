use ratatui::style::{Color, Modifier, Style};

// Colors matching AcpConnection.cpp ANSI codes
pub const USER_PROMPT: Style = Style::new().fg(Color::DarkGray);
// Tracks the scheme foreground now that INPUT_BG is the scheme background —
// a hardcoded white would be invisible in the box on a light scheme (#234).
pub const INPUT_TEXT: Style = Style::new().fg(Color::Reset);
// Default foreground (Color::Reset) so the agent's reply text tracks the
// pane's color scheme — light text on dark schemes, dark text on light
// schemes. A hardcoded white was invisible on light color schemes (#234).
pub const AGENT_TEXT: Style = Style::new().fg(Color::Reset);
pub const SYSTEM_TEXT: Style = Style::new().fg(Color::Cyan);
pub const TOOL_CALL: Style = Style::new().fg(Color::DarkGray);
pub const PLAN_STYLE: Style = Style::new().fg(Color::Cyan);
pub const ERROR_STYLE: Style = Style::new().fg(Color::Red);
pub const IN_PROGRESS: Style = Style::new()
    .fg(Color::Yellow)
    .add_modifier(Modifier::BOLD)
    .add_modifier(Modifier::ITALIC);
pub const DIM: Style = Style::new().fg(Color::DarkGray);
// Match the /sessions cursor: cyan foreground with no full-row background.
pub const SELECTED: Style = Style::new().fg(Color::Cyan);
// Preserve the selection when the pane loses focus without presenting it as
// the active keyboard target.
pub const SELECTED_INACTIVE: Style = Style::new()
    .fg(Color::Cyan)
    .add_modifier(Modifier::DIM);
pub const DEBUG_SENT: Style = Style::new().fg(Color::Green);
pub const DEBUG_RECEIVED: Style = Style::new().fg(Color::Cyan);
pub const RECOMMENDATION_TITLE: Style = Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD);
// Dimmed default fg rather than Color::Gray (ANSI 7, near-white) so secondary
// text stays readable on light schemes while still reading as "muted" (#234).
pub const RECOMMENDATION_DETAIL: Style = Style::new()
    .fg(Color::Reset)
    .add_modifier(Modifier::DIM);
// Card-style recommendation UI.
// Border color = `#FFF @ 10%` over `#000`: 0×0.9 + 255×0.1 ≈ 26 → #1A1A1A.
pub const CARD_FRAME_COLOR: Color = Color::Rgb(26, 26, 26);
pub const BUTTON_BG: Color = Color::Rgb(70, 70, 70);
pub const CARD_BORDER: Style = Style::new().fg(CARD_FRAME_COLOR);
pub const CARD_BORDER_SELECTED: Style = Style::new().fg(CARD_FRAME_COLOR);
pub const CARD_CODE: Style = Style::new().fg(Color::Reset);
pub const CARD_DESCRIPTION: Style = Style::new()
    .fg(Color::Reset)
    .add_modifier(Modifier::DIM)
    .add_modifier(Modifier::ITALIC);
pub const BUTTON: Style = Style::new().fg(Color::Gray).bg(BUTTON_BG);
// Reverse-video (swap scheme fg/bg) instead of a hardcoded black-on-white,
// which vanished on light schemes (white button on a light pane). REVERSED
// gives a solid, high-contrast block in the scheme's own colors either way.
pub const BUTTON_FOCUSED: Style = Style::new()
    .add_modifier(Modifier::REVERSED)
    .add_modifier(Modifier::BOLD);
pub const BUTTON_PLAIN: Style = Style::new().fg(Color::Reset);
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
// Input box / command & model popups. Color::Reset == the pane's color-scheme
// background, so the box tracks the theme (light box on light schemes) instead
// of a hardcoded black panel, while still painting an opaque surface over the
// content behind a popup (#234).
pub const INPUT_BG: Color = Color::Reset;
pub const INPUT_BORDER: Style = Style::new().fg(Color::Rgb(50, 50, 50));
