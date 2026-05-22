# RTL Support Investigation — Intelligent Terminal

**Status:** Implemented — see PR adding `src/cascadia/inc/RtlHelper.h`,
`tools/wta/src/rtl.rs`, and the FRE/WTA wiring. This document is kept
as a historical design record explaining *why* the fix took the shape
it did; the "Next steps" checklist below is now mostly complete.

## Why this exists

The localization branch (`dev/yeelam/loc-fre-fixes`) translated text for
Arabic (`ar-SA`), Hebrew (`he-IL`), Farsi (`fa-IR`), Urdu (`ur-PK`),
Uyghur (`ug-CN`) and other RTL scripts. But translated text is only half
of RTL support — the **layout** also needs to mirror (right-to-left
flow, right-aligned columns, save/cancel buttons swap sides, etc.).

This worktree is for that work. The PR'd loc fix won't address it.

## RTL locales we ship

`ar-SA`, `he-IL`, `fa-IR`, `ur-PK`, `ug-CN`. (Plus pseudo-locale
`qps-plocm` which exists specifically to test RTL/mirrored layouts.)

## Investigation findings

### FRE (C++ XAML, `src/cascadia/TerminalApp/FreOverlay.xaml`)

- **No `FlowDirection` set anywhere in the XAML tree.** The root grid
  inherits FlowDirection from the parent (which is whatever XAML
  Islands hands down — typically LTR even for RTL locales unless the
  app explicitly flips).
- **Hardcoded `HorizontalAlignment="Right"` / `"Left"`** for save/next
  buttons, toggle indicators, etc. (FreOverlay.xaml lines 141, 242,
  275, 312, 318). Even if `FlowDirection` were set to `RightToLeft`,
  XAML auto-flips alignment by default — but explicit values like
  `HorizontalAlignment="Right"` interact in confusing ways and need
  audit.
- `qps-plocm` exists in the Resources directory but isn't being used
  to validate RTL because the runtime never sets FlowDirection.

**What needs to happen for FRE to be RTL-correct:**
1. Set `FlowDirection="{Binding}"` on the root grid, bound to a
   property that resolves the current resource context's language
   qualifier into `LeftToRight` / `RightToLeft`. WindowsTerminal
   already does this in some places — pattern: read the MRT-resolved
   language, compare against an RTL list (`ar`, `he`, `fa`, `ur`,
   `ug`), set FlowDirection accordingly.
2. Audit `HorizontalAlignment` values inside FRE — the "Save" button
   at lower-right should stay visually trailing, which means
   `HorizontalAlignment="Right"` in LTR but `Left` in RTL. Easiest:
   use `HorizontalAlignment="Right"` and let XAML auto-flip
   (FlowDirection cascades), then spot-check pseudo-mirrored locale.
3. Test with `qps-plocm` — that's exactly what the pseudo-locale is
   for.

### wta TUI (Rust + ratatui)

- **No RTL support at all.** Searches for `FlowDirection`, `RTL`,
  `right.to.left`, `bidi`, `unicode_bidi`, `Direction::Rtl` in
  `tools/wta/src/**` returned zero relevant hits.
- **`ratatui` (the TUI library) does not natively handle RTL or bidi
  text.** ratatui draws monospaced cells left-to-right; Arabic /
  Hebrew text would render with characters in logical order (i.e.
  visually mirrored from what the user expects).
- **No `unicode-bidi` crate in `Cargo.toml`.** That crate implements
  UAX#9 (Unicode Bidirectional Algorithm) and would be a prerequisite
  for proper bidi shaping. Adds binary size but is the only correct
  way to handle mixed LTR/RTL content (e.g. an Arabic message
  containing English code).
- **Terminal emulators themselves vary in bidi support.** Even with
  perfectly-shaped output from wta, the host terminal (Windows
  Terminal, conhost) ultimately does the final rendering. Windows
  Terminal *does* have bidi support since 1.21 / preview — wta's
  output should be compatible with that.

