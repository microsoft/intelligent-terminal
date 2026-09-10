use std::{collections::VecDeque, ops::Range};

use crate::commands::{self, MovePositionSpec};

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

#[derive(Clone)]
struct InputEditState {
    cursor_pos: usize,
    selected: bool,
    attachments: super::attachments::PendingAttachments,
}

struct InputEdit {
    start: usize,
    removed: String,
    inserted: String,
    before: InputEditState,
    after: InputEditState,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InputEditKind {
    Typing,
    Other,
}

#[derive(Default)]
pub(super) struct InputEditHistory {
    undo: Vec<InputEdit>,
    redo: Vec<InputEdit>,
    typing_open: bool,
}

impl InputEditHistory {
    fn record(&mut self, edit: InputEdit, kind: InputEditKind) {
        self.redo.clear();
        if kind == InputEditKind::Typing && self.typing_open && edit.removed.is_empty() {
            if let Some(previous) = self.undo.last_mut() {
                if previous.start + previous.inserted.len() == edit.start
                    && previous.after.cursor_pos == edit.before.cursor_pos
                    && !edit.before.selected
                {
                    previous.inserted.push_str(&edit.inserted);
                    previous.after = edit.after;
                    return;
                }
            }
        }
        self.undo.push(edit);
        self.typing_open = kind == InputEditKind::Typing;
    }
}

#[derive(Default)]
pub(super) struct InputHistoryDraft {
    input: String,
    cursor_pos: usize,
    attachments: super::attachments::PendingAttachments,
    edits: InputEditHistory,
}

#[derive(Default)]
pub(super) struct InputHistory {
    pub(super) entries: VecDeque<String>,
    pub(super) selected: Option<usize>,
    pub(super) draft: Option<InputHistoryDraft>,
}

impl TabSession {
    pub(super) fn break_input_undo_group(&mut self) {
        self.input_edits.typing_open = false;
    }

    pub(super) fn reset_input_undo_history(&mut self) {
        self.input_edits = InputEditHistory::default();
    }

    fn input_edit_state(&self) -> InputEditState {
        InputEditState {
            cursor_pos: clamp_cursor_to_boundary(&self.input, self.cursor_pos),
            selected: self.input_all_selected,
            attachments: self.attachments.clone(),
        }
    }

    fn input_replacement_range(&mut self) -> Range<usize> {
        self.cursor_pos = clamp_cursor_to_boundary(&self.input, self.cursor_pos);
        if self.input_all_selected {
            0..self.input.len()
        } else {
            self.cursor_pos..self.cursor_pos
        }
    }

    fn replace_input_range(&mut self, range: Range<usize>, text: &str, kind: InputEditKind) {
        if range.is_empty() && text.is_empty() {
            return;
        }
        let before = self.input_edit_state();
        let removed = self.input[range.clone()].to_owned();
        self.reset_input_history_navigation();
        self.cursor_pos = range.start;
        if let Some(deleted) =
            TextEditor::new(&mut self.input, &mut self.cursor_pos).delete_range(range.clone())
        {
            self.attachments.on_text_deleted(deleted);
        }
        if let Some(inserted) =
            TextEditor::new(&mut self.input, &mut self.cursor_pos).insert_str(text)
        {
            self.attachments
                .on_text_inserted(inserted.start, inserted.len());
        }
        self.refresh_command_popup();
        let after = self.input_edit_state();
        self.input_edits.record(
            InputEdit {
                start: range.start,
                removed,
                inserted: text.to_owned(),
                before,
                after,
            },
            kind,
        );
    }

