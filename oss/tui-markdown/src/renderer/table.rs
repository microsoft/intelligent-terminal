//! Table rendering support for tui-markdown.
//!
//! A table must be buffered before rendering because every cell can increase its column's terminal
//! display width. [`TableBuilder`] collects the header and body rows, then renders their content,
//! alignment, padding, and Unicode box-drawing borders once pulldown-cmark closes the table.
//!
//! The central renderer dispatches events and owns shared inline state. This module owns the table
//! event handlers, buffered table state, list-aware output placement, and final table layout.

use pulldown_cmark::Alignment;
use ratatui_core::style::Style;
use ratatui_core::text::{Line, Span, StyledGrapheme};

use super::TextWriter;
use crate::StyleSheet;

const HORIZONTAL_BORDER: char = '─';
const VERTICAL_BORDER: &str = "│";
const TOP_BORDER: BorderGlyphs = BorderGlyphs::new('┌', '┬', '┐');
const HEADER_SEPARATOR: BorderGlyphs = BorderGlyphs::new('├', '┼', '┤');
const BOTTOM_BORDER: BorderGlyphs = BorderGlyphs::new('└', '┴', '┘');

impl<'a, 'theme, I, S> TextWriter<'a, 'theme, I, S>
where
    I: Iterator<Item = pulldown_cmark::Event<'a>>,
    S: StyleSheet,
{
    pub fn start_table(&mut self, alignments: Vec<Alignment>) {
        if self.needs_newline {
            self.push_line(Line::default());
        }
        self.table_builder = Some(TableBuilder::new(alignments));
        self.needs_newline = false;
    }

    pub fn end_table_header(&mut self) {
        if let Some(builder) = &mut self.table_builder {
            builder.finish_header();
        }
    }

    pub fn end_table_row(&mut self) {
        if let Some(builder) = &mut self.table_builder {
            builder.finish_row();
        }
    }

    pub fn start_table_cell(&mut self) {
        if let Some(builder) = &mut self.table_builder {
            builder.start_cell();
        }
    }

    pub fn end_table_cell(&mut self) {
        if let Some(builder) = &mut self.table_builder {
            builder.finish_cell();
        }
    }

    pub fn end_table(&mut self) {
        if let Some(builder) = self.table_builder.take() {
            // Reserve the enclosing prefixes, their shared trailing space, and list indentation
            // before laying out the table. The remaining width includes its borders and padding.
            let prefix_width = self.line_prefixes.iter().map(Span::width).sum::<usize>()
                + usize::from(!self.line_prefixes.is_empty());
            let indent = self
                .list_items
                .last()
                .map_or(0, |item| item.continuation_width);
            let width = self
                .table_width
                .map(|width| usize::from(width).saturating_sub(prefix_width + indent));
            let lines = builder.render(&self.styles, width);
            self.push_table_lines(lines);
            self.needs_newline = true;
        }
    }

    /// Adds a buffered table to the output while preserving an active list item's layout.
    ///
    /// A table that is the first content in an item starts on the marker line. Its remaining lines
    /// are indented by the marker's display width. A later table cannot reuse the marker line, but
    /// all of its lines still need the continuation indentation.
    ///
    /// Table rendering currently puts styles on individual spans and leaves the line style and
    /// alignment at their defaults. This makes it safe to move the first rendered line's spans
    /// onto the existing marker line.
    fn push_table_lines(&mut self, lines: Vec<Line<'a>>) {
        let Some(list_item) = self.list_items.last().copied() else {
            for line in lines {
                self.push_line(line);
            }
            return;
        };

        let mut lines = lines.into_iter();
        // The line position alone is insufficient: inline item content may already have appended
        // spans to the marker line before the table was buffered.
        let marker_line_is_last = self.text.lines.len() == list_item.marker_line + 1;
        let marker_has_no_content =
            self.text.lines[list_item.marker_line].spans.len() == list_item.marker_span_count;
        let table_starts_on_marker = marker_line_is_last && marker_has_no_content;
        if table_starts_on_marker {
            if let Some(first_line) = lines.next() {
                self.text.lines[list_item.marker_line]
                    .spans
                    .extend(first_line.spans);
            }
        }

        let continuation = " ".repeat(list_item.continuation_width);
        for mut line in lines {
            line.spans.insert(0, Span::raw(continuation.clone()));
            self.push_line(line);
        }
    }
}

