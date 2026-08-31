use std::{collections::VecDeque, ops::Range};

use crate::commands::{self, MovePositionSpec, YoloOptionSpec};

use super::tab_state::TabSession;

pub(super) const INPUT_HISTORY_MAX_ENTRIES: usize = 50;

pub(super) struct TextEditor<'a> {
    text: &'a mut String,
    cursor_pos: &'a mut usize,
}

impl<'a> TextEditor<'a> {
    pub fn new(text: &'a mut String, cursor_pos: &'a mut usize) -> Self {
        *cursor_pos = clamp_cursor_to_boundary(text, *cursor_pos);
        Self { text, cursor_pos }
    }

    pub fn insert_char(&mut self, character: char) -> Range<usize> {
        let start = *self.cursor_pos;
        self.text.insert(start, character);
        *self.cursor_pos += character.len_utf8();
        start..*self.cursor_pos
    }

    pub fn insert_str(&mut self, value: &str) -> Option<Range<usize>> {
        if value.is_empty() {
            return None;
        }
        let start = *self.cursor_pos;
        self.text.insert_str(start, value);
        *self.cursor_pos += value.len();
        Some(start..*self.cursor_pos)
    }

    pub fn delete_before_cursor(&mut self) -> Option<Range<usize>> {
        let end = *self.cursor_pos;
        let start = prev_char_boundary(self.text, end);
        self.delete_range(start..end)
    }

    pub fn delete_at_cursor(&mut self) -> Option<Range<usize>> {
        let start = *self.cursor_pos;
        let end = next_char_boundary(self.text, start);
        self.delete_range(start..end)
    }

    pub fn delete_word_before_cursor(&mut self) -> Option<Range<usize>> {
        let end = *self.cursor_pos;
        let start = prev_word_boundary(self.text, end);
        self.delete_range(start..end)
    }

    pub fn delete_range(&mut self, range: Range<usize>) -> Option<Range<usize>> {
        if range.is_empty() {
            return None;
        }
        self.text.replace_range(range.clone(), "");
        *self.cursor_pos = range.start;
        Some(range)
    }

    pub fn move_left(&mut self) {
        *self.cursor_pos = prev_char_boundary(self.text, *self.cursor_pos);
    }

    pub fn move_right(&mut self) {
        *self.cursor_pos = next_char_boundary(self.text, *self.cursor_pos);
    }

    pub fn move_word_left(&mut self) {
        *self.cursor_pos = prev_word_boundary(self.text, *self.cursor_pos);
    }

    pub fn move_word_right(&mut self) {
        *self.cursor_pos = next_word_boundary(self.text, *self.cursor_pos);
    }

    pub fn move_home(&mut self) {
        *self.cursor_pos = 0;
    }

    pub fn move_end(&mut self) {
        *self.cursor_pos = self.text.len();
    }
}

#[derive(Default)]
pub(super) struct InputHistory {
    pub(super) entries: VecDeque<String>,
    pub(super) selected: Option<usize>,
    pub(super) draft: Option<(String, usize, super::attachments::PendingAttachments)>,
}

impl TabSession {
    pub fn clear_input(&mut self) {
        self.reset_input_history_navigation();
        self.input.clear();
        self.cursor_pos = 0;
        self.attachments.clear();
        self.refresh_command_popup();
    }

    pub fn replace_input(&mut self, input: String) {
        self.reset_input_history_navigation();
        self.input = input;
        self.cursor_pos = self.input.len();
        self.attachments.clear();
        self.refresh_command_popup();
    }

    pub fn insert_input_char(&mut self, ch: char) {
        self.reset_input_history_navigation();
        let inserted = TextEditor::new(&mut self.input, &mut self.cursor_pos).insert_char(ch);
        self.attachments
            .on_text_inserted(inserted.start, inserted.len());
        self.refresh_command_popup();
    }

    pub fn insert_input_str(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.reset_input_history_navigation();
        if let Some(inserted) =
            TextEditor::new(&mut self.input, &mut self.cursor_pos).insert_str(text)
        {
            self.attachments
                .on_text_inserted(inserted.start, inserted.len());
        }
        self.refresh_command_popup();
    }

    pub fn insert_image_attachment(&mut self, image: crate::clipboard_image::PastedImage) {
        self.reset_input_history_navigation();
        self.cursor_pos = clamp_cursor_to_boundary(&self.input, self.cursor_pos);
        self.attachments
            .insert_image(&mut self.input, &mut self.cursor_pos, image);
        self.refresh_command_popup();
    }

    pub fn delete_before_cursor(&mut self) {
        self.cursor_pos = clamp_cursor_to_boundary(&self.input, self.cursor_pos);
        if self.cursor_pos == 0 {
            return;
        }

        self.reset_input_history_navigation();
        if self
            .attachments
            .remove_before_cursor(&mut self.input, &mut self.cursor_pos)
        {
            self.refresh_command_popup();
            return;
        }
        if let Some(deleted) =
            TextEditor::new(&mut self.input, &mut self.cursor_pos).delete_before_cursor()
        {
            self.attachments.on_text_deleted(deleted);
        }
        self.refresh_command_popup();
    }

