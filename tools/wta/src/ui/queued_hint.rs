//! One-row "Queued (N): preview" indicator rendered directly above the input
//! box whenever the current tab has pending prompts. See `app_queue.rs` and
//! the Enter / Esc handlers for the producer side.

use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::theme;

/// Whether the current tab has queued work worth a dedicated row. The layout
/// planner owns the final decision, so a short pane can still drop the row.
pub(crate) fn queue_hint_requested(app: &App) -> bool {
    !app.current_tab().pending_prompts.is_empty()
}

/// Two-cell leading-edge indent matching other transient hints. In RTL
/// locales the leading edge is the right, so the padding follows the text.
const HORIZONTAL_PADDING: u16 = 2;

pub fn render(frame: &mut Frame, app: &App, area: Rect) {
    let tab = app.current_tab();
    if tab.pending_prompts.is_empty() || area.width == 0 || area.height == 0 {
        return;
    }
    let count = tab.pending_prompts.len();
    // The next Esc would pop the BACK of the deque (LIFO undo), so the
    // preview shows the most-recently queued prompt to match what Esc
    // affects. FIFO dispatch order (next to send is the front) is conveyed
    // by the count alone — the user sees the queue shrink as the agent
    // works through it.
    //
    // Use the raw collapsed text (no char-truncation) and let the single
    // width-aware truncation pass below own all clipping. This avoids
    // double-ellipsis artefacts when both `QueuedPrompt::preview` and
    // `truncate_to_width` would clip the same string.
    let preview = tab
        .pending_prompts
        .back()
        .map(|p| p.collapsed_text())
        .unwrap_or_default();
    let text = t!("input.queue.indicator", count = count, preview = preview).into_owned();
    // The render line below adds up to `HORIZONTAL_PADDING` cells at the
    // locale's leading edge, so the truncation budget is `area.width - padding`. In
    // very narrow panes (`area.width < HORIZONTAL_PADDING + 1`) we'd
    // otherwise pass 0 to `truncate_to_width` and get an empty string,
    // making the indicator a row of pure padding. Floor the budget at 1
    // whenever there's at least 1 cell available so the truncation marker
    // is always visible — `truncate_to_width(_, 1)` emits a bare `…`.
    let budget = (area.width as usize)
        .saturating_sub(HORIZONTAL_PADDING as usize)
        .max(1);
    let truncated = truncate_to_width(&text, budget);
    // Clamp the leading-edge padding to whatever room is left after the
    // truncated body — in very narrow terminals padding shrinks to 0 so the
    // marker stays visible.
    let padding_width = (area.width as usize)
        .saturating_sub(unicode_width::UnicodeWidthStr::width(truncated.as_str()));
    let text = pad_indicator(
        &truncated,
        padding_width.min(HORIZONTAL_PADDING as usize),
        crate::rtl::is_current_locale_rtl(),
    );
    let line = Line::from(Span::styled(text, theme::DIM));
    frame.render_widget(
        Paragraph::new(line).alignment(crate::rtl::text_alignment()),
        area,
    );
}

fn pad_indicator(text: &str, padding_width: usize, rtl: bool) -> String {
    let padding = " ".repeat(padding_width);
    if rtl {
        format!("{text}{padding}")
    } else {
        format!("{padding}{text}")
    }
}