/// Accumulates a complete table before calculating its column widths and rendering it.
///
/// The parent renderer starts and finishes each cell as pulldown-cmark emits table events. It then
/// finishes the header or body row and calls [`Self::render`] after the table closes.
pub struct TableBuilder<'a> {
    alignments: Vec<Alignment>,
    header: TableHeader<'a>,
    rows: Vec<TableRow<'a>>,
    current_row: TableRow<'a>,
    current_cell: TableCell<'a>,
}

impl<'a> TableBuilder<'a> {
    pub fn new(alignments: Vec<Alignment>) -> Self {
        Self {
            alignments,
            header: TableHeader::default(),
            rows: Vec::new(),
            current_row: TableRow::default(),
            current_cell: TableCell::default(),
        }
    }

    pub fn start_cell(&mut self) {
        self.current_cell = TableCell::default();
    }

    pub fn push_span(&mut self, span: Span<'a>) {
        self.current_cell.push(span);
    }

    pub fn finish_cell(&mut self) {
        let cell = std::mem::take(&mut self.current_cell);
        self.current_row.cells.push(cell);
    }

    pub fn finish_header(&mut self) {
        self.header.cells = std::mem::take(&mut self.current_row.cells);
    }

    pub fn finish_row(&mut self) {
        self.rows.push(std::mem::take(&mut self.current_row));
    }

    pub fn render<S: StyleSheet>(self, styles: &S, width: Option<usize>) -> Vec<Line<'a>> {
        let column_count = self.column_count();
        if column_count == 0 {
            return Vec::new();
        }

        let mut column_widths = self.column_widths(column_count);
        if let Some(width) = width {
            self.fit_columns(&mut column_widths, width);
        }
        let border_style = styles.table_border();

        let top_border = TOP_BORDER.render(&column_widths, border_style);
        let header = self.header.render(&column_widths, &self.alignments, styles);
        let header_separator = HEADER_SEPARATOR.render(&column_widths, border_style);
        let body = self
            .rows
            .iter()
            .flat_map(|row| row.render(&column_widths, &self.alignments, styles));
        let bottom_border = BOTTOM_BORDER.render(&column_widths, border_style);

        let mut lines = vec![top_border];
        lines.extend(header);
        lines.push(header_separator);
        lines.extend(body);
        lines.push(bottom_border);
        lines
    }

    /// Share available space between columns, stopping each at its natural width.
    fn fit_columns(&self, widths: &mut [usize], width: usize) {
        // Each column needs two padding spaces and a right border, plus the table's left border.
        let budget = width.saturating_sub(3 * widths.len() + 1);
        if widths.iter().sum::<usize>() <= budget {
            return;
        }
        let natural_widths = widths.to_vec();
        widths.fill(1);
        for cells in
            std::iter::once(&self.header.cells).chain(self.rows.iter().map(|row| &row.cells))
        {
            for (column, cell) in cells.iter().enumerate() {
                widths[column] = widths[column].max(cell.minimum_width());
            }
        }

        // Start at the indivisible grapheme minimums, even if those exceed the pane width.
        // Grow the narrowest unfinished column, giving equal-width ties to the leftmost column.
        let remaining = budget.saturating_sub(widths.iter().sum());
        for _ in 0..remaining {
            let column = (0..widths.len())
                .filter(|&column| widths[column] < natural_widths[column])
                .min_by_key(|&column| widths[column]);
            let Some(column) = column else { break };
            widths[column] += 1;
        }
    }

    fn column_count(&self) -> usize {
        self.alignments.len().max(self.header.cells.len()).max(
            self.rows
                .iter()
                .map(|row| row.cells.len())
                .max()
                .unwrap_or(0),
        )
    }

    fn column_widths(&self, column_count: usize) -> Vec<usize> {
        let mut widths = vec![0; column_count];
        for (col_idx, cell) in self.header.cells.iter().enumerate() {
            widths[col_idx] = widths[col_idx].max(cell.width());
        }
        for row in &self.rows {
            for (col_idx, cell) in row.cells.iter().enumerate() {
                widths[col_idx] = widths[col_idx].max(cell.width());
            }
        }
        for width in &mut widths {
            *width = (*width).max(1);
        }
        widths
    }
}

#[derive(Default)]
struct TableHeader<'a> {
    cells: Vec<TableCell<'a>>,
}

impl<'a> TableHeader<'a> {
    fn render<S: StyleSheet>(
        &self,
        column_widths: &[usize],
        alignments: &[Alignment],
        styles: &S,
    ) -> Vec<Line<'a>> {
        render_lines(
            &self.cells,
            column_widths,
            alignments,
            styles.table_header(),
            styles.table_border(),
        )
    }
}

