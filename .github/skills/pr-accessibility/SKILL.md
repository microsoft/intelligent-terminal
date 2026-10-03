---
name: pr-accessibility
description: 'Review and repair native Windows WinUI, XAML, C++, UI Automation, keyboard, focus, screen-reader, high-contrast, scaling, settings, tab, pane, and agent-card accessibility changes. Use for accessibility analysis of Intelligent Terminal UI pull requests and focused local accessibility checks.'
---

# Native Windows Accessibility Review

Use this procedure for native WinUI/XAML/C++ accessibility review. The caller
owns PR metadata, immutable revisions, trust boundaries, permitted edits,
publication, and safe-output policy. This skill owns domain analysis,
repository-specific evidence gathering, and focused validation.

## Mechanical evidence

Use [`scripts/accessibility_review.py`](./scripts/accessibility_review.py) for
deterministic preparation and final-report validation. It classifies changed
native UI files, emits stable source signals, records unavailable runtime
checks, and enforces the high-only repair contract. Treat its findings as
evidence leads, not complete accessibility conclusions.

The source rules are intentionally conservative:

- `AXSTATIC001`: a changed interactive control explicitly placed in the raw UIA
  view. This is HIGH impact but medium confidence until peer, style, and intent
  are inspected, so it is not automatically repairable.
- `AXSTATIC002`: an apparently icon-only changed control with no local name
  source. Treat as MEDIUM needs-review evidence. Content, `x:Uid`, bindings,
  styles, labels, and peers can provide a name.
- `AXSTATIC003`: a changed literal accessible name. Treat as MEDIUM,
  high-confidence localization evidence, not an accessibility auto-fix.

## Review procedure

1. Read the caller-provided prepared report and immutable base/head identifiers.
   Read the exact changed hunks, not only summaries or changed-line snippets.
2. Inspect companion XAML, C++ implementation/header, styles/templates,
   `x:Uid` source resources, custom AutomationPeers, and focused UIA tests needed
   to understand the changed operation.
3. Determine accessible name, role/control type, value/state, label,
   description/help text, and required UIA patterns. Do not report a missing
   literal `AutomationProperties.Name` by itself:
   - standard controls can derive names from content or headers;
   - `x:Uid`, bindings, styles/templates, `LabeledBy`, and peers can supply
     accessible data;
   - decorative elements can intentionally be absent from control/content view.
4. Trace keyboard-only activation and traversal. Check tab and arrow-key order,
   access/accelerator keys, dialog/flyout entry and exit, focus restoration,
   traps, stashed/restored panes, and custom key handlers.
5. Trace screen-reader state changes. Check `LiveSetting`, UIA notification
   events, meaningful payloads, duplicate/noisy announcements, and whether
   disabled, unavailable, loading, and busy states are exposed.
6. Inspect visual accessibility: high-contrast and theme resources, color-only
   communication, text wrapping/trimming, fixed dimensions, 200% text/display
   scaling, clipping, reflow, and scroll reachability.
   - When a change replaces local control properties with a shared style,
     compare every removed alignment, size, wrapping, and focus-related
     property with the style's actual setters and the framework default.
     Do not assume the style preserves omitted behavior.
   - In Grid layouts, account for the default `Stretch` alignment and rows
     whose height is driven by wrapped sibling text. A picker or button that
     previously used centered/natural sizing can otherwise expand to the
     sibling's full height.
7. Apply the same review to tabs, panes, settings controls, titlebar controls,
   and agent cards. Preserve repository behavior and custom peer semantics.
8. Classify each finding with severity independent from confidence:
   - **HIGH**: a significant operation is inaccessible, with direct
     repository-specific evidence.
   - **MEDIUM/LOW**: advice or a risk requiring runtime confirmation.
   Unsafe or uncertain HIGH findings remain `remaining` or `blocked`.
9. Repair only a high-confidence HIGH defect when the caller exposes a trusted
   recipe for it. The only current recipe is
   `AXSTATIC001-remove-raw-view`: remove exactly the raw-view property from the
   matching prepared standard interactive control, which must already have a
   name source. Make no other hunk or file change. Set `repair_recipe` to that
   value and leave `validation` empty for the trusted validator to attest.
10. Keep runtime-dependent fixes `remaining` or `blocked`. The agent cannot
    authenticate its own command/result JSON, and the Ubuntu workflow cannot
    validate UIA patterns, focus, announcements, rendering, or scaling.
11. Re-read the complete final diff. Never repair MEDIUM/LOW findings merely to
    reduce the report, and never add unrelated or untracked files.

## Localization

Accessible names and descriptions are customer-facing. Reuse existing localized
resources or bindings. Never add hard-coded English accessibility text.

When a repair needs a new or changed string, leave a concrete blocking finding
for the localization workflow. Do not edit `.resw` or locale YAML from this
skill.

## Axe.Windows and runtime checks

[Axe.Windows](https://github.com/microsoft/axe-windows) is a native Windows UI
Automation scanner. It evaluates runtime UIA hierarchy, properties, patterns,
values, and children against independent Error, Warning, and NeedsReview rules.
It is not a XAML/C++ source linter.

Use Axe.Windows only with a built application on Windows and an interactive
desktop. Navigate to every affected state, scan the exact process/window, and
retain `.a11ytest` output. Pair it with:

- keyboard-only traversal, activation, focus restoration, and trap checks;
- UIA pattern invocation and property/state assertions;
- Narrator observation of names and announcements;
- high-contrast/theme and color-use inspection;
- 200% text size/display scale, clipping, wrapping, and reflow checks.

Axe.Windows does not by itself prove keyboard journeys, focus restoration,
spoken announcement quality, rendered contrast, or scaling/clipping. When
Windows, the built app, Axe.Windows, or an interactive desktop is unavailable,
record the check as `SKIPPED` or `BLOCKED`, never `PASS`. Browser axe-core and
Playwright accessibility snapshots are not native WinUI UIA coverage.

## Repository test path

Prefer a focused test in `src/cascadia/WindowsTerminal_UIATests` for UI state,
names, control types, patterns, and keyboard focus. Existing lower-level UIA
provider behavior also lives under `src/host/ft_uia`.

Do not add Axe.Windows or another dependency unless the caller permits a
dependency change. Without that dependency and an interactive Windows lane,
document the exact native gap rather than substituting a Linux source check.

## Findings

Use the caller's machine-readable schema. Every finding must include a stable
ID, severity, confidence, source/head SHA, file/line, observed behavior,
expected behavior, user impact, evidence, proposed fix, validation, and
disposition. Preserve prepared deterministic IDs when they apply. Derive new IDs
from rule, normalized path, line, and normalized evidence.

Emit only actual findings. Do not serialize false positives as findings and do
not invent severities or dispositions outside the caller's enum. Put only
actually executed checks in `validation`, using the caller's required object
shape; describe unavailable checks in `runtime_checks`, never as validation
strings. For a proposed trusted static repair, leave `validation` empty. The
native gate rejects model-authored PASS claims, verifies the complete candidate
against its recipe, and adds the trusted attestation itself.

Keep runtime prerequisites separate from findings. Do not turn unavailable
native evidence into success. Make reruns idempotent and avoid duplicate
comments.

## Validation

```powershell
python .github\skills\pr-accessibility\tests\test_accessibility_review.py
gh aw compile ghaw-pr-accessibility --no-check-update
gh aw validate ghaw-pr-accessibility --no-check-update
```
