use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::theme;

pub(crate) const INPUT_MIN_HEIGHT: u16 = 3;
pub(crate) const INPUT_MAX_HEIGHT: u16 = 8;
const LEFT_PADDING: u16 = 1;
const PROMPT_WIDTH: u16 = 2;

pub(crate) struct ComposerLayout {
    pub text: Rect,
}

fn has_border(area: Rect, bordered: bool) -> bool {
    bordered && area.width >= 3 && area.height >= 3
}

fn inner_area(area: Rect, bordered: bool) -> Rect {
    if has_border(area, bordered) {
        area.inner(Margin::new(1, 1))
    } else {
        area
    }
}

fn prefix_width(inner: Rect) -> u16 {
    if inner.width > PROMPT_WIDTH {
        PROMPT_WIDTH
    } else {
        0
    }
}

/// Text geometry shared by the renderer and each frontend's editor viewport.
/// At small sizes, drop decoration before consuming the last text cell.
pub(crate) fn layout(area: Rect, bordered: bool) -> ComposerLayout {
    let inner = inner_area(area, bordered);
    let prefix = prefix_width(inner);
    let padding = if inner.width > LEFT_PADDING + PROMPT_WIDTH {
        LEFT_PADDING
    } else {
        0
    };
    let inset = padding + prefix;
    ComposerLayout {
        text: Rect::new(
            inner.x.saturating_add(inset),
            inner.y,
            inner.width.saturating_sub(inset),
            inner.height,
        ),
    }
}