#[derive(Default)]
struct TableRow<'a> {
    cells: Vec<TableCell<'a>>,
}

impl<'a> TableRow<'a> {
    fn render<S: StyleSheet>(
        &self,
        column_widths: &[usize],
        alignments: &[Alignment],
        styles: &S,
    ) -> Vec<Line<'a>> {
        render_lines(
            &self.cells,
            column_widths,
            alignments,
            styles.table_cell(),
            styles.table_border(),
        )
    }
}

#[derive(Default)]
struct TableCell<'a> {
    spans: Vec<Span<'a>>,
}

impl<'a> TableCell<'a> {
    fn push(&mut self, span: Span<'a>) {
        self.spans.push(span);
    }

    fn width(&self) -> usize {
        self.spans.iter().map(Span::width).sum()
    }

    /// Wrap at whitespace when possible, splitting long words only at grapheme boundaries.
    fn wrap(&self, width: usize) -> Vec<Self> {
        if self.width() <= width {
            return vec![Self {
                spans: self.spans.clone(),
            }];
        }
        let graphemes: Vec<_> = self
            .spans
            .iter()
            .flat_map(|span| span.styled_graphemes(Style::default()))
            .collect();
        let mut remaining = graphemes.as_slice();
        let mut lines = Vec::new();
        while !remaining.is_empty() {
            let end = cell_line_end(remaining, width);
            let (line, rest) = remaining.split_at(end);
            lines.push(Self::from_graphemes(line));
            // Whitespace at a wrap boundary separates words; it is not next-line indentation.
            let next_word = rest
                .iter()
                .position(|grapheme| !grapheme.symbol.chars().all(char::is_whitespace))
                .unwrap_or(rest.len());
            remaining = &rest[next_word..];
        }
        lines
    }

    /// The widest indivisible grapheme determines how narrow a column can be.
    fn minimum_width(&self) -> usize {
        self.spans
            .iter()
            .flat_map(|span| span.styled_graphemes(Style::default()))
            .map(|grapheme| Span::raw(grapheme.symbol).width())
            .max()
            .unwrap_or(1)
    }

    fn from_graphemes(graphemes: &[StyledGrapheme<'_>]) -> Self {
        // Recombine equal styles so wrapping does not create one span per character.
        let mut spans: Vec<Span<'a>> = Vec::new();
        for grapheme in graphemes {
            if let Some(span) = spans.last_mut().filter(|span| span.style == grapheme.style) {
                span.content.to_mut().push_str(grapheme.symbol);
            } else {
                spans.push(Span::styled(grapheme.symbol.to_owned(), grapheme.style));
            }
        }
        Self { spans }
    }

    fn render_spans(
        &self,
        column_width: usize,
        alignment: Alignment,
        style: Style,
    ) -> Vec<Span<'a>> {
        let (pad_left, pad_right) = padding(column_width, self.width(), alignment);
        let mut spans = vec![Span::styled(" ".repeat(pad_left + 1), style)];

        for span in &self.spans {
            let mut span = span.clone();
            span.style = span.style.patch(style);
            spans.push(span);
        }
        spans.push(Span::styled(" ".repeat(pad_right + 1), style));
        spans
    }
}

