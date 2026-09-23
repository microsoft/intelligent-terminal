// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

use crate::app::TextEditor;
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Default)]
pub(in crate::agent_center) struct DraftEditor {
    pub cursor: usize,
    pub anchor: Option<usize>,
    pub width: usize,
    vertical_column: Option<usize>,
}

pub(in crate::agent_center) struct DraftLayout {
    pub lines: Vec<Line<'static>>,
    // Byte offsets and screen cells come from the same grapheme layout.
    positions: Vec<(usize, usize, usize)>,
}

impl DraftLayout {
    pub fn cursor(&self, offset: usize) -> (usize, usize) {
        self.positions
            .iter()
            .rev()
            .find(|(byte, _, _)| *byte <= offset)
            .map(|(_, row, col)| (*row, *col))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_replacement_deletion_and_boundary_movement() {
        let mut editor = DraftEditor::default();
        let mut text = String::new();
        editor.insert(&mut text, "abcdef");
        for _ in 0..2 {
            editor.key(&mut text, KeyCode::Left, KeyModifiers::SHIFT);
        }
        assert_eq!(editor.selection(), 4..6);
        editor.insert(&mut text, "XY");
        assert_eq!(text, "abcdXY");
        assert_eq!(editor.cursor, 6);
        assert_eq!(editor.anchor, None);
        editor.key(&mut text, KeyCode::Left, KeyModifiers::NONE);
        editor.key(&mut text, KeyCode::Delete, KeyModifiers::NONE);
        editor.key(&mut text, KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(text, "abcd");
        editor.key(&mut text, KeyCode::Home, KeyModifiers::NONE);
        editor.key(&mut text, KeyCode::Backspace, KeyModifiers::NONE);
        editor.key(&mut text, KeyCode::Left, KeyModifiers::NONE);
        assert_eq!(editor.cursor, 0);
        editor.key(&mut text, KeyCode::Right, KeyModifiers::SHIFT);
        editor.key(&mut text, KeyCode::Right, KeyModifiers::NONE);
        assert_eq!(editor.cursor, 1, "Right collapses to selection end");
        editor.key(&mut text, KeyCode::Char('a'), KeyModifiers::CONTROL);
        editor.key(&mut text, KeyCode::Left, KeyModifiers::NONE);
        assert_eq!(editor.cursor, 0, "Left collapses to selection start");
        editor.key(&mut text, KeyCode::Char('a'), KeyModifiers::CONTROL);
        editor.key(&mut text, KeyCode::Delete, KeyModifiers::NONE);
        assert_eq!(text, "");
        assert_eq!(editor.cursor, 0);
    }

    #[test]
    fn unicode_clusters_are_atomic_for_navigation_deletion_and_rendering() {
        let mut editor = DraftEditor {
            width: 8,
            ..Default::default()
        };
        let mut text = String::new();
        let clusters = [
            "\u{4e2d}",
            "e\u{301}",
            "\u{1f469}\u{200d}\u{1f4bb}",
            "\u{1f1e8}\u{1f1f3}",
        ];
        for cluster in clusters {
            editor.insert(&mut text, cluster);
        }
        assert_eq!(editor.layout(&text).cursor(editor.cursor), (0, 7));
        for cluster in clusters.into_iter().rev() {
            let end = editor.cursor;
            editor.key(&mut text, KeyCode::Left, KeyModifiers::NONE);
            assert_eq!(&text[editor.cursor..end], cluster);
            editor.key(&mut text, KeyCode::Right, KeyModifiers::SHIFT);
            editor.key(&mut text, KeyCode::Backspace, KeyModifiers::NONE);
        }
        assert_eq!(text, "");
        editor.insert(&mut text, "eX");
        editor.key(&mut text, KeyCode::Left, KeyModifiers::NONE);
        editor.insert(&mut text, "\u{301}");
        assert_eq!(editor.cursor, "e\u{301}".len());
        editor.key(&mut text, KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(text, "X");
    }

    #[test]
    fn line_word_and_wrapped_vertical_navigation_share_the_rendered_layout() {
        let mut editor = DraftEditor {
            width: 4,
            ..Default::default()
        };
        let mut text = String::new();
        editor.insert(&mut text, "abcdef\nxy\n123456");
        editor.key(&mut text, KeyCode::Home, KeyModifiers::NONE);
        assert_eq!(&text[..editor.cursor], "abcdef\nxy\n");
        editor.key(&mut text, KeyCode::End, KeyModifiers::NONE);
        assert_eq!(editor.cursor, text.len());
        editor.key(&mut text, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(&text[editor.cursor..], "3456");
        editor.key(&mut text, KeyCode::Up, KeyModifiers::SHIFT);
        assert_eq!(&text[editor.cursor..], "\n123456");
        assert_eq!(&text[editor.selection()], "\n12");
        editor.key(&mut text, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(&text[editor.cursor..], "3456");
        editor.reset(0);
        editor.insert(&mut text, "hello world ");
        editor.key(&mut text, KeyCode::Left, KeyModifiers::CONTROL);
        assert_eq!(editor.cursor, 6);
        editor.key(&mut text, KeyCode::Home, KeyModifiers::CONTROL);
        assert_eq!(editor.cursor, 0);
        editor.key(&mut text, KeyCode::Right, KeyModifiers::CONTROL);
        assert_eq!(editor.cursor, 5, "reuse the helper's word-end navigation");
    }

    #[test]
    fn layout_preserves_spaces_tabs_newlines_and_keeps_caret_inside_narrow_rows() {
        let mut editor = DraftEditor::default();
        let mut text = String::new();
        editor.insert(&mut text, "ab  \t\u{4e2d}e\u{301}\nlast\n");
        for width in [1, 2, 4, 8, 40] {
            editor.width = width;
            let layout = editor.layout(&text);
            for (offset, row, col) in &layout.positions {
                assert!(text.is_char_boundary(*offset));
                assert!(*col < width);
                assert!(*row < layout.lines.len());
            }
            assert_eq!(layout.cursor(text.len()).1, 0);
            assert_eq!(layout.lines.last().unwrap().to_string(), " ");
            assert!(layout.lines.iter().all(|line| line.width() <= width));
        }
    }
}

impl DraftEditor {
    pub fn reset(&mut self, cursor: usize) {
        self.cursor = cursor;
        self.anchor = None;
        self.vertical_column = None;
    }

    pub fn selection(&self) -> Range<usize> {
        let anchor = self.anchor.unwrap_or(self.cursor);
        anchor.min(self.cursor)..anchor.max(self.cursor)
    }

    fn boundaries(text: &str) -> impl DoubleEndedIterator<Item = usize> + '_ {
        text.grapheme_indices(true)
            .map(|(offset, _)| offset)
            .chain(std::iter::once(text.len()))
    }

    pub fn insert(&mut self, text: &mut String, value: &str) {
        if value.is_empty() {
            return;
        }
        let selection = self.selection();
        let mut editor = TextEditor::new(text, &mut self.cursor);
        editor.delete_range(selection);
        editor.insert_str(value);
        // Insertion can join adjacent graphemes (combining marks or ZWJ emoji).
        let cursor = Self::boundaries(text)
            .find(|offset| *offset >= self.cursor)
            .unwrap_or(text.len());
        self.reset(cursor);
    }

    pub fn key(&mut self, text: &mut String, key: KeyCode, modifiers: KeyModifiers) -> bool {
        let shift = modifiers.contains(KeyModifiers::SHIFT);
        let control = modifiers.contains(KeyModifiers::CONTROL);
        let selection = self.selection();
        let previous = self.cursor;
        match key {
            KeyCode::Char('a') if control => {
                self.anchor = Some(0);
                self.cursor = text.len();
            }
            KeyCode::Backspace | KeyCode::Delete => {
                let range = if !selection.is_empty() {
                    selection
                } else if key == KeyCode::Backspace {
                    let start = Self::boundaries(text)
                        .rev()
                        .find(|offset| *offset < self.cursor)
                        .unwrap_or(0);
                    start..self.cursor
                } else {
                    let end = Self::boundaries(text)
                        .find(|offset| *offset > self.cursor)
                        .unwrap_or(text.len());
                    self.cursor..end
                };
                TextEditor::new(text, &mut self.cursor).delete_range(range);
                let cursor = Self::boundaries(text)
                    .rev()
                    .find(|offset| *offset <= self.cursor)
                    .unwrap_or(0);
                self.reset(cursor);
            }
            KeyCode::Left | KeyCode::Right => {
                let left = key == KeyCode::Left;
                self.cursor = if !shift && !selection.is_empty() {
                    if left {
                        selection.start
                    } else {
                        selection.end
                    }
                } else if control {
                    let mut editor = TextEditor::new(text, &mut self.cursor);
                    if left {
                        editor.move_word_left();
                        Self::boundaries(text)
                            .rev()
                            .find(|offset| *offset <= self.cursor)
                            .unwrap_or(0)
                    } else {
                        editor.move_word_right();
                        Self::boundaries(text)
                            .find(|offset| *offset >= self.cursor)
                            .unwrap_or(text.len())
                    }
                } else if left {
                    Self::boundaries(text)
                        .rev()
                        .find(|offset| *offset < self.cursor)
                        .unwrap_or(0)
                } else {
                    Self::boundaries(text)
                        .find(|offset| *offset > self.cursor)
                        .unwrap_or(text.len())
                };
            }
            KeyCode::Home => {
                self.cursor = if control {
                    0
                } else {
                    text[..self.cursor].rfind('\n').map_or(0, |index| index + 1)
                };
            }
            KeyCode::End => {
                self.cursor += text[self.cursor..]
                    .find('\n')
                    .unwrap_or(text.len() - self.cursor);
            }
            KeyCode::Up | KeyCode::Down => {
                let layout = self.layout(text);
                let (row, col) = layout.cursor(self.cursor);
                let column = *self.vertical_column.get_or_insert(col);
                let target = if key == KeyCode::Up {
                    row.saturating_sub(1)
                } else {
                    (row + 1).min(layout.lines.len() - 1)
                };
                self.cursor = layout
                    .positions
                    .iter()
                    .filter(|(_, row, _)| *row == target)
                    .min_by_key(|(_, _, col)| col.abs_diff(column))
                    .map(|(offset, _, _)| *offset)
                    .unwrap_or(self.cursor);
            }
            _ => return false,
        }
        if matches!(
            key,
            KeyCode::Left
                | KeyCode::Right
                | KeyCode::Home
                | KeyCode::End
                | KeyCode::Up
                | KeyCode::Down
        ) {
            self.anchor = if shift {
                Some(self.anchor.unwrap_or(previous))
            } else {
                None
            };
        }
        if !matches!(key, KeyCode::Up | KeyCode::Down) {
            self.vertical_column = None;
        }
        true
    }

    pub fn layout(&self, text: &str) -> DraftLayout {
        let width = self.width.max(1);
        let selection = self.selection();
        let mut lines = vec![Line::default()];
        let mut positions = Vec::new();
        let mut row = 0;
        let mut col = 0;
        for (offset, grapheme) in text
            .grapheme_indices(true)
            .chain(std::iter::once((text.len(), "")))
        {
            let newline = grapheme == "\n";
            let glyph = match grapheme {
                "" | "\n" => " ".to_owned(),
                "\t" => " ".repeat(4.min(width)),
                value if value.width() == 0 => format!(" {value}"),
                value if value.width() > width => " ".to_owned(),
                value => value.to_owned(),
            };
            let cells = glyph.width();
            if col + cells > width {
                row += 1;
                col = 0;
                lines.push(Line::default());
            }
            positions.push((offset, row, col));
            let style = if selection.contains(&offset) {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            lines[row].spans.push(Span::styled(glyph, style));
            col += cells;
            if newline {
                row += 1;
                col = 0;
                lines.push(Line::default());
            }
        }
        DraftLayout { lines, positions }
    }
}
