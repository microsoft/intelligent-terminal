use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::prelude::Modifier;
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthStr;

const MULTI_CLICK_WINDOW: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Point {
    x: u16,
    y: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectionKind {
    Linear,
    Word,
    Lines,
}

#[derive(Debug, Clone, Copy)]
struct Selection {
    anchor: Point,
    focus: Point,
    kind: SelectionKind,
    moved: bool,
    atomic: bool,
}

#[derive(Debug, Clone, Copy)]
struct ClickRecord {
    point: Point,
    at: Instant,
    count: u8,
}

#[derive(Default)]
pub(crate) struct TextSelection {
    buffer: Option<Buffer>,
    click_buffer: Option<Buffer>,
    selection: Option<Selection>,
    last_click: Option<ClickRecord>,
}

impl TextSelection {
    pub(crate) fn handle_mouse(&mut self, mouse: MouseEvent) -> Option<String> {
        self.handle_mouse_at(mouse, Instant::now())
    }

    fn handle_mouse_at(&mut self, mouse: MouseEvent, now: Instant) -> Option<String> {
        let point = self
            .buffer
            .as_ref()
            .and_then(|buffer| point_in_buffer(buffer, mouse.column, mouse.row));

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let Some(point) = point else {
                    self.clear();
                    return None;
                };
                let click_count = self.register_click(point, now);
                if click_count == 1 {
                    self.click_buffer = self.buffer.clone();
                } else if let Some(buffer) = self.click_buffer.as_ref() {
                    self.buffer = Some(buffer.clone());
                }
                let buffer = self.buffer.as_ref()?;
                self.selection = Some(match click_count {
                    2 => {
                        let (anchor, focus) = word_bounds(buffer, point);
                        Selection {
                            anchor,
                            focus,
                            kind: SelectionKind::Word,
                            moved: false,
                            atomic: true,
                        }
                    }
                    3 => Selection {
                        anchor: point,
                        focus: point,
                        kind: SelectionKind::Lines,
                        moved: false,
                        atomic: true,
                    },
                    _ => Selection {
                        anchor: point,
                        focus: point,
                        kind: SelectionKind::Linear,
                        moved: false,
                        atomic: false,
                    },
                });
                None
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let Some(point) = point else {
                    return None;
                };
                if let Some(selection) = self.selection.as_mut() {
                    if !selection.atomic {
                        selection.focus = point;
                        selection.moved |= point != selection.anchor;
                    }
                }
                if self.selection.is_some_and(|selection| selection.moved) {
                    self.last_click = None;
                    self.click_buffer = None;
                }
                None
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if let (Some(point), Some(selection)) = (point, self.selection.as_mut()) {
                    if !selection.atomic {
                        selection.focus = point;
                        selection.moved |= point != selection.anchor;
                    }
                }
                let selection = self.selection?;
                if !selection.atomic && !selection.moved {
                    self.selection = None;
                    return None;
                }
                None
            }
            _ => None,
        }
    }

    pub(crate) fn selected_text(&self) -> Option<String> {
        let buffer = self.buffer.as_ref()?;
        let selection = self.selection?;
        selected_text(buffer, selection).filter(|text| !text.is_empty())
    }

    pub(crate) fn click_count(&self) -> Option<u8> {
        self.last_click.map(|click| click.count)
    }

    pub(crate) fn clear(&mut self) {
        self.click_buffer = None;
        self.selection = None;
        self.last_click = None;
    }

    /// Capture the unmodified frame for text extraction, then paint the
    /// selection overlay onto the frame that will be flushed.
    pub(crate) fn snapshot_and_render(&mut self, buffer: &mut Buffer) {
        self.buffer = Some(buffer.clone());
        let Some(selection) = self.selection else {
            return;
        };
        let area = buffer.area;
        for y in area.y..area.y.saturating_add(area.height) {
            for x in area.x..area.x.saturating_add(area.width) {
                let point = Point { x, y };
                if selection_contains(selection, buffer, point) {
                    if let Some(cell) = buffer.cell_mut((x, y)) {
                        let style = cell.style().add_modifier(Modifier::REVERSED);
                        cell.set_style(style);
                    }
                }
            }
        }
    }

    fn register_click(&mut self, point: Point, now: Instant) -> u8 {
        let count = self
            .last_click
            .filter(|record| {
                record.point == point
                    && now.saturating_duration_since(record.at) <= MULTI_CLICK_WINDOW
                    && record.count < 3
            })
            .map_or(1, |record| record.count + 1);
        self.last_click = Some(ClickRecord {
            point,
            at: now,
            count,
        });
        count
    }
}