    fn replay_input_edit(&mut self, edit: &InputEdit, redo: bool) -> bool {
        let (expected, replacement, state) = if redo {
            (&edit.removed, &edit.inserted, &edit.after)
        } else {
            (&edit.inserted, &edit.removed, &edit.before)
        };
        let range = edit.start..edit.start + expected.len();
        if self.input.get(range.clone()) != Some(expected.as_str()) {
            tracing::error!(target: "input_undo", "discarding stale input edit history");
            return false;
        }
        self.input.replace_range(range, replacement);
        self.reset_input_history_navigation();
        self.cursor_pos = clamp_cursor_to_boundary(&self.input, state.cursor_pos);
        self.input_all_selected = state.selected && !self.input.is_empty();
        self.attachments = state.attachments.clone();
        self.refresh_command_popup();
        true
    }

    pub(super) fn undo_input(&mut self) {
        self.break_input_undo_group();
        let Some(edit) = self.input_edits.undo.pop() else {
            return;
        };
        if self.replay_input_edit(&edit, false) {
            self.input_edits.redo.push(edit);
        } else {
            self.reset_input_undo_history();
        }
    }

    pub(super) fn redo_input(&mut self) {
        self.break_input_undo_group();
        let Some(edit) = self.input_edits.redo.pop() else {
            return;
        };
        if self.replay_input_edit(&edit, true) {
            self.input_edits.undo.push(edit);
        } else {
            self.reset_input_undo_history();
        }
    }

    pub fn select_all_input(&mut self) {
        self.break_input_undo_group();
        self.input_vertical_goal = None;
        self.input_all_selected = !self.input.is_empty();
        self.cursor_pos = self.input.len();
    }

    pub fn delete_input_selection(&mut self) -> bool {
        if !self.input_all_selected {
            return false;
        }
        self.clear_input();
        true
    }

    pub fn clear_input(&mut self) {
        self.break_input_undo_group();
        if self.input.is_empty() {
            self.reset_input_history_navigation();
            self.cursor_pos = 0;
            self.attachments.clear();
            self.refresh_command_popup();
        } else {
            self.replace_input_range(0..self.input.len(), "", InputEditKind::Other);
        }
    }

    pub(super) fn discard_input(&mut self) {
        self.clear_input();
        self.reset_input_undo_history();
    }

    pub fn replace_input(&mut self, input: String) {
        if input.is_empty() {
            self.clear_input();
        } else {
            self.replace_input_range(0..self.input.len(), &input, InputEditKind::Other);
        }
    }

    pub fn insert_input_char(&mut self, ch: char) {
        let range = self.input_replacement_range();
        let mut encoded = [0; 4];
        let kind = if ch.is_control() {
            InputEditKind::Other
        } else {
            InputEditKind::Typing
        };
        self.replace_input_range(range, ch.encode_utf8(&mut encoded), kind);
    }

    pub fn insert_input_str(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let range = self.input_replacement_range();
        self.replace_input_range(range, text, InputEditKind::Other);
    }

    pub fn insert_image_attachment(&mut self, image: crate::clipboard_image::PastedImage) {
        let range = self.input_replacement_range();
        let before = self.input_edit_state();
        let removed = self.input[range.clone()].to_owned();
        self.reset_input_history_navigation();
        self.cursor_pos = range.start;
        if let Some(deleted) =
            TextEditor::new(&mut self.input, &mut self.cursor_pos).delete_range(range.clone())
        {
            self.attachments.on_text_deleted(deleted);
        }
        self.attachments
            .insert_image(&mut self.input, &mut self.cursor_pos, image);
        self.refresh_command_popup();
        let after = self.input_edit_state();
        self.input_edits.record(
            InputEdit {
                start: range.start,
                removed,
                inserted: self.input[range.start..self.cursor_pos].to_owned(),
                before,
                after,
            },
            InputEditKind::Other,
        );
    }

    pub fn delete_before_cursor(&mut self) {
        if self.delete_input_selection() {
            return;
        }
        self.cursor_pos = clamp_cursor_to_boundary(&self.input, self.cursor_pos);
        if self.cursor_pos == 0 {
            return;
        }

        let range = self.attachments.expand_deletion_range(
            prev_char_boundary(&self.input, self.cursor_pos)..self.cursor_pos,
        );
        self.replace_input_range(range, "", InputEditKind::Other);
    }

