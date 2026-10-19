---
name: review-globalization
description: 'Review customer-facing Windows Terminal and WTA changes for RTL, Unicode, locale formatting, invariant protocol data, and localizable message construction. Use for globalization PR review or repair.'
---

# Review Globalization

Review functional behavior across locales and scripts. This is not translation
repair. Use `.github/skills/ensure-localization/SKILL.md` only for its
deterministic RESW/YAML invariants when resources are in scope.

## Establish reachability and architecture

Read the exact immutable base-to-head diff. For each changed value or operation,
trace whether it reaches customer-facing UI, terminal protocol semantics,
persistence/wire data, or only internal diagnostics. Inspect unchanged callers,
helpers, and tests before reporting a finding.

Apply the matching checks:

The review vocabulary is deliberate: check directional icons, mixed paths,
UTF-16 surrogate pairs, UTF-8 boundaries, emoji/CJK width, invariant persistence,
placeholder reordering, and concatenated sentence fragments.

| Surface | Checks |
| --- | --- |
| C++/WinRT/XAML | `FlowDirection` inheritance, RTL layout, directional icon mirroring exceptions, alignment/order, keyboard/focus order, expansion/clipping, UTF-16 boundaries, and display formatting |
| RESW | Complete messages, placeholders, reordering, pluralization where supported, ownership, translator context, and expansion; use the localization skill for file invariants |
| Rust UI/localization YAML | UTF-8 and grapheme boundaries, ratatui width/alignment, mixed-direction content, message construction, and localization placeholders |
| Settings, ACP, COM, VT | Invariant parsing/serialization and exact machine tokens; localize only data proven to be human display |
| Buffer/parser/renderer | Surrogates, graphemes/combining sequences, emoji/CJK cell width, truncation/indexing, and intentional terminal protocol behavior |

Debug logs, telemetry, identifiers, command construction, tests, and
machine-readable tokens are not automatically customer-facing. Do not propose
localization or culture-sensitive operations without proving a UI sink.

## Review questions

### RTL and bidirectional text

- Does root `FlowDirection` reach the intended visual tree without incorrectly
  mirroring semantic icons, terminal content, paths, or code?
- Do visual order, keyboard navigation, focus order, alignment, and hit targets
  remain coherent?
- Are mixed RTL/LTR values isolated by the UI/rendering layer rather than
  mutating stored logical text?

### Unicode

- In UTF-16 code, can indexing or truncation split a surrogate pair?
- In UTF-8 code, can byte indexing split a scalar value?
- Can truncation, cursor movement, selection, or width logic split a grapheme
  or combining sequence?
- Does UI width account for emoji/CJK display while preserving the terminal
  buffer/parser's intentional cell and protocol semantics?
- Is casing/comparison appropriate for human language versus identifiers,
  paths, keys, and protocols?

### Locale and round trips

- Use locale-sensitive dates, numbers, sorting, casing, and parsing for human
  display only.
- Keep settings JSON, ACP/COM/VT tokens, timestamps, IDs, command arguments,
  and other wire/persistence values invariant and round-trippable.
- Verify both display and persistence paths; a localized display must not feed
  back into invariant parsing.

### Messages

- Avoid concatenated sentence fragments when translators need to reorder
  grammar or placeholders. Do not blanket-replace safe token/path assembly.
- Preserve placeholder identity/count and allow reordering.
- Use pluralization mechanisms where the stack supports them.
- Put strings in the resource owner that renders them and provide translator
  context for ambiguous meaning or placeholder roles.
- Check pseudo-localized expansion and clipping.

## Evidence and severity

Every finding needs a stable `GLOB-*` ID, exact immutable base/head SHAs,
file/line, reachable scenario, affected locale/script, observed and expected
behavior, user impact, repository-specific evidence, bounded proposed fix,
specific validation, confidence, and disposition.

- **HIGH:** reachable corruption, data loss, unusable navigation/layout, broken
  persistence/wire round trip, or customer-facing crash.
- **MEDIUM:** clear degraded behavior with bounded impact.
- **LOW:** concrete resilience or test gap.

Severity is not confidence. Automatic repair is eligible only for HIGH +
strong confidence + a small behavior-preserving patch + successful final-patch
validation. Unsafe HIGH findings remain blocked. MEDIUM/LOW are suggestions.

## Validation fixtures

Use fixtures that expose the behavior:

- RTL layout: `qps-plocm`
- Mixed direction: `אבג C:\src\报告.txt`
- UTF-16/grapheme: `A😀é`
- Width: emoji plus narrow/wide CJK cells
- Locale round trip: display `1,5`, persist/parse invariant `1.5`
- Placeholder reorder: source `{0} opened {1}`, target order `{1} ... {0}`
- False positive: the same literal in a debug log and an ACP/COM/VT token

For WTA tests that call `rust_i18n::set_locale` or assert localized UI, hold
`crate::test_support::lock_locale()` for the full assertion scope.

For changed RESW or localization YAML, run applicable
`Test-ResourceSyntax`, `Test-ResourceEncoding`, `Test-RequiredKeys`,
`Test-PlaceholderParity`, `Test-LockedContent`, and `Test-PseudoLocale` checks
on the exact scoped immutable or final-patch files. Their actual bundles are
supporting evidence; they do not prove RTL or Unicode runtime behavior.

## Repair boundaries

- Never translate resources as part of globalization repair.
- Never inject localization dependencies into protocol paths.
- Preserve intended terminal protocol and cell-width semantics.
- Run the smallest existing tests that exercise the exact fix.
- Report unavailable native validation as unavailable, never passing.
- Re-review the final patch and rerun applicable resource checks after edits.
