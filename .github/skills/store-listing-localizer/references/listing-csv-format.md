# listingData CSV format (Partner Center export/import)

The Partner Center **Export listings** button produces a UTF-8 (BOM),
comma-delimited, CRLF CSV. Each **row** is one listing field; each **column** is
one locale. Values legitimately contain embedded newlines (ReleaseNotes,
Description), so always parse with a quote-aware parser — `scripts/csvlib.mjs`
does this; never split on `\n` or use a naive CSV reader.

## Shape (Intelligent Terminal, Store ID `9NMQC2SSJX24`)

- **93 columns**: `Field`, `ID`, `Type (Type)`, `default`, then **89 locale
  columns** starting at `en-us`, `en-gb`, … `zh-tw`.
- **~450 field rows**: `Description`, `ReleaseNotes`, `Title`, screenshots,
  captions, features, search terms, trailers, logos, hardware reqs, etc.
- Locale codes are **lowercase** (`zh-cn`, not `zh-CN`). Look them up
  case-insensitively (`findLocaleCol`).
- **Never edit** the `Field`, `ID`, or `Type` columns — only locale columns.

## Field handling (see `scripts/fields.mjs`)

| Class | Fields | Behavior |
|-------|--------|----------|
| **appname** | `Title` | Filled from the per-locale AppName table (`intelligent-terminal-translations.md`). Falls back to en-US if a locale is missing. |
| **translate** | `Description`, `ReleaseNotes`, `ShortTitle`/`SortTitle`/`VoiceTitle`, `ShortDescription`, `Feature1..20`, `*ScreenshotCaption*`, `SearchTerm*`, `MinimumHardwareReq*`, `RecommendedHardwareReq*`, `TrailerTitle*` | Localized per locale. Uses provided translation → else (if field unchanged) keeps existing → else falls back to en-US. |
| **verbatim** | everything else: `*Screenshot\d+` (URLs), `*Logo*`, `*Promo*`, `Trailer\d+`/`TrailerThumbnail*`, `OverrideLogosForWin10`, `DevStudio`, `PublisherName`, `CopyrightTrademarkInformation`, `AdditionalLicenseTerms` | Copied from en-US into **empty** locale cells only; existing values left untouched. Never translated. |

## Field character limits (hard Store rejects)

Partner Center enforces per-locale maximum lengths and **fails the entire
import** if any locale exceeds them (error surfaces as "Processing failed" →
**View errors** → e.g. "ReleaseNotes is too long (must be 1500 characters or
fewer)").

| Field | Max chars (per locale) |
|-------|------------------------|
| ReleaseNotes | **1500** |
| Description | 10000 |
| ShortDescription | 1000 |

> **Translations expand.** Observed expansion vs. en-US is up to ~1.3–1.4× for
> verbose languages (Scottish Gaelic, French, Greek, German, Tamil). A 1280-char
> en-US ReleaseNotes yielded 1680-char translations that failed import. **Keep
> en-US ReleaseNotes ≤ ~1100 chars**, and instruct translation sub-agents to
> cap output at ≤1400 (condensing wording, never dropping a bullet).
> `apply.mjs` runs the length guard **before writing** and refuses to emit an
> over-limit CSV (exits non-zero, no output file) — a hard stop before importing.

## The stale-translation trap (most important)

When an en-US field changes (e.g. a new `ReleaseNotes` version), the other
locales still hold the **previous version's** translation. Blindly "keeping
existing non-empty" would ship stale notes (wrong version number, missing new
bullets). `apply.mjs` treats any field with an en-US override — or listed in
`--changed-fields` — as **changed**: it never keeps the old per-locale value;
it uses the supplied translation, or falls back to the new en-US text. Always
mark changed fields so stale content can't survive.

## ReleaseNotes specifics

- Format: `Version v<x.y.zzzz>\n\n- bullet\n- bullet…`.
- The **version string** (`Version v0.1.1600`) is **locked** — copy verbatim in
  every locale; only translate the bullet text.
- Product nouns inside bullets (`Agent pane`, `CLI`, `Copilot`, `PowerShell`)
  follow the locked-token rules in `localization-rules.md`.

## Output

`apply.mjs` writes `<stem>-localized.csv` next to the source with **UTF-8 BOM +
CRLF** (matching the export) so Partner Center accepts it on import. It preserves
row/column counts exactly — verify `cols`/`rows` are unchanged before importing.