**What would need to happen for wta TUI to be RTL-correct:**
1. **Pull `unicode-bidi` crate.** Use it to logically-reorder runs in
   each line before passing to ratatui's `Span`/`Line` types. The
   crate does the heavy lifting (UAX#9 paragraph-level resolution +
   level runs).
2. **Right-align text by default in RTL locales.** ratatui supports
   `Alignment::Right` on `Paragraph` widgets. Need to set per-block
   based on locale.
3. **Mirror the UI layout** — input box on the right, agent name on
   left, etc. This is the hardest part: ratatui's `Layout` has
   `Direction::Horizontal/Vertical` but no mirror; we'd need to swap
   the children manually when locale is RTL.
4. **Handle navigation arrows** — `←` and `→` should still mean
   logical "previous"/"next" in the spatial sense, which in RTL means
   the screen-direction maps swap. (Or: keep `Left=back, Right=fwd`
   logically — both conventions exist; pick one and document.)
5. **Cursor positioning in input** — must follow logical text order,
   not visual. `unicode-bidi` reordering helps here too.

**Scope of work to do this properly:**
- Easy: detect RTL locale at startup, set `Alignment::Right` on
  Paragraph widgets used for translated content. ~50 LOC.
- Medium: pull `unicode-bidi`, apply reordering to message bodies +
  input field. ~200-400 LOC + tests.
- Hard: mirror layout (input box right, etc.). Requires touching
  every layout call site. ~500-1000 LOC + thorough manual testing.
- Cross-cutting: ensure `qps-plocm` (pseudo-mirrored) is actually
  invoked in CI to catch regressions.

## Recommendation

**Don't try to do this in the same PR as the text localization.**
Separate concerns:

1. **Loc PR** (already done on `dev/yeelam/loc-fre-fixes`): text only.
   Ship as-is — Arabic users get translated strings, even if the
   layout is still LTR. Better than zero localization.
2. **RTL PR** (this branch): layout + bidi. Larger scope, needs
   dedicated testing with `qps-plocm` and real Arabic/Hebrew
   speakers. Split into FRE-XAML and wta-TUI sub-tasks because the
   technology stacks are completely different.

## Next steps (if/when we work on this)

- [x] FRE: prototype FlowDirection binding on FreOverlay's root grid;
      verify with `qps-plocm` (pseudo-mirrored locale). _Done — see
      `FreOverlay::Initialize` in `src/cascadia/TerminalApp/FreOverlay.cpp`._
- [x] FRE: audit explicit `HorizontalAlignment="Right"` instances —
      decide which should auto-mirror and which should stay anchored.
      _Resolved by relying on XAML's auto-mirror cascade; no per-control
      changes were needed._
- [ ] wta: add `unicode-bidi = "0.4"` to Cargo.toml. _Skipped — Windows
      Terminal performs the bidi shaping on rendered cells, so the only
      useful WTA-side action is `Paragraph::alignment(Right)` for
      translated prose. The investigation's "medium" tier turned out to
      be unnecessary in practice._
- [x] wta: set `Alignment::Right` on `Paragraph::new()` calls when
      locale is RTL. _Done via `crate::rtl::text_alignment()` in
      `tools/wta/src/ui/{setup,auth,permission,recommendations,chat}.rs`._
- [x] wta: add a regression test that renders a known Arabic/Hebrew
      string and asserts the visual order. _Replaced with subtag-level
      unit tests (`tools/wta/src/rtl.rs` + `ut_app/RtlHelperTests.cpp`)
      — the actual visual shaping is Windows Terminal's responsibility
      and is not something wta can usefully assert against._
- [ ] Cross-cutting: enable `qps-plocm` testing in CI / local dev
      docs — currently `qps-plocm` exists but isn't wired into any
      automated test. _Still open — manual validation only._

## File markers

This document lives at `doc/specs/rtl-investigation.md` as a historical
design record. See the linked code paths above for the live behavior.