/// Render prepared text rows without owning editor, session, or backend state.
/// Rows contain their own caret/selection styles, but never a prompt prefix.
pub(crate) fn render(
    frame: &mut Frame,
    area: Rect,
    bordered: bool,
    focused: bool,
    lines: Vec<Line<'_>>,
    first_row: usize,
) {
    let area = area.intersection(frame.area());
    if area.is_empty() {
        return;
    }
    let block = Block::default()
        .borders(if has_border(area, bordered) {
            Borders::ALL
        } else {
            Borders::NONE
        })
        .border_style(if focused {
            theme::INPUT_BORDER_FOCUSED
        } else {
            theme::INPUT_BORDER
        })
        .style(Style::new().bg(theme::INPUT_BG));
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);

    let text = layout(area, bordered).text;
    let prefix = prefix_width(inner_area(area, bordered));
    if prefix > 0 {
        let prefixes = (0..lines.len().min(usize::from(text.height)))
            .map(|row| {
                if first_row == 0 && row == 0 {
                    Line::from(Span::styled("> ", theme::DIM))
                } else {
                    Line::from("  ")
                }
            })
            .collect::<Vec<_>>();
        frame.render_widget(
            Paragraph::new(prefixes),
            Rect::new(text.x - prefix, text.y, prefix, text.height),
        );
    }
    frame.render_widget(Paragraph::new(lines), text);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    fn draw(
        area: Rect,
        bordered: bool,
        focused: bool,
        lines: Vec<Line<'static>>,
        first_row: usize,
    ) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(20, 8)).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(
                    Paragraph::new(vec![Line::from(".".repeat(20)); 8]),
                    frame.area(),
                );
                render(frame, area, bordered, focused, lines, first_row);
            })
            .unwrap();
        terminal.backend().buffer().clone()
    }

    #[test]
    fn layout_matches_pane_chrome_and_borderless_composer() {
        let area = Rect::new(2, 1, 12, 5);
        assert_eq!(layout(area, true).text, Rect::new(6, 2, 7, 3));
        assert_eq!(layout(area, false).text, Rect::new(5, 1, 9, 5));
    }

    #[test]
    fn focus_changes_only_the_border() {
        let area = Rect::new(1, 1, 12, 4);
        for focused in [false, true] {
            let buffer = draw(area, true, focused, vec![Line::from("text")], 0);
            let expected = if focused {
                theme::INPUT_BORDER_FOCUSED
            } else {
                theme::INPUT_BORDER
            };
            assert_eq!(buffer[(1, 1)].fg, expected.fg.unwrap());
            assert_eq!(buffer[(5, 2)].symbol(), "t");
            assert_eq!(buffer[(5, 2)].fg, Color::Reset);
            assert_eq!(buffer[(0, 0)].symbol(), ".");
        }
    }

    #[test]
    fn prompt_marks_only_the_absolute_first_row() {
        for bordered in [false, true] {
            let area = Rect::new(1, 1, 12, 5);
            let text = layout(area, bordered).text;
            for first_row in [0, 3, usize::MAX] {
                let buffer = draw(
                    area,
                    bordered,
                    true,
                    vec![Line::from("first"), Line::from("next")],
                    first_row,
                );
                assert_eq!(
                    buffer[(text.x - 2, text.y)].symbol(),
                    if first_row == 0 { ">" } else { " " }
                );
                assert_eq!(buffer[(text.x - 2, text.y + 1)].symbol(), " ");
                assert_eq!(buffer[(text.x, text.y)].symbol(), "f");
                assert_eq!(buffer[(text.x, text.y + 1)].symbol(), "n");
            }
        }
    }

    #[test]
    fn scheme_relative_background_and_painted_caret_are_preserved() {
        let area = Rect::new(1, 1, 12, 4);
        let text = layout(area, true).text;
        let buffer = draw(
            area,
            true,
            true,
            vec![Line::from(Span::styled(
                " ",
                Style::new().add_modifier(Modifier::REVERSED),
            ))],
            0,
        );
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                assert_eq!(buffer[(x, y)].bg, theme::INPUT_BG);
            }
        }
        let caret = &buffer[(text.x, text.y)];
        assert_eq!(caret.fg, Color::Reset);
        assert!(caret.modifier.contains(Modifier::REVERSED));
        assert_eq!(buffer[(text.x - 2, text.y)].fg, theme::DIM.fg.unwrap());
    }

    #[test]
    fn prepared_selection_and_token_styles_survive_clipping() {
        let area = Rect::new(1, 1, 9, 3);
        let text = layout(area, true).text;
        let buffer = draw(
            area,
            true,
            false,
            vec![
                Line::from(vec![
                    Span::styled("ab", theme::INPUT_TEXT.add_modifier(Modifier::REVERSED)),
                    Span::styled("cdEXCESS", theme::ATTACHMENT_TOKEN),
                ]),
                Line::from("hidden"),
            ],
            0,
        );
        assert!(buffer[(text.x, text.y)]
            .modifier
            .contains(Modifier::REVERSED));
        assert!(!buffer[(text.x - 2, text.y)]
            .modifier
            .contains(Modifier::REVERSED));
        assert_eq!(
            buffer[(text.x + 2, text.y)].fg,
            theme::ATTACHMENT_TOKEN.fg.unwrap()
        );
        assert_eq!(buffer[(text.right() - 1, text.y)].symbol(), "d");
        assert_eq!(buffer[(area.right() - 1, text.y)].symbol(), "│");
        assert_eq!(buffer[(area.right(), text.y)].symbol(), ".");
    }

    #[test]
    fn tiny_rectangles_keep_the_caret_inside_and_do_not_touch_neighbors() {
        for bordered in [false, true] {
            for width in 0..=7 {
                for height in 0..=4 {
                    let area = Rect::new(2, 2, width, height);
                    let text = layout(area, bordered).text;
                    let buffer = draw(
                        area,
                        bordered,
                        true,
                        vec![Line::from(Span::styled(
                            "Xoverflow",
                            Style::new().add_modifier(Modifier::REVERSED),
                        ))],
                        0,
                    );
                    if width > 0 && height > 0 {
                        assert!(text.width > 0 && text.height > 0);
                        assert_eq!(buffer[(text.x, text.y)].symbol(), "X");
                        assert!(buffer[(text.x, text.y)]
                            .modifier
                            .contains(Modifier::REVERSED));
                    }
                    for y in 0..8 {
                        for x in 0..20 {
                            if !area.contains(Position::new(x, y)) {
                                assert_eq!(buffer[(x, y)].symbol(), ".");
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn rectangles_beyond_the_frame_are_clipped() {
        let buffer = draw(
            Rect::new(18, 7, 10, 5),
            true,
            true,
            vec![Line::from("XYoverflow")],
            0,
        );
        assert_eq!(buffer[(18, 7)].symbol(), "X");
        assert_eq!(buffer[(19, 7)].symbol(), "Y");
        assert_eq!(buffer[(17, 7)].symbol(), ".");
    }
}