/// Prefer the last word boundary that fits; split an oversized word at a grapheme boundary.
fn cell_line_end(graphemes: &[StyledGrapheme<'_>], width: usize) -> usize {
    let mut used = 0;
    let mut word_boundary = None;
    for (index, grapheme) in graphemes.iter().enumerate() {
        let is_space = grapheme.symbol.chars().all(char::is_whitespace);
        if is_space && index > 0 {
            word_boundary = Some(index);
        }
        used += Span::raw(grapheme.symbol).width();
        if used > width && index > 0 {
            return word_boundary.unwrap_or(index);
        }
    }
    graphemes.len()
}

#[derive(Clone, Copy)]
struct BorderGlyphs {
    left: char,
    intersection: char,
    right: char,
}

impl BorderGlyphs {
    const fn new(left: char, intersection: char, right: char) -> Self {
        Self {
            left,
            intersection,
            right,
        }
    }

    fn render<'a>(self, column_widths: &[usize], style: Style) -> Line<'a> {
        let mut border = String::new();
        border.push(self.left);
        for (index, width) in column_widths.iter().enumerate() {
            for _ in 0..(width + 2) {
                border.push(HORIZONTAL_BORDER);
            }
            if index + 1 < column_widths.len() {
                border.push(self.intersection);
            }
        }
        border.push(self.right);
        Line::from(Span::styled(border, style))
    }
}

fn render_lines<'a>(
    cells: &[TableCell<'a>],
    column_widths: &[usize],
    alignments: &[Alignment],
    content_style: Style,
    border_style: Style,
) -> Vec<Line<'a>> {
    let empty_cell = TableCell::default();
    let wrapped: Vec<_> = column_widths
        .iter()
        .enumerate()
        .map(|(index, &width)| cells.get(index).unwrap_or(&empty_cell).wrap(width))
        .collect();
    // All cells share the tallest cell's row height; shorter cells render styled blank padding.
    let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
    (0..height)
        .map(|row| {
            let mut spans = vec![Span::styled(VERTICAL_BORDER, border_style)];
            for (column, &width) in column_widths.iter().enumerate() {
                let cell = wrapped[column].get(row).unwrap_or(&empty_cell);
                let alignment = alignments.get(column).copied().unwrap_or(Alignment::None);
                spans.extend(cell.render_spans(width, alignment, content_style));
                spans.push(Span::styled(VERTICAL_BORDER, border_style));
            }
            Line::from(spans)
        })
        .collect()
}

fn padding(column_width: usize, content_width: usize, alignment: Alignment) -> (usize, usize) {
    if content_width >= column_width {
        return (0, 0);
    }
    let total_pad = column_width - content_width;
    match alignment {
        Alignment::Left | Alignment::None => (0, total_pad),
        Alignment::Right => (total_pad, 0),
        Alignment::Center => {
            let left = total_pad / 2;
            (left, total_pad - left)
        }
    }
}

#[cfg(test)]
mod tests {
    use indoc::indoc;
    use pretty_assertions::assert_eq;
    use ratatui_core::style::{Style, Stylize};
    use ratatui_core::text::Span;
    use rstest::rstest;

    use super::*;
    use crate::{from_str, from_str_with_options, DefaultStyleSheet, Options, StyleSheet};