fn point_in_buffer(buffer: &Buffer, x: u16, y: u16) -> Option<Point> {
    let area = buffer.area;
    let right = area.x.checked_add(area.width)?;
    let bottom = area.y.checked_add(area.height)?;
    if area.width == 0 || area.height == 0 || x < area.x || x >= right || y < area.y || y >= bottom
    {
        return None;
    }
    Some(Point {
        x: normalize_wide_cell_x(buffer, x, y),
        y,
    })
}

fn normalize_wide_cell_x(buffer: &Buffer, x: u16, y: u16) -> u16 {
    if x <= buffer.area.x {
        return x;
    }
    let previous_x = x - 1;
    let Some(previous) = buffer.cell((previous_x, y)) else {
        return x;
    };
    if UnicodeWidthStr::width(previous.symbol()) > 1 {
        previous_x
    } else {
        x
    }
}

fn selection_contains(selection: Selection, buffer: &Buffer, point: Point) -> bool {
    let area = buffer.area;
    let min_y = selection.anchor.y.min(selection.focus.y);
    let max_y = selection.anchor.y.max(selection.focus.y);
    if point.y < min_y || point.y > max_y {
        return false;
    }
    match selection.kind {
        SelectionKind::Lines => point.x >= area.x && point.x < area.x.saturating_add(area.width),
        SelectionKind::Word => {
            let min_x = selection.anchor.x.min(selection.focus.x);
            let max_x = selection.anchor.x.max(selection.focus.x);
            let max_x = cell_end_x(buffer, max_x, point.y);
            point.x >= min_x && point.x <= max_x
        }
        SelectionKind::Linear => {
            let (start, end) = ordered_points(selection.anchor, selection.focus);
            let end_x = cell_end_x(buffer, end.x, end.y);
            if start.y == end.y {
                point.x >= start.x && point.x <= end_x
            } else if point.y == start.y {
                point.x >= start.x
            } else if point.y == end.y {
                point.x <= end_x
            } else {
                true
            }
        }
    }
}

fn selected_text(buffer: &Buffer, selection: Selection) -> Option<String> {
    let area = buffer.area;
    let right = area.x.checked_add(area.width)?.checked_sub(1)?;
    let (start, end) = ordered_points(selection.anchor, selection.focus);

    let mut rows = Vec::new();
    for y in start.y..=end.y {
        let (min_x, max_x) = match selection.kind {
            SelectionKind::Lines => (area.x, right),
            SelectionKind::Word => (start.x, end.x),
            SelectionKind::Linear if start.y == end.y => (start.x, end.x),
            SelectionKind::Linear if y == start.y => (start.x, right),
            SelectionKind::Linear if y == end.y => (area.x, end.x),
            SelectionKind::Linear => (area.x, right),
        };
        let mut row = String::new();
        let mut x = min_x;
        while x <= max_x {
            let cell = buffer.cell((x, y))?;
            let symbol = cell.symbol();
            row.push_str(symbol);
            let width = UnicodeWidthStr::width(symbol).max(1);
            x = x.saturating_add(width as u16);
            if x == u16::MAX && x <= max_x {
                break;
            }
        }
        rows.push(row.trim_end_matches(char::is_whitespace).to_string());
    }
    Some(rows.join("\r\n"))
}