fn truncate_to_width(text: &str, max_cells: usize) -> String {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
    if max_cells == 0 {
        return String::new();
    }
    // Fast path: fits as-is. `UnicodeWidthStr::width` counts zero-width
    // chars as 0 cells (combining marks layered onto a base char don't add
    // visible columns), so a string of `a\u{0301}b` reports width 2 here.
    // That matches the actual rendered width, so the fast path is sound.
    if UnicodeWidthStr::width(text) <= max_cells {
        return text.to_string();
    }
    // Reserve 1 cell up-front for the ellipsis so the displayed width
    // is provably ≤ max_cells without a post-trim that could chew off
    // the marker. When max_cells == 1, content_budget is 0 → we emit a
    // bare `…` (preferred over a blank row).
    let content_budget = max_cells.saturating_sub(1);
    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        // A zero-width char that follows a base char already in the output
        // costs nothing to render, so keeping it preserves the grapheme
        // (`a` + U+0301 stays `á`). A leading zero-width char has no base to
        // attach to, so charge it a cell rather than letting an unbounded run
        // of them slip past the budget.
        let width = UnicodeWidthChar::width(ch).unwrap_or(0);
        let w = if width == 0 && !out.is_empty() {
            0
        } else {
            width.max(1)
        };
        if used + w > content_budget {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::{pad_indicator, truncate_to_width};

    #[test]
    fn truncate_under_width_keeps_string() {
        assert_eq!(truncate_to_width("hello", 10), "hello");
    }

    #[test]
    fn truncate_over_width_inserts_ellipsis() {
        let out = truncate_to_width("abcdefghij", 5);
        // `truncate_to_width` reserves 1 cell for the ellipsis, so at most
        // 4 chars are emitted before `…` is appended.
        assert!(out.ends_with('…'), "got: {out}");
        assert!(out.chars().count() <= 5);
        assert_eq!(
            unicode_width::UnicodeWidthStr::width(out.as_str()),
            5,
            "result must fill exactly the requested width when content overflows"
        );
    }

    #[test]
    fn truncate_zero_width_returns_empty() {
        assert_eq!(truncate_to_width("anything", 0), "");
    }

    #[test]
    fn truncate_wide_char_with_narrow_budget_emits_ellipsis() {
        // CJK full-width glyph is 2 cells; with max_cells=1, we can't fit it
        // but we still want a visible truncation marker rather than a blank
        // row. Regression for the Copilot-review edge case.
        let out = truncate_to_width("中文", 1);
        assert_eq!(out, "…", "got: {out:?}");
    }

    #[test]
    fn truncate_respects_width_with_combining_marks() {
        // Zero-width combining marks can't push the visible width past
        // `max_cells`. Regression for the Copilot round-3 review case:
        // `a\u{0301}b` with `max_cells=1` previously emitted `a…` which
        // displays as 2 cells.
        //
        // Fixture: one grapheme `á` (a + combining acute, U+0301) followed
        // by a few trailing chars to force the truncate loop past the
        // budget. The trailing chars are deliberately digits — their
        // identity doesn't matter, only that there's *something* after the
        // base grapheme. Using digits avoids the spell checker flagging an
        // arbitrary letter run as an unrecognized word.
        let out = truncate_to_width("a\u{0301}1234", 1);
        let width = unicode_width::UnicodeWidthStr::width(out.as_str());
        assert!(
            width <= 1,
            "truncated string {out:?} has display width {width}, expected ≤ 1"
        );
        assert!(
            out.contains('…'),
            "must keep the ellipsis marker; got {out:?}"
        );
    }

    #[test]
    fn truncate_wide_char_with_two_cell_budget_emits_ellipsis_only() {
        // Two cells is enough for either the wide char OR the ellipsis,
        // but not both. We choose the truncation marker over a partial
        // word so the user knows there is hidden content.
        let out = truncate_to_width("中文abc", 2);
        let width = unicode_width::UnicodeWidthStr::width(out.as_str());
        assert!(width <= 2, "got {out:?} width {width}");
        assert!(out.contains('…'));
    }

    #[test]
    fn truncate_keeps_a_combining_mark_with_its_base_character() {
        // The accent adds no display cells, so clipping must not strip it
        // from the base character it belongs to and leave a bare `a`.
        let out = truncate_to_width("a\u{0301}b1234", 3);
        assert_eq!(out, "a\u{0301}b…");
        assert_eq!(unicode_width::UnicodeWidthStr::width(out.as_str()), 3);
    }

    #[test]
    fn rtl_indicator_padding_uses_the_right_leading_edge() {
        assert_eq!(pad_indicator("Queued", 2, false), "  Queued");
        assert_eq!(pad_indicator("בתור", 2, true), "בתור  ");
    }
}
