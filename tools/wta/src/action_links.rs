use std::io::{self, Write};

use crossterm::{queue, style::Print};
use ratatui::{
    backend::{Backend, CrosstermBackend},
    buffer::{Buffer, Cell},
};
use unicode_width::UnicodeWidthStr;

const CLOSE_HYPERLINK: &str = "\x1b]8;;\x1b\\";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompletedTurnAction {
    Collapse,
    Expand,
}

impl CompletedTurnAction {
    fn open_hyperlink(self) -> &'static str {
        match self {
            Self::Collapse => "\x1b]8;;wta-action://collapse\x1b\\",
            Self::Expand => "\x1b]8;;wta-action://expand\x1b\\",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CompletedTurnActionLink {
    pub(crate) start_column: u16,
    pub(crate) end_column: u16,
    pub(crate) row: u16,
    pub(crate) action: CompletedTurnAction,
}

#[derive(Debug, Clone)]
struct PositionedCell {
    column: u16,
    row: u16,
    cell: Cell,
}

#[derive(Debug, Clone)]
struct ActionCells {
    action: CompletedTurnAction,
    cells: Vec<PositionedCell>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ActionLinkOverlay {
    clear: Vec<PositionedCell>,
    actions: Vec<ActionCells>,
}

pub(crate) fn build_overlay(
    buffer: &Buffer,
    previous: &[CompletedTurnActionLink],
    current: &[CompletedTurnActionLink],
) -> ActionLinkOverlay {
    ActionLinkOverlay {
        clear: previous
            .iter()
            .flat_map(|region| cells_in_region(buffer, *region))
            .collect(),
        actions: current
            .iter()
            .map(|region| ActionCells {
                action: region.action,
                cells: cells_in_region(buffer, *region),
            })
            .filter(|action| !action.cells.is_empty())
            .collect(),
    }
}

pub(crate) fn paint<W: Write>(
    backend: &mut CrosstermBackend<W>,
    overlay: &ActionLinkOverlay,
) -> io::Result<()> {
    draw_cells(backend, &overlay.clear)?;
    for action in &overlay.actions {
        queue!(backend, Print(action.action.open_hyperlink()))?;
        draw_cells(backend, &action.cells)?;
        queue!(backend, Print(CLOSE_HYPERLINK))?;
    }
    Ok(())
}

fn draw_cells<W: Write>(
    backend: &mut CrosstermBackend<W>,
    cells: &[PositionedCell],
) -> io::Result<()> {
    Backend::draw(
        backend,
        cells.iter().map(|cell| (cell.column, cell.row, &cell.cell)),
    )
}

fn cells_in_region(buffer: &Buffer, region: CompletedTurnActionLink) -> Vec<PositionedCell> {
    let area = buffer.area;
    if region.row < area.y || region.row >= area.y.saturating_add(area.height) {
        return Vec::new();
    }

    let mut cells = Vec::new();
    let mut column = region.start_column.max(area.x);
    let end = region.end_column.min(area.x.saturating_add(area.width));
    while column < end {
        let Some(cell) = buffer.cell((column, region.row)) else {
            break;
        };
        if cell.skip {
            column = column.saturating_add(1);
            continue;
        }
        let width = UnicodeWidthStr::width(cell.symbol()).max(1);
        cells.push(PositionedCell {
            column,
            row: region.row,
            cell: cell.clone(),
        });
        column = column.saturating_add(width.min(u16::MAX as usize) as u16);
    }
    cells
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{
        buffer::Buffer,
        layout::Rect,
        style::{Color, Modifier, Style},
    };

    #[test]
    fn overlay_serializes_stateful_links_and_skips_wide_cell_tails() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 6, 1));
        buffer.set_string(0, 0, "界 abc", ratatui::style::Style::default());
        let current = [CompletedTurnActionLink {
            start_column: 0,
            end_column: 6,
            row: 0,
            action: CompletedTurnAction::Collapse,
        }];
        let overlay = build_overlay(&buffer, &[], &current);
        let mut bytes = Vec::new();
        {
            let mut backend = CrosstermBackend::new(&mut bytes);
            paint(&mut backend, &overlay).expect("action overlay must serialize");
        }
        let output = String::from_utf8(bytes).expect("VT output is UTF-8");

        assert!(output.contains("wta-action://collapse"));
        assert!(output.contains("界"));
        assert!(output.contains(CLOSE_HYPERLINK));
    }

    #[test]
    fn overlay_repaints_previous_regions_without_action_metadata() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 6, 1));
        buffer.set_string(0, 0, "plain", ratatui::style::Style::default());
        let previous = [CompletedTurnActionLink {
            start_column: 0,
            end_column: 6,
            row: 0,
            action: CompletedTurnAction::Expand,
        }];
        let overlay = build_overlay(&buffer, &previous, &[]);
        let mut bytes = Vec::new();
        {
            let mut backend = CrosstermBackend::new(&mut bytes);
            paint(&mut backend, &overlay).expect("stale action overlay must clear");
        }
        let output = String::from_utf8(bytes).expect("VT output is UTF-8");

        assert!(output.contains("plain"));
        assert!(!output.contains("wta-action://"));
    }

    #[test]
    fn overlay_preserves_full_rows_and_cell_styles() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 8, 2));
        let style = Style::default()
            .fg(Color::Cyan)
            .bg(Color::Black)
            .add_modifier(Modifier::BOLD);
        buffer.set_string(0, 0, "界 row", style);
        for column in 0..8 {
            buffer[(column, 1)].set_style(style);
        }
        let current = [
            CompletedTurnActionLink {
                start_column: 0,
                end_column: 8,
                row: 0,
                action: CompletedTurnAction::Collapse,
            },
            CompletedTurnActionLink {
                start_column: 0,
                end_column: 8,
                row: 1,
                action: CompletedTurnAction::Collapse,
            },
        ];

        let overlay = build_overlay(&buffer, &[], &current);

        assert_eq!(overlay.actions.len(), 2);
        assert_eq!(
            overlay.actions[0]
                .cells
                .iter()
                .map(|cell| cell.column)
                .collect::<Vec<_>>(),
            vec![0, 2, 3, 4, 5, 6, 7],
            "wide-cell tails must be skipped while trailing blank cells remain covered",
        );
        assert_eq!(overlay.actions[1].cells.len(), 8);
        for action in &overlay.actions {
            for positioned in &action.cells {
                let original = &buffer[(positioned.column, positioned.row)];
                assert_eq!(positioned.cell.symbol(), original.symbol());
                assert_eq!(positioned.cell.style(), original.style());
            }
        }
    }
}