    pub fn delete_word_before_cursor(&mut self) {
        if self.delete_input_selection() {
            return;
        }
        self.cursor_pos = clamp_cursor_to_boundary(&self.input, self.cursor_pos);
        if self.cursor_pos == 0 {
            return;
        }
        let range = self.attachments.expand_deletion_range(
            prev_word_boundary(&self.input, self.cursor_pos)..self.cursor_pos,
        );
        self.replace_input_range(range, "", InputEditKind::Other);
    }

    pub fn delete_at_cursor(&mut self) {
        if self.delete_input_selection() {
            return;
        }
        self.cursor_pos = clamp_cursor_to_boundary(&self.input, self.cursor_pos);
        if self.cursor_pos >= self.input.len() {
            return;
        }

        let range = self.attachments.expand_deletion_range(
            self.cursor_pos..next_char_boundary(&self.input, self.cursor_pos),
        );
        self.replace_input_range(range, "", InputEditKind::Other);
    }

    pub fn move_cursor_left(&mut self) {
        self.break_input_undo_group();
        self.input_vertical_goal = None;
        if self.input_all_selected {
            self.move_cursor_home();
            return;
        }
        if let Some(cursor_pos) = self.attachments.cursor_left(self.cursor_pos) {
            self.cursor_pos = cursor_pos;
        } else {
            TextEditor::new(&mut self.input, &mut self.cursor_pos).move_left();
        }
    }

    pub fn move_cursor_right(&mut self) {
        self.break_input_undo_group();
        self.input_vertical_goal = None;
        if self.input_all_selected {
            self.move_cursor_end();
            return;
        }
        if let Some(cursor_pos) = self.attachments.cursor_right(self.cursor_pos) {
            self.cursor_pos = cursor_pos;
        } else {
            TextEditor::new(&mut self.input, &mut self.cursor_pos).move_right();
        }
    }

    pub fn move_cursor_word_left(&mut self) {
        self.break_input_undo_group();
        self.input_vertical_goal = None;
        if self.input_all_selected {
            self.move_cursor_home();
            return;
        }
        TextEditor::new(&mut self.input, &mut self.cursor_pos).move_word_left();
        self.cursor_pos = self.attachments.snap_cursor_left(self.cursor_pos);
    }

    pub fn move_cursor_word_right(&mut self) {
        self.break_input_undo_group();
        self.input_vertical_goal = None;
        if self.input_all_selected {
            self.move_cursor_end();
            return;
        }
        TextEditor::new(&mut self.input, &mut self.cursor_pos).move_word_right();
        self.cursor_pos = self.attachments.snap_cursor_right(self.cursor_pos);
    }

    pub fn move_cursor_home(&mut self) {
        self.break_input_undo_group();
        self.input_vertical_goal = None;
        self.input_all_selected = false;
        TextEditor::new(&mut self.input, &mut self.cursor_pos).move_home();
    }

    pub fn move_cursor_end(&mut self) {
        self.break_input_undo_group();
        self.input_vertical_goal = None;
        self.input_all_selected = false;
        TextEditor::new(&mut self.input, &mut self.cursor_pos).move_end();
    }

    pub fn move_cursor_vertical(&mut self, input_width: u16, upward: bool) -> bool {
        self.break_input_undo_group();
        if self.input_all_selected {
            if upward {
                self.move_cursor_home();
            } else {
                self.move_cursor_end();
            }
            return true;
        }
        let preferred = self
            .input_vertical_goal
            .filter(|(width, _)| *width == input_width)
            .map(|(_, column)| column);
        let Some((position, column)) = crate::ui::adjacent_input_cursor(
            &self.input,
            self.cursor_pos,
            input_width,
            upward,
            preferred,
        ) else {
            return false;
        };
        self.cursor_pos = if upward {
            self.attachments.snap_cursor_left(position)
        } else {
            self.attachments.snap_cursor_right(position)
        };
        self.input_vertical_goal = Some((input_width, column));
        true
    }

