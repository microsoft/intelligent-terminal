# RTL Support — Design Notes

**Status:** Implemented.

This document explains the right-to-left (RTL) layout work that
accompanies the localization effort, so that future contributors can
see *why* the implementation is shaped the way it is. The code lives
in `src/cascadia/inc/RtlHelper.h` (C++ / FRE) and `tools/wta/src/rtl.rs`
(Rust / `wta`); this note is the design background.

## Why this exists

The localization pass translated text for several right-to-left
scripts. Translated text is only half of RTL support — the *layout*
also needs to mirror (right-to-left flow, right-aligned columns,
primary buttons swap sides, etc.).

## RTL coverage

Every locale the OS classifies as right-to-left is supported — the
helpers delegate to the Windows locale database, so the set is the
OS's, not ours. For end-to-end validation we use the pseudo-mirrored
pseudo-locale `qps-plocm`, which Microsoft ships specifically so apps
can verify RTL / mirrored layouts without a real RTL build.

## Implementation overview

The product spans two stacks with very different layout primitives, so
each gets its own one-line wrapper around the OS locale database.

### FRE (C++ XAML, `src/cascadia/TerminalApp/FreOverlay.xaml`)

- XAML inherits `FlowDirection` down the visual tree and auto-mirrors
  `HorizontalAlignment`. One assignment at the root flips the whole
  two-page wizard, including the explicit `HorizontalAlignment="Right"`
  on the Next / Save buttons (XAML auto-flips those when the parent's
  `FlowDirection` is `RightToLeft`).
- `FreOverlay::Initialize` reads the resolved UI language
  (`globals.Language()` first, then `ApplicationLanguages::Languages()`)
  and sets `RootGrid().FlowDirection(...)` on both branches —
  `RightToLeft` for RTL languages, `LeftToRight` otherwise. Setting
  both branches explicitly matters because the FRE element is reused
  across shows; without an explicit LTR assignment, an RTL session
  could leak its mirrored layout into a subsequent LTR show.
- Classification itself is delegated to `GetLocaleInfoEx` with the
  Win32 reading-layout locale-info field — the same OS API the Rust
  side uses, so we have one cross-stack source of truth.

### `wta` TUI (Rust)

- The Rust TUI library has no native bidi engine; it draws monospaced
  cells left-to-right. The terminal emulator that hosts `wta` —
  Windows Terminal since 1.21 / preview — performs the actual character
  shaping. So the only useful `wta`-side action is right-aligning
  `Paragraph` widgets that render translated copy.
- `tools/wta/src/rtl.rs` exposes `is_rtl_locale(&str)` and
  `text_alignment()`. Classification delegates to the Win32
  reading-layout locale-info API via `windows-sys` (the
  `Win32_Globalization` feature) — the same OS classifier the C++ side
  uses, so both stacks agree by construction.
- Applied to the user-facing prose `Paragraph`s in `setup.rs`,
  `auth.rs`, `permission.rs` (both the full card and the compact
  fallback), `recommendations.rs` (nav hint), `chat.rs`, and the
  agents view loading / footer paragraphs in `agents_view.rs`.
- Status / token rows (input box, debug panel) are intentionally not
  flipped — those are visually anchored chrome where mirroring just
  confuses non-RTL readers of mixed-language UIs.

## Validation

Set `"language": "qps-plocm"` in `settings.json`:

- FRE: the wizard mirrors. Title centered, Next / Save buttons swap to
  the left, ComboBoxes drop down on the opposite side.
- Agent pane: setup / auth / permission / recommendations / chat
  prose right-aligns.

Non-RTL locales explicitly receive `FlowDirection::LeftToRight` on the
FRE root grid (we set both branches so the cascade always reflects the
current locale, not whatever was set on the previous show). The OS
returns LTR for the vast majority of locales, so the visible behavior
is unchanged for them.

## What was intentionally skipped

The original investigation considered pulling a Unicode bidirectional
text crate for in-string reordering. We did not — Windows Terminal
performs the bidi shaping on rendered cells, and reordering cells in
`wta` would duplicate that work and risk disagreement with the host
terminal.

## File markers

This document lives at `doc/specs/rtl-investigation.md` as a design
record alongside the live code. See the linked code paths above for
the live behavior.