    pub fn delete_word_before_cursor(&mut self) {
        self.cursor_pos = clamp_cursor_to_boundary(&self.input, self.cursor_pos);
        if self.cursor_pos == 0 {
            return;
        }
        self.reset_input_history_navigation();
        let range = self.attachments.expand_deletion_range(
            prev_word_boundary(&self.input, self.cursor_pos)..self.cursor_pos,
        );
        if let Some(deleted) =
            TextEditor::new(&mut self.input, &mut self.cursor_pos).delete_range(range)
        {
            self.attachments.on_text_deleted(deleted);
        }
        self.refresh_command_popup();
    }

    pub fn delete_at_cursor(&mut self) {
        self.cursor_pos = clamp_cursor_to_boundary(&self.input, self.cursor_pos);
        if self.cursor_pos >= self.input.len() {
            return;
        }

        self.reset_input_history_navigation();
        if self
            .attachments
            .remove_at_cursor(&mut self.input, self.cursor_pos)
        {
            self.refresh_command_popup();
            return;
        }
        if let Some(deleted) =
            TextEditor::new(&mut self.input, &mut self.cursor_pos).delete_at_cursor()
        {
            self.attachments.on_text_deleted(deleted);
        }
        self.refresh_command_popup();
    }

    pub fn move_cursor_left(&mut self) {
        if let Some(cursor_pos) = self.attachments.cursor_left(self.cursor_pos) {
            self.cursor_pos = cursor_pos;
        } else {
            TextEditor::new(&mut self.input, &mut self.cursor_pos).move_left();
        }
    }

    pub fn move_cursor_right(&mut self) {
        if let Some(cursor_pos) = self.attachments.cursor_right(self.cursor_pos) {
            self.cursor_pos = cursor_pos;
        } else {
            TextEditor::new(&mut self.input, &mut self.cursor_pos).move_right();
        }
    }

    pub fn move_cursor_word_left(&mut self) {
        TextEditor::new(&mut self.input, &mut self.cursor_pos).move_word_left();
        self.cursor_pos = self.attachments.snap_cursor_left(self.cursor_pos);
    }

    pub fn move_cursor_word_right(&mut self) {
        TextEditor::new(&mut self.input, &mut self.cursor_pos).move_word_right();
        self.cursor_pos = self.attachments.snap_cursor_right(self.cursor_pos);
    }

    pub fn move_cursor_home(&mut self) {
        TextEditor::new(&mut self.input, &mut self.cursor_pos).move_home();
    }

    pub fn move_cursor_end(&mut self) {
        TextEditor::new(&mut self.input, &mut self.cursor_pos).move_end();
    }

    pub(super) fn record_input_history(&mut self, input: &str) {
        self.reset_input_history_navigation();
        if input.is_empty() {
            return;
        }
        if let Some(index) = self
            .input_history
            .entries
            .iter()
            .position(|entry| entry == input)
        {
            self.input_history.entries.remove(index);
        }
        self.input_history.entries.push_front(input.to_string());
        self.input_history
            .entries
            .truncate(INPUT_HISTORY_MAX_ENTRIES);
    }

    pub(super) fn input_history_is_browsing(&self) -> bool {
        self.input_history.selected.is_some()
    }

    pub(super) fn has_input_history(&self) -> bool {
        !self.input_history.entries.is_empty()
    }

    pub(super) fn navigate_input_history_older(&mut self) {
        if self.input_history.entries.is_empty() {
            return;
        }
        let index = match self.input_history.selected {
            Some(index) => (index + 1).min(self.input_history.entries.len() - 1),
            None => {
                self.input_history.draft = Some((
                    self.input.clone(),
                    self.cursor_pos,
                    std::mem::take(&mut self.attachments),
                ));
                0
            }
        };
        self.input_history.selected = Some(index);
        self.input = self.input_history.entries[index].clone();
        self.cursor_pos = self.input.len();
        self.command_popup_candidates.clear();
        self.move_position_candidates.clear();
        self.yolo_option_candidates.clear();
        self.command_popup_selected = 0;
    }

    pub(super) fn navigate_input_history_newer(&mut self) {
        let Some(index) = self.input_history.selected else {
            return;
        };
        if index == 0 {
            let (draft, cursor_pos, attachments) =
                self.input_history.draft.take().unwrap_or_default();
            self.input = draft;
            self.attachments = attachments;
            self.cursor_pos = clamp_cursor_to_boundary(&self.input, cursor_pos);
            self.input_history.selected = None;
        } else {
            let next = index - 1;
            self.input_history.selected = Some(next);
            self.input = self.input_history.entries[next].clone();
            self.cursor_pos = self.input.len();
            self.command_popup_candidates.clear();
            self.move_position_candidates.clear();
            self.yolo_option_candidates.clear();
            self.command_popup_selected = 0;
        }
        if self.input_history.selected.is_none() {
            self.refresh_command_popup();
        }
    }