    pub(super) fn record_input_history(&mut self, input: &str) {
        self.reset_input_undo_history();
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
        self.break_input_undo_group();
        self.input_vertical_goal = None;
        self.input_all_selected = false;
        if self.input_history.entries.is_empty() {
            return;
        }
        let index = match self.input_history.selected {
            Some(index) => (index + 1).min(self.input_history.entries.len() - 1),
            None => {
                self.input_history.draft = Some(InputHistoryDraft {
                    input: self.input.clone(),
                    cursor_pos: self.cursor_pos,
                    attachments: std::mem::take(&mut self.attachments),
                    edits: std::mem::take(&mut self.input_edits),
                });
                0
            }
        };
        self.input_history.selected = Some(index);
        self.reset_input_undo_history();
        self.input = self.input_history.entries[index].clone();
        self.cursor_pos = self.input.len();
        self.command_popup_candidates.clear();
        self.move_position_candidates.clear();
        self.command_popup_selected = 0;
    }

    pub(super) fn navigate_input_history_newer(&mut self) {
        self.break_input_undo_group();
        self.input_vertical_goal = None;
        self.input_all_selected = false;
        let Some(index) = self.input_history.selected else {
            return;
        };
        if index == 0 {
            let draft = self.input_history.draft.take().unwrap_or_default();
            self.input = draft.input;
            self.attachments = draft.attachments;
            self.input_edits = draft.edits;
            self.cursor_pos = clamp_cursor_to_boundary(&self.input, draft.cursor_pos);
            self.input_history.selected = None;
        } else {
            self.reset_input_undo_history();
            let next = index - 1;
            self.input_history.selected = Some(next);
            self.input = self.input_history.entries[next].clone();
            self.cursor_pos = self.input.len();
            self.command_popup_candidates.clear();
            self.move_position_candidates.clear();
            self.command_popup_selected = 0;
        }
        if self.input_history.selected.is_none() {
            self.refresh_command_popup();
        }
    }

    pub(super) fn reset_input_history_navigation(&mut self) {
        self.input_vertical_goal = None;
        self.input_all_selected = false;
        self.input_history.selected = None;
        self.input_history.draft = None;
    }

    pub(super) fn clear_history_draft_attachments(&mut self) {
        if let Some(draft) = self.input_history.draft.as_mut() {
            draft
                .attachments
                .remove_tokens_from_input(&mut draft.input, &mut draft.cursor_pos);
            draft.edits = InputEditHistory::default();
        }
    }

    /// Recompute the slash-command popup candidates from the current
    /// input. Called after every input mutation. Clamps the selected
    /// index so it stays valid when the candidate list shrinks.
    pub fn refresh_command_popup(&mut self) {
        if let Some(prefix) = commands::move_position_prefix(&self.input) {
            self.command_popup_candidates.clear();
            self.move_position_candidates = commands::match_move_positions(prefix);
        } else if commands::is_command_prefix(&self.input) {
            // Strip leading whitespace + the `/` to get the user's
            // name query. `is_command_prefix` already guarantees the
            // shape, so the unwrap is safe.
            let trimmed = self.input.trim_start();
            let name = trimmed.strip_prefix('/').unwrap_or("");
            self.command_popup_candidates = commands::matches(name);
            self.move_position_candidates.clear();
        } else {
            self.command_popup_candidates.clear();
            self.move_position_candidates.clear();
        }
        let candidate_count =
            self.command_popup_candidates.len() + self.move_position_candidates.len();
        if candidate_count == 0 {
            self.command_popup_selected = 0;
        } else if self.command_popup_selected >= candidate_count {
            self.command_popup_selected = candidate_count - 1;
        }
    }

    pub fn command_popup_visible(&self) -> bool {
        !self.command_popup_candidates.is_empty() || !self.move_position_candidates.is_empty()
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
