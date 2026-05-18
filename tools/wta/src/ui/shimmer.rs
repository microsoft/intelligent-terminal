//! Reusable "shimmer sweep" animation for inline status labels.
//!
//! Soft white highlight sweeps left→right across a label, matching the
//! `linear-gradient(90deg, …) + background-position` animation used by the
//! web chat UI. The web version runs at 1.6s/cycle on a 60FPS canvas; we run
//! on the TUI Tick (120ms ≈ 8FPS). At 18 ticks across the padded span each
//! frame moves under one cell so the band reads as "sweeping" rather than
//! "stepping", and the cycle finishes in ~2.16s.
//!
//! Callers own the frame counter — typically advanced once per Tick — and
//! pass it plus the label text to [`shimmer_spans`].

use ratatui::prelude::*;

/// Number of frames per full sweep. Cycle counters should be taken mod this.
pub const CYCLE_FRAMES: usize = 18;

/// Padding on both sides lets the highlight enter from off-screen-right and
/// exit off-screen-left instead of clamping at the label edges.
const PAD: f32 = 3.0;
/// Half-width of the cosine falloff, in cells. ≥2σ from the center → fully dim.
const SIGMA: f32 = 1.8;
/// White composited on the default dark Terminal background at ~25% / ~85%
/// opacity — matches the CSS gradient's two stops. (Terminal cells have no
/// real alpha, so the values are pre-multiplied against an assumed dark bg.)
const DIM_RGB: (u8, u8, u8) = (64, 64, 64);
const BRIGHT_RGB: (u8, u8, u8) = (217, 217, 217);

/// Render `text` as a list of per-character `Span`s with the shimmer
/// highlight at position `frame` (taken mod [`CYCLE_FRAMES`]).
///
/// Each character becomes its own `Span` because the highlight is a
/// cell-granularity gradient — we can't express it with a single `Style`.
pub fn shimmer_spans(text: &str, frame: usize) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len() as f32;
    let span = n + 2.0 * PAD;
    let phase = (frame % CYCLE_FRAMES) as f32 / CYCLE_FRAMES as f32;
    // Center starts at -PAD (off the left edge) and walks to (n + PAD) at
    // phase=1 — left→right across the padded span.
    let center = -PAD + phase * span;

    chars
        .into_iter()
        .enumerate()
        .map(|(i, ch)| {
            let d = (i as f32 + 0.5) - center;
            let w = if d.abs() >= 2.0 * SIGMA {
                0.0
            } else {
                0.5 * (1.0 + (std::f32::consts::PI * d / (2.0 * SIGMA)).cos())
            };
            let r = lerp_u8(DIM_RGB.0, BRIGHT_RGB.0, w);
            let g = lerp_u8(DIM_RGB.1, BRIGHT_RGB.1, w);
            let b = lerp_u8(DIM_RGB.2, BRIGHT_RGB.2, w);
            Span::styled(ch.to_string(), Style::new().fg(Color::Rgb(r, g, b)))
        })
        .collect()
}

fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    let t = t.clamp(0.0, 1.0);
    (a as f32 + (b as f32 - a as f32) * t).round() as u8
}