    pub(super) fn reset_input_history_navigation(&mut self) {
        self.input_history.selected = None;
        self.input_history.draft = None;
    }

    pub(super) fn clear_history_draft_attachments(&mut self) {
        if let Some((input, cursor_pos, attachments)) = self.input_history.draft.as_mut() {
            attachments.remove_tokens_from_input(input, cursor_pos);
        }
    }

    /// Recompute the slash-command popup candidates from the current
    /// input. Called after every input mutation. Clamps the selected
    /// index so it stays valid when the candidate list shrinks.
    pub fn refresh_command_popup(&mut self) {
        if let Some(prefix) = commands::move_position_prefix(&self.input) {
            self.command_popup_candidates.clear();
            self.move_position_candidates = commands::match_move_positions(prefix);
            self.yolo_option_candidates.clear();
        } else if let Some(prefix) = commands::yolo_option_prefix(&self.input) {
            self.command_popup_candidates.clear();
            self.move_position_candidates.clear();
            self.yolo_option_candidates = commands::match_yolo_options(prefix);
        } else if commands::is_command_prefix(&self.input) {
            // Strip leading whitespace + the `/` to get the user's
            // name query. `is_command_prefix` already guarantees the
            // shape, so the unwrap is safe.
            let trimmed = self.input.trim_start();
            let name = trimmed.strip_prefix('/').unwrap_or("");
            self.command_popup_candidates = commands::matches(name);
            self.move_position_candidates.clear();
            self.yolo_option_candidates.clear();
        } else {
            self.command_popup_candidates.clear();
            self.move_position_candidates.clear();
            self.yolo_option_candidates.clear();
        }
        let candidate_count = self.command_popup_candidates.len()
            + self.move_position_candidates.len()
            + self.yolo_option_candidates.len();
        if candidate_count == 0 {
            self.command_popup_selected = 0;
        } else if self.command_popup_selected >= candidate_count {
            self.command_popup_selected = candidate_count - 1;
        }
    }

    pub fn command_popup_visible(&self) -> bool {
        !self.command_popup_candidates.is_empty()
            || !self.move_position_candidates.is_empty()
            || !self.yolo_option_candidates.is_empty()
    }

    pub fn command_popup_up(&mut self) {
        if self.command_popup_selected > 0 {
            self.command_popup_selected -= 1;
        }
    }

    pub fn selected_move_position(&self) -> Option<&'static MovePositionSpec> {
        self.move_position_candidates
            .get(self.command_popup_selected)
            .copied()
    }

    pub fn selected_yolo_option(&self) -> Option<&'static YoloOptionSpec> {
        self.yolo_option_candidates
            .get(self.command_popup_selected)
            .copied()
    }
}

pub(super) fn clamp_cursor_to_boundary(input: &str, cursor_pos: usize) -> usize {
    let mut clamped = cursor_pos.min(input.len());
    while clamped > 0 && !input.is_char_boundary(clamped) {
        clamped -= 1;
    }
    clamped
}

pub(super) fn prev_char_boundary(input: &str, cursor_pos: usize) -> usize {
    let cursor_pos = clamp_cursor_to_boundary(input, cursor_pos);
    if cursor_pos == 0 {
        return 0;
    }

    input[..cursor_pos]
        .char_indices()
        .last()
        .map(|(idx, _)| idx)
        .unwrap_or(0)
}

pub(super) fn next_char_boundary(input: &str, cursor_pos: usize) -> usize {
    let cursor_pos = clamp_cursor_to_boundary(input, cursor_pos);
    if cursor_pos >= input.len() {
        return input.len();
    }

    input[cursor_pos..]
        .chars()
        .next()
        .map(|ch| cursor_pos + ch.len_utf8())
        .unwrap_or(input.len())
}

fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

pub(super) fn next_word_boundary(input: &str, cursor_pos: usize) -> usize {
    let cursor_pos = clamp_cursor_to_boundary(input, cursor_pos);
    if cursor_pos >= input.len() {
        return input.len();
    }

    let mut i = cursor_pos;
    while i < input.len() {
        let ch = input[i..].chars().next().unwrap();
        if is_word_char(ch) {
            break;
        }
        i += ch.len_utf8();
    }
    while i < input.len() {
        let ch = input[i..].chars().next().unwrap();
        if !is_word_char(ch) {
            break;
        }
        i += ch.len_utf8();
    }
    i
}

pub(super) fn prev_word_boundary(input: &str, cursor_pos: usize) -> usize {
    let cursor_pos = clamp_cursor_to_boundary(input, cursor_pos);
    if cursor_pos == 0 {
        return 0;
    }

    let mut i = cursor_pos;
    while i > 0 {
        let prev = prev_char_boundary(input, i);
        let ch = input[prev..].chars().next().unwrap();
        if is_word_char(ch) {
            break;
        }
        i = prev;
    }
    while i > 0 {
        let prev = prev_char_boundary(input, i);
        let ch = input[prev..].chars().next().unwrap();
        if !is_word_char(ch) {
            break;
        }
        i = prev;
    }
    i
}