fn ordered_points(first: Point, second: Point) -> (Point, Point) {
    if (first.y, first.x) <= (second.y, second.x) {
        (first, second)
    } else {
        (second, first)
    }
}

fn cell_end_x(buffer: &Buffer, x: u16, y: u16) -> u16 {
    buffer
        .cell((x, y))
        .map(|cell| UnicodeWidthStr::width(cell.symbol()).max(1) as u16)
        .and_then(|width| x.checked_add(width - 1))
        .unwrap_or(x)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CellClass {
    Whitespace,
    Word,
    Punctuation,
}

fn word_bounds(buffer: &Buffer, point: Point) -> (Point, Point) {
    let class = cell_class(buffer, point.x, point.y);
    let mut start = point.x;
    while let Some(previous) = previous_cell_x(buffer, start, point.y) {
        if cell_class(buffer, previous, point.y) != class {
            break;
        }
        start = previous;
    }
    let mut end = point.x;
    while let Some(next) = next_cell_x(buffer, end, point.y) {
        if cell_class(buffer, next, point.y) != class {
            break;
        }
        end = next;
    }
    (
        Point {
            x: start,
            y: point.y,
        },
        Point { x: end, y: point.y },
    )
}

fn previous_cell_x(buffer: &Buffer, x: u16, y: u16) -> Option<u16> {
    (x > buffer.area.x).then(|| normalize_wide_cell_x(buffer, x - 1, y))
}

fn next_cell_x(buffer: &Buffer, x: u16, y: u16) -> Option<u16> {
    let cell = buffer.cell((x, y))?;
    let width = UnicodeWidthStr::width(cell.symbol()).max(1) as u16;
    let next = x.checked_add(width)?;
    let right = buffer.area.x.checked_add(buffer.area.width)?;
    (next < right).then_some(next)
}

fn cell_class(buffer: &Buffer, x: u16, y: u16) -> CellClass {
    let symbol = buffer
        .cell((normalize_wide_cell_x(buffer, x, y), y))
        .map(|cell| cell.symbol())
        .unwrap_or(" ");
    let Some(character) = symbol.chars().next() else {
        return CellClass::Whitespace;
    };
    if character.is_whitespace() {
        CellClass::Whitespace
    } else if character.is_alphanumeric() || character == '_' {
        CellClass::Word
    } else {
        CellClass::Punctuation
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;
    use ratatui::prelude::{Line, Rect};

    fn buffer() -> Buffer {
        Buffer::with_lines([
            Line::from("alpha beta  "),
            Line::from("gamma delta "),
            Line::from("宽字 ok      "),
        ])
    }

    fn mouse(kind: MouseEventKind, x: u16, y: u16, modifiers: KeyModifiers) -> MouseEvent {
        MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers,
        }
    }

    fn seeded_selection() -> TextSelection {
        let mut selection = TextSelection::default();
        let mut buffer = buffer();
        selection.snapshot_and_render(&mut buffer);
        selection
    }

    #[test]
    fn drag_selects_linear_text_across_lines() {
        let mut state = seeded_selection();
        let now = Instant::now();
        state.handle_mouse_at(
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                6,
                0,
                KeyModifiers::NONE,
            ),
            now,
        );
        state.handle_mouse_at(
            mouse(
                MouseEventKind::Drag(MouseButton::Left),
                4,
                1,
                KeyModifiers::NONE,
            ),
            now,
        );
        assert_eq!(
            state.handle_mouse_at(
                mouse(
                    MouseEventKind::Up(MouseButton::Left),
                    4,
                    1,
                    KeyModifiers::NONE,
                ),
                now,
            ),
            None
        );
        assert_eq!(state.selected_text().as_deref(), Some("beta\r\ngamma"));
    }

    #[test]
    fn mouse_release_does_not_return_text_for_automatic_copy() {
        let mut state = seeded_selection();
        let now = Instant::now();
        state.handle_mouse_at(
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                0,
                0,
                KeyModifiers::NONE,
            ),
            now,
        );
        state.handle_mouse_at(
            mouse(
                MouseEventKind::Drag(MouseButton::Left),
                4,
                1,
                KeyModifiers::NONE,
            ),
            now,
        );
        assert_eq!(
            state.handle_mouse_at(
                mouse(
                    MouseEventKind::Up(MouseButton::Left),
                    4,
                    1,
                    KeyModifiers::NONE,
                ),
                now,
            ),
            None
        );
        assert!(state.selected_text().is_some());
    }

    #[test]
    fn reverse_drag_selects_the_same_linear_text() {
        let mut state = seeded_selection();
        let now = Instant::now();
        state.handle_mouse_at(
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                4,
                1,
                KeyModifiers::NONE,
            ),
            now,
        );
        state.handle_mouse_at(
            mouse(
                MouseEventKind::Drag(MouseButton::Left),
                6,
                0,
                KeyModifiers::NONE,
            ),
            now,
        );
        state.handle_mouse_at(
            mouse(
                MouseEventKind::Up(MouseButton::Left),
                6,
                0,
                KeyModifiers::NONE,
            ),
            now,
        );
        assert_eq!(state.selected_text().as_deref(), Some("beta\r\ngamma"));
    }

    #[test]
    fn linear_selection_includes_complete_intermediate_rows() {
        let mut state = seeded_selection();
        let now = Instant::now();
        state.handle_mouse_at(
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                6,
                0,
                KeyModifiers::NONE,
            ),
            now,
        );
        state.handle_mouse_at(
            mouse(
                MouseEventKind::Drag(MouseButton::Left),
                3,
                2,
                KeyModifiers::NONE,
            ),
            now,
        );
        state.handle_mouse_at(
            mouse(
                MouseEventKind::Up(MouseButton::Left),
                3,
                2,
                KeyModifiers::NONE,
            ),
            now,
        );
        assert_eq!(
            state.selected_text().as_deref(),
            Some("beta\r\ngamma delta\r\n宽字")
        );
    }

    #[test]
    fn shift_drag_uses_the_same_linear_selection() {
        let mut state = seeded_selection();
        let now = Instant::now();
        state.handle_mouse_at(
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                6,
                0,
                KeyModifiers::SHIFT,
            ),
            now,
        );
        state.handle_mouse_at(
            mouse(
                MouseEventKind::Drag(MouseButton::Left),
                2,
                1,
                KeyModifiers::SHIFT,
            ),
            now,
        );
        state.handle_mouse_at(
            mouse(
                MouseEventKind::Up(MouseButton::Left),
                2,
                1,
                KeyModifiers::SHIFT,
            ),
            now,
        );
        assert_eq!(state.selected_text().as_deref(), Some("beta\r\ngam"));
    }

    #[test]
    fn double_click_selects_word_and_triple_click_selects_line() {
        let mut state = seeded_selection();
        let now = Instant::now();
        let down = mouse(
            MouseEventKind::Down(MouseButton::Left),
            7,
            0,
            KeyModifiers::NONE,
        );
        let up = mouse(
            MouseEventKind::Up(MouseButton::Left),
            7,
            0,
            KeyModifiers::NONE,
        );

        state.handle_mouse_at(down, now);
        assert_eq!(state.handle_mouse_at(up, now), None);
        state.handle_mouse_at(down, now + Duration::from_millis(100));
        state.handle_mouse_at(up, now + Duration::from_millis(100));
        assert_eq!(state.selected_text().as_deref(), Some("beta"));
        state.handle_mouse_at(down, now + Duration::from_millis(200));
        state.handle_mouse_at(up, now + Duration::from_millis(200));
        assert_eq!(state.selected_text().as_deref(), Some("alpha beta"));
    }

    #[test]
    fn selection_extracts_wide_unicode_once() {
        let mut state = seeded_selection();
        let now = Instant::now();
        state.handle_mouse_at(
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                0,
                2,
                KeyModifiers::NONE,
            ),
            now,
        );
        state.handle_mouse_at(
            mouse(
                MouseEventKind::Drag(MouseButton::Left),
                3,
                2,
                KeyModifiers::NONE,
            ),
            now,
        );
        state.handle_mouse_at(
            mouse(
                MouseEventKind::Up(MouseButton::Left),
                3,
                2,
                KeyModifiers::NONE,
            ),
            now,
        );
        assert_eq!(state.selected_text().as_deref(), Some("宽字"));
    }

    #[test]
    fn double_click_selects_adjacent_wide_characters() {
        let mut state = seeded_selection();
        let now = Instant::now();
        let down = mouse(
            MouseEventKind::Down(MouseButton::Left),
            0,
            2,
            KeyModifiers::NONE,
        );
        let up = mouse(
            MouseEventKind::Up(MouseButton::Left),
            0,
            2,
            KeyModifiers::NONE,
        );

        state.handle_mouse_at(down, now);
        state.handle_mouse_at(up, now);
        state.handle_mouse_at(down, now + Duration::from_millis(100));
        state.handle_mouse_at(up, now + Duration::from_millis(100));
        assert_eq!(state.selected_text().as_deref(), Some("宽字"));
    }

    #[test]
    fn clearing_selection_resets_multi_click_sequence() {
        let mut state = seeded_selection();
        let now = Instant::now();
        let down = mouse(
            MouseEventKind::Down(MouseButton::Left),
            1,
            0,
            KeyModifiers::NONE,
        );
        let up = mouse(
            MouseEventKind::Up(MouseButton::Left),
            1,
            0,
            KeyModifiers::NONE,
        );

        state.handle_mouse_at(down, now);
        state.handle_mouse_at(up, now);
        state.clear();
        state.handle_mouse_at(down, now + Duration::from_millis(100));
        assert_eq!(
            state.handle_mouse_at(up, now + Duration::from_millis(100)),
            None
        );
    }

    #[test]
    fn render_highlights_selection_without_polluting_snapshot() {
        let mut state = seeded_selection();
        let now = Instant::now();
        state.handle_mouse_at(
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                0,
                0,
                KeyModifiers::NONE,
            ),
            now,
        );
        state.handle_mouse_at(
            mouse(
                MouseEventKind::Drag(MouseButton::Left),
                1,
                0,
                KeyModifiers::NONE,
            ),
            now,
        );

        let mut rendered = buffer();
        state.snapshot_and_render(&mut rendered);
        assert!(rendered[(0, 0)].modifier.contains(Modifier::REVERSED));
        assert!(!state.buffer.as_ref().unwrap()[(0, 0)]
            .modifier
            .contains(Modifier::REVERSED));
    }

    #[test]
    fn render_highlights_both_columns_of_a_wide_character() {
        let mut state = seeded_selection();
        let now = Instant::now();
        state.handle_mouse_at(
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                0,
                2,
                KeyModifiers::NONE,
            ),
            now,
        );
        state.handle_mouse_at(
            mouse(
                MouseEventKind::Drag(MouseButton::Left),
                0,
                2,
                KeyModifiers::NONE,
            ),
            now,
        );
        state.selection.as_mut().unwrap().moved = true;

        let mut rendered = buffer();
        state.snapshot_and_render(&mut rendered);
        assert!(rendered[(0, 2)].modifier.contains(Modifier::REVERSED));
        assert!(rendered[(1, 2)].modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn clicks_outside_buffer_clear_selection() {
        let mut state = seeded_selection();
        state.selection = Some(Selection {
            anchor: Point { x: 0, y: 0 },
            focus: Point { x: 1, y: 0 },
            kind: SelectionKind::Linear,
            moved: true,
            atomic: false,
        });
        state.handle_mouse_at(
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                40,
                40,
                KeyModifiers::NONE,
            ),
            Instant::now(),
        );
        assert!(state.selection.is_none());
    }

    #[test]
    fn empty_buffer_rejects_points() {
        let buffer = Buffer::empty(Rect::new(0, 0, 0, 0));
        assert_eq!(point_in_buffer(&buffer, 0, 0), None);
    }
}