    // Layout-only tests use exact equality: display snapshots normalize trailing whitespace.
    #[test]
    fn wide_table_wraps_without_losing_content() {
        let options = Options::default().table_width(40);

        let text = from_str_with_options(
            indoc! {"
                | iOS concept | Android reality |
                | --- | --- |
                | productCatalog.createWithoutStore branch | No v0 equivalent. The store is created up front, so there's no deferred path. |
                | recommendedActions non-empty | No such field on the engine response. |
            "},
            &options,
        );

        assert_eq!(
            text.to_string(),
            indoc! {"
                ┌───────────────────┬──────────────────┐
                │ iOS concept       │ Android reality  │
                ├───────────────────┼──────────────────┤
                │ productCatalog.cr │ No v0            │
                │ eateWithoutStore  │ equivalent. The  │
                │ branch            │ store is created │
                │                   │ up front, so     │
                │                   │ there's no       │
                │                   │ deferred path.   │
                │ recommendedAction │ No such field on │
                │ s non-empty       │ the engine       │
                │                   │ response.        │
                └───────────────────┴──────────────────┘"}
        );
    }

    #[test]
    fn wrapped_rows_keep_alignment_and_padding() {
        let options = Options::default().table_width(19);

        let text = from_str_with_options(
            indoc! {"
                | L | R | C |
                | :-- | --: | :-: |
                | a bb ccc | a bb ccc | a bb ccc |
            "},
            &options,
        );

        assert_eq!(
            text.to_string(),
            indoc! {"
                ┌─────┬─────┬─────┐
                │ L   │   R │  C  │
                ├─────┼─────┼─────┤
                │ a   │   a │  a  │
                │ bb  │  bb │ bb  │
                │ ccc │ ccc │ ccc │
                └─────┴─────┴─────┘"}
        );
    }

    #[test]
    fn wrapping_preserves_inline_styles() {
        let options = Options::default().table_width(6);
        let text = from_str_with_options(
            indoc! {"
                | H |
                | --- |
                | **ab***cd* |
            "},
            &options,
        );

        insta::assert_debug_snapshot!(text);
        insta::assert_snapshot!(text, @"
        ┌────┐
        │ H  │
        ├────┤
        │ ab │
        │ cd │
        └────┘
        ");
    }

    #[test]
    fn wrapping_preserves_graphemes() {
        let options = Options::default().table_width(6);

        let text = from_str_with_options(
            indoc! {"
                | H |
                | --- |
                | 👩‍💻é界 |
            "},
            &options,
        );

        assert_eq!(
            text.to_string(),
            indoc! {"
                ┌────┐
                │ H  │
                ├────┤
                │ 👩‍💻 │
                │ é  │
                │ 界 │
                └────┘"}
        );
    }

    #[test]
    fn wrapped_table_in_blockquote() {
        let options = Options::default().table_width(24);

        let text = from_str_with_options(
            indoc! {"
                > | Header | Value |
                > | --- | --- |
                > | long words here | more content here |
            "},
            &options,
        );

        assert_eq!(
            text.to_string(),
            indoc! {"
                > ┌──────────┬─────────┐
                > │ Header   │ Value   │
                > ├──────────┼─────────┤
                > │ long     │ more    │
                > │ words    │ content │
                > │ here     │ here    │
                > └──────────┴─────────┘"}
        );
    }

    #[test]
    fn wrapped_table_in_list() {
        let options = Options::default().table_width(24);

        let text = from_str_with_options(
            indoc! {"
                - | Header | Value |
                  | --- | --- |
                  | long words here | more content here |
            "},
            &options,
        );

        assert_eq!(
            text.to_string(),
            indoc! {"
                - ┌──────────┬─────────┐
                  │ Header   │ Value   │
                  ├──────────┼─────────┤
                  │ long     │ more    │
                  │ words    │ content │
                  │ here     │ here    │
                  └──────────┴─────────┘"}
        );
    }

    #[test]
    fn wrapped_table_in_ordered_list() {
        let options = Options::default().table_width(24);

        let text = from_str_with_options(
            indoc! {"
                10. | Header | Value |
                    | --- | --- |
                    | long words here | more lengthy text |
            "},
            &options,
        );

        assert_eq!(
            text.to_string(),
            indoc! {"
                10. ┌─────────┬────────┐
                    │ Header  │ Value  │
                    ├─────────┼────────┤
                    │ long    │ more   │
                    │ words   │ length │
                    │ here    │ y text │
                    └─────────┴────────┘"}
        );
    }

    #[test]
    fn wrapped_table_in_quoted_list() {
        let options = Options::default().table_width(24);

        let text = from_str_with_options(
            indoc! {"
                > - | Header | Value |
                >   | --- | --- |
                >   | long words here | more lengthy text |
            "},
            &options,
        );

        assert_eq!(
            text.to_string(),
            indoc! {"
                > - ┌─────────┬────────┐
                >   │ Header  │ Value  │
                >   ├─────────┼────────┤
                >   │ long    │ more   │
                >   │ words   │ length │
                >   │ here    │ y text │
                >   └─────────┴────────┘"}
        );
    }

    #[rstest]
    #[case::zero(0)]
    #[case::one_column(1)]
    #[case::borders(5)]
    #[case::graphemes(9)]
    fn narrow_table_preserves_content(#[case] width: u16) {
        let options = Options::default().table_width(width);

        let text = from_str_with_options(
            indoc! {"
                | 界 | x |
                | --- | --- |
                | 中文 | abc |
            "},
            &options,
        );

        assert_eq!(
            text.to_string(),
            indoc! {"
                ┌────┬───┐
                │ 界 │ x │
                ├────┼───┤
                │ 中 │ a │
                │ 文 │ b │
                │    │ c │
                └────┴───┘"}
        );
    }

    #[test]
    fn sufficient_width_keeps_natural_column_widths() {
        let options = Options::default().table_width(80);

        let text = from_str_with_options(
            indoc! {"
                | Header | Value |
                | --- | --- |
                | short | x |
            "},
            &options,
        );

        assert_eq!(
            text.to_string(),
            indoc! {"
                ┌────────┬───────┐
                │ Header │ Value │
                ├────────┼───────┤
                │ short  │ x     │
                └────────┴───────┘"}
        );
    }

    #[test]
    fn empty_table() {
        let builder = TableBuilder::new(vec![]);
        assert!(builder.render(&DefaultStyleSheet, None).is_empty());
    }

    #[test]
    fn single_cell() {
        let mut builder = TableBuilder::new(vec![Alignment::None]);
        builder.start_cell();
        builder.push_span(Span::raw("hi"));
        builder.finish_cell();
        builder.finish_header();
        assert_eq!(builder.render(&DefaultStyleSheet, None).len(), 4);
    }

    #[rstest]
    #[case::left(3, Alignment::Left, (0, 7))]
    #[case::right(3, Alignment::Right, (7, 0))]
    #[case::center_even(4, Alignment::Center, (3, 3))]
    #[case::center_odd(3, Alignment::Center, (3, 4))]
    fn padding_for_each_alignment(
        #[case] content_width: usize,
        #[case] alignment: Alignment,
        #[case] expected: (usize, usize),
    ) {
        assert_eq!(padding(10, content_width, alignment), expected);
    }

    #[test]
    fn cell_style_covers_padding_and_empty_cells() {
        let style = Style::new().on_green();
        let cell = TableCell {
            spans: vec![Span::raw("x")],
        };
        insta::assert_debug_snapshot!(cell.render_spans(4, Alignment::Center, style), @r#"
        [
            Span::from("  ").on_green(),
            Span::from("x").on_green(),
            Span::from("   ").on_green(),
        ]
        "#);

        let empty_cell = TableCell::default();
        insta::assert_debug_snapshot!(empty_cell.render_spans(4, Alignment::Right, style), @r#"
        [
            Span::from("     ").on_green(),
            Span::from(" ").on_green(),
        ]
        "#);
    }

    #[test]
    fn column_widths_have_a_minimum_of_one() {
        let mut builder = TableBuilder::new(vec![]);
        builder.header.cells.push(TableCell::default());
        assert_eq!(builder.column_widths(1), vec![1]);
    }

    #[test]
    fn styled_cell_width() {
        let cell = TableCell {
            spans: vec![Span::from("hello").bold(), Span::raw(" world")],
        };
        assert_eq!(cell.width(), 11);
    }

    #[test]
    fn emoji_cell_width() {
        let cell = TableCell {
            spans: vec![Span::raw("✅"), Span::raw(" ok")],
        };
        assert_eq!(cell.width(), 5);
    }

    #[test]
    fn cjk_cell_width() {
        let cell = TableCell {
            spans: vec![Span::raw("日本"), Span::raw(" ok")],
        };
        assert_eq!(cell.width(), 7);
    }

    #[test]
    fn table_with_alignment() {
        let text = from_str(indoc! {"
            | Left | Center | Right |
            |:-----|:------:|------:|
            | a    | b      | c     |
        "});

        assert_eq!(
            text.to_string(),
            indoc! {"
                ┌──────┬────────┬───────┐
                │ Left │ Center │ Right │
                ├──────┼────────┼───────┤
                │ a    │   b    │     c │
                └──────┴────────┴───────┘"}
        );
    }

    #[test]
    fn table_without_outer_pipes() {
        let text = from_str(indoc! {"
            A | B
            ---|---
            a | b
        "});

        assert_eq!(
            text.to_string(),
            indoc! {"
                ┌───┬───┐
                │ A │ B │
                ├───┼───┤
                │ a │ b │
                └───┴───┘"}
        );
    }

    #[test]
    fn escaped_pipe_stays_inside_its_cell() {
        let text = from_str(indoc! {"
            | Value |
            |-------|
            | a \\| b |
        "});

        assert_eq!(
            text.to_string(),
            indoc! {"
                ┌───────┐
                │ Value │
                ├───────┤
                │ a | b │
                └───────┘"}
        );
    }

    #[test]
    fn table_with_cjk_content() {
        let text = from_str(indoc! {"
            | Latin | CJK |
            |-------|-----|
            | a     | 日本 |
        "});

        assert_eq!(
            text.to_string(),
            indoc! {"
                ┌───────┬──────┐
                │ Latin │ CJK  │
                ├───────┼──────┤
                │ a     │ 日本 │
                └───────┴──────┘"}
        );
        assert!(text.lines.iter().all(|line| line.width() == 16));
    }

    #[derive(Clone)]
    struct CustomTableStyleSheet;

    impl StyleSheet for CustomTableStyleSheet {
        fn heading(&self, _level: u8) -> Style {
            Style::default()
        }

        fn code(&self) -> Style {
            Style::default()
        }

        fn link(&self) -> Style {
            Style::new().blue().underlined()
        }

        fn blockquote(&self) -> Style {
            Style::default()
        }

        fn heading_meta(&self) -> Style {
            Style::default()
        }

        fn metadata_block(&self) -> Style {
            Style::default()
        }

        fn table_header(&self) -> Style {
            Style::new().on_blue()
        }

        fn table_cell(&self) -> Style {
            Style::new().red().on_green()
        }

        fn table_border(&self) -> Style {
            Style::new().red()
        }
    }

    #[test]
    fn custom_styles_apply_to_header_cells_body_cells_and_borders() {
        let options = Options::new(CustomTableStyleSheet);

        let text = from_str_with_options(
            indoc! {"
                | A |
                |---|
                | a |
            "},
            &options,
        );
        insta::assert_debug_snapshot!(
            "custom_styles_apply_to_header_cells_body_cells_and_borders",
            text
        );
        insta::assert_snapshot!(text, @"
        ┌───┐
        │ A │
        ├───┤
        │ a │
        └───┘
        ");
    }

    #[test]
    fn custom_cell_style_composes_with_inline_formatting() {
        let options = Options::new(CustomTableStyleSheet);
        let text = from_str_with_options(
            indoc! {"
                | A |
                |---|
                | **bold** |
            "},
            &options,
        );
        insta::assert_debug_snapshot!(text.lines[3], @r#"
        Line::from_iter([
            Span::from("│").red(),
            Span::from(" ").red().on_green(),
            Span::from("bold").red().on_green().bold(),
            Span::from(" ").red().on_green(),
            Span::from("│").red(),
        ])
        "#);
    }

    #[test]
    fn table_cell_style_overrides_conflicting_inline_properties() {
        let options = Options::new(CustomTableStyleSheet);
        let text = from_str_with_options(
            indoc! {"
                | A |
                |---|
                | [docs](url) |
            "},
            &options,
        );

        insta::assert_debug_snapshot!(text.lines[3], @r#"
        Line::from_iter([
            Span::from("│").red(),
            Span::from(" ").red().on_green(),
            Span::from("docs").red().on_green().underlined(),
            Span::from(" (").red().on_green(),
            Span::from("url").red().on_green().underlined(),
            Span::from(")").red().on_green(),
            Span::from(" ").red().on_green(),
            Span::from("│").red(),
        ])
        "#);
    }

    #[test]
    fn table_preserves_surrounding_paragraph_spacing() {
        let text = from_str(indoc! {"
            Before

            | A |
            |---|
            | a |

            After
        "});

        assert_eq!(
            text.to_string(),
            indoc! {"
                Before
                
                ┌───┐
                │ A │
                ├───┤
                │ a │
                └───┘
                
                After"}
        );
    }

    #[test]
    fn consecutive_tables_keep_separate_layout_state() {
        let text = from_str(indoc! {"
            | Long |
            |------|
            | value |

            | A |
            |---|
            | b |
        "});

        assert_eq!(
            text.to_string(),
            indoc! {"
                ┌───────┐
                │ Long  │
                ├───────┤
                │ value │
                └───────┘
                
                ┌───┐
                │ A │
                ├───┤
                │ b │
                └───┘"}
        );
    }

    #[test]
    fn empty_cells_keep_minimum_column_width() {
        let text = from_str(indoc! {"
            | A | B |
            |---|---|
            |   |   |
        "});

        assert_eq!(
            text.to_string(),
            indoc! {"
                ┌───┬───┐
                │ A │ B │
                ├───┼───┤
                │   │   │
                └───┴───┘"}
        );
    }

    #[test]
    fn header_only_table_has_a_complete_frame() {
        let text = from_str(indoc! {"
            | A |
            |---|
        "});

        assert_eq!(
            text.to_string(),
            indoc! {"
                ┌───┐
                │ A │
                ├───┤
                └───┘"}
        );
    }

    #[test]
    fn short_rows_are_padded_and_extra_cells_are_ignored() {
        let text = from_str(indoc! {"
            | A | B |
            |---|---|
            | one |
            | x | y | ignored |
        "});

        assert_eq!(
            text.to_string(),
            indoc! {"
                ┌─────┬───┐
                │ A   │ B │
                ├─────┼───┤
                │ one │   │
                │ x   │ y │
                └─────┴───┘"}
        );
    }

    #[test]
    fn table_with_inline_code() {
        let text = from_str(indoc! {"
            | Name | Type |
            |------|------|
            | foo  | `u32` |
        "});

        insta::assert_debug_snapshot!(text.lines[3], @r#"
        Line::from_iter([
            Span::from("│").dark_gray(),
            Span::from(" "),
            Span::from("foo"),
            Span::from("  "),
            Span::from("│").dark_gray(),
            Span::from(" "),
            Span::from("u32").white().on_black(),
            Span::from("  "),
            Span::from("│").dark_gray(),
        ])
        "#);
    }

    #[test]
    fn table_with_bold_in_cells() {
        let text = from_str(indoc! {"
            | Col |
            |-----|
            | **bold** |
        "});

        insta::assert_debug_snapshot!(text.lines[3], @r#"
        Line::from_iter([
            Span::from("│").dark_gray(),
            Span::from(" "),
            Span::from("bold").bold(),
            Span::from(" "),
            Span::from("│").dark_gray(),
        ])
        "#);
    }

    #[test]
    fn table_keeps_link_destination_in_cell() {
        let text = from_str(indoc! {"
            | Link |
            |------|
            | [docs](u) |
        "});

        insta::assert_debug_snapshot!(text);
        insta::assert_snapshot!(text, @"
        ┌──────────┐
        │ Link     │
        ├──────────┤
        │ docs (u) │
        └──────────┘
        ");
    }

    #[test]
    fn table_keeps_inline_features_in_cell() {
        let text = from_str(indoc! {"
            | Value |
            |-------|
            | <em>x</em> $y$ |
        "});

        insta::assert_debug_snapshot!(text);
        insta::assert_snapshot!(text, @"
        ┌────────────────┐
        │ Value          │
        ├────────────────┤
        │ <em>x</em> $y$ │
        └────────────────┘
        ");
    }

    #[test]
    fn table_routes_inline_content_through_the_active_cell() {
        let text = from_str(indoc! {"
            | Value |
            |-------|
            | **bold** `code` [link](url) <em>x</em> $y$ [^n] |

            [^n]: note
        "});

        insta::assert_debug_snapshot!(text);
        insta::assert_snapshot!(text, @"
        ┌─────────────────────────────────────────┐
        │ Value                                   │
        ├─────────────────────────────────────────┤
        │ bold code link (url) <em>x</em> $y$ [n] │
        └─────────────────────────────────────────┘

        [n]: note
        ");
    }

    #[test]
    fn block_markers_inside_cells_remain_inline_text() {
        let text = from_str(indoc! {"
            | Value |
            |-------|
            | # heading |
            | > quote |
            | - list |
        "});

        assert_eq!(
            text.to_string(),
            indoc! {"
                ┌───────────┐
                │ Value     │
                ├───────────┤
                │ # heading │
                │ > quote   │
                │ - list    │
                └───────────┘"}
        );
    }

    #[test]
    fn table_in_blockquote_keeps_quote_prefix() {
        let text = from_str(indoc! {"
            > | A |
            > |---|
            > | a |
        "});

        assert_eq!(
            text.to_string(),
            indoc! {"
                > ┌───┐
                > │ A │
                > ├───┤
                > │ a │
                > └───┘"}
        );
    }

    #[test]
    fn table_list_item_keeps_marker_and_continuation_indent() {
        let text = from_str(indoc! {"
            - | A |
              |---|
              | a |
        "});

        assert_eq!(
            text.to_string(),
            indoc! {"
                - ┌───┐
                  │ A │
                  ├───┤
                  │ a │
                  └───┘"}
        );
    }

    #[test]
    fn later_table_in_list_uses_continuation_indent() {
        let text = from_str(indoc! {"
            - | A |
              |---|
              | a |

              | B |
              |---|
              | b |
        "});

        assert_eq!(
            text.to_string(),
            indoc! {"
                - ┌───┐
                  │ A │
                  ├───┤
                  │ a │
                  └───┘
                
                  ┌───┐
                  │ B │
                  ├───┤
                  │ b │
                  └───┘"}
        );
    }

    #[test]
    fn ordered_table_list_item_uses_full_marker_width() {
        let text = from_str(indoc! {"
            10. | A |
                |---|
                | a |
        "});

        assert_eq!(
            text.to_string(),
            indoc! {"
                10. ┌───┐
                    │ A │
                    ├───┤
                    │ a │
                    └───┘"}
        );
    }

    #[test]
    fn nested_table_list_item_uses_nested_marker_width() {
        let text = from_str(indoc! {"
            - Parent
              - | A |
                |---|
                | a |
        "});

        assert_eq!(
            text.to_string(),
            indoc! {"
                - Parent
                    - ┌───┐
                      │ A │
                      ├───┤
                      │ a │
                      └───┘"}
        );
    }

    #[test]
    fn table_snapshot() {
        insta::assert_snapshot!(from_str(indoc!("
            | Name | Value |
            |------|-------|
            | foo  | bar   |
            | baz  | qux   |
        ")), @"
        ┌──────┬───────┐
        │ Name │ Value │
        ├──────┼───────┤
        │ foo  │ bar   │
        │ baz  │ qux   │
        └──────┴───────┘
        ");
    }
}
