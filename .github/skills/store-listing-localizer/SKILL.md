---
name: store-listing-localizer
description: 'Automate Microsoft Store (Partner Center) listing localization for Intelligent Terminal (Store ID 9NMQC2SSJX24). Use when asked to export/import Store listings, localize release notes / descriptions / captions / features into 80+ languages, update listingData CSV, or push Store listing translations without manually clicking through Partner Center. Drives Partner Center export/import via the Playwright MCP and merges per-locale translations into the listingData CSV.'
license: Complete terms in LICENSE.txt
---

# Store Listing Localizer

End-to-end automation for localizing the Intelligent Terminal Microsoft Store
listing: **export** the `listingData` CSV from Partner Center, **localize** the
changed fields (release notes, description, captions, features) into all 80+
Store locales using the repo's terminology rules, then **import** the result —
without manually logging into and clicking through the Partner Center web UI.

- Store ID: `9NMQC2SSJX24` ·
  Overview: `https://partner.microsoft.com/en-us/dashboard/products/9NMQC2SSJX24/overview`
- The CSV has 89 locale columns × ~450 field rows. See
  [listing-csv-format.md](./references/listing-csv-format.md).

## When to Use This Skill

- "Export the Store listings and localize the release notes to all languages."
- "The en-US release note changed — update every locale and re-import."
- Updating Store description / screenshot captions / features across locales.
- Any Partner Center listing export → translate → import round trip.
- The new en-US text may come **from the export CSV** *or* **be given in the
  prompt** (handled via en-US overrides — see below).

## Prerequisites

- **Playwright MCP** registered as `playwright` in `~/.copilot/mcp-config.json`
  (`@playwright/mcp`, `--browser msedge`, dedicated `--user-data-dir`,
  `--output-dir <repo>\Generated Files\playwright-mcp`). Write browser outputs
  to a per-run subfolder `store-listing-localizer/<YYYY-MM-DD>/…` (pass that as
  the `filename` subpath). **Restart Copilot CLI** after enabling it, then sign
  into Partner Center once in the launched Edge window. Full setup + gotchas:
  [partner-center-mcp-workflow.md](./references/partner-center-mcp-workflow.md).
- **Node.js** (uses only built-ins; `scripts/` has no npm dependencies).
- **AppName table** — the per-locale product names ship with the skill at
  [`references/intelligent-terminal-translations.md`](./references/intelligent-terminal-translations.md),
  and `apply.mjs` defaults to it. Override with `--appnames <path>` only if you
  maintain a newer copy elsewhere.

## Workflow

Track progress with a TODO list; each step links to its reference.

1. **Export** the listing CSV from Partner Center —
   [partner-center-mcp-workflow.md#export](./references/partner-center-mcp-workflow.md).
   Copy the downloaded `listingData-9NMQC2SSJX24-*.csv` into a clean working dir.
2. **Build the work order** — lists every translatable field with its en-US
   source and which locales are stale:

   ```bash
   node scripts/extract.mjs --csv <export.csv> [--enus <overrides.json>] --out work-order.json
   ```
3. **Translate** each changed field into every target locale, following
   [localization-rules.md](./references/localization-rules.md) (locked tokens,
   `.resw` terminology alignment, RTL handling). Use per-language sub-agents for
   breadth + a reviewer pass. Emit `translations.json` keyed
   `{ "<locale>": { "<Field>": "<text>" } }`.
4. **Merge** (`--appnames` defaults to the bundled
   `references/intelligent-terminal-translations.md`):

   ```bash
   node scripts/apply.mjs --csv <export.csv> [--appnames <translations.md>] \
     --translations translations.json [--enus overrides.json] \
     [--changed-fields ReleaseNotes] --out <stem>-localized.csv
   ```
5. **Verify** the output (cols/rows unchanged, spot-check locales), then
   **import** `<stem>-localized.csv` —
   [partner-center-mcp-workflow.md#import](./references/partner-center-mcp-workflow.md).
6. Review in the UI and **publish the submission** separately (import only
   stages the listing).

### en-US supplied in the prompt (not from the CSV)

When the new en-US text is given in the prompt, write it to an overrides file
and pass it to both scripts:

```json
// enus.json
{ "ReleaseNotes": "Version v0.1.1600\n\n- New: ...\n- Fixed: ..." }
```

`--enus` updates the en-US column *and* marks the field as **changed**, so stale
per-locale values are never preserved (see Gotchas).

## Scripts

| Script | Purpose |
|--------|---------|
| [`scripts/extract.mjs`](./scripts/extract.mjs) | Build a translation work order (what to translate, into which locales). |
| [`scripts/apply.mjs`](./scripts/apply.mjs) | Merge translations + AppName table into the export → `*-localized.csv`. |
| [`scripts/fields.mjs`](./scripts/fields.mjs) | Field classification (appname / translate / verbatim) + AppName-table parser. |
| [`scripts/csvlib.mjs`](./scripts/csvlib.mjs) | Quote-aware CSV parse/serialize (UTF-8 BOM + CRLF, embedded newlines). |

Run any script with `--help` for its options.

## Gotchas

- **Stale translations are the #1 risk.** When an en-US field changes, every
  locale still holds the *previous version's* text. `apply.mjs` has an
  **automatic version-drift safety net**: for any field whose en-US value
  carries a `v<x.y.z>` token (e.g. ReleaseNotes), a locale whose existing text
  has a *different* token is treated as stale and refreshed to the new en-US —
  even if you forget `--changed-fields`. Still pass `--enus` (or
  `--changed-fields`) for changed fields to be explicit; the guard is a backstop.
  Verified: without any flag, a German release note that kept `v0.1.1531` while
  en-US moved to `v0.1.1600` is now auto-refreshed.
- **The Playwright MCP needs a CLI restart** to load, and a **dedicated** Edge
  profile dir (never the live `Edge\User Data`, which is locked while Edge runs).
- **Import ≠ publish.** Importing stages the draft listing; the submission must
  be published separately.
- **Never edit the `Field`/`ID`/`Type` columns** or split the CSV on raw
  newlines — ReleaseNotes/Description contain embedded newlines. Use `csvlib.mjs`.
- **Locale codes are lowercase** in the CSV (`zh-cn`), mixed-case in `.resw`
  (`zh-CN`). Match case-insensitively when borrowing terminology.
- **Version string is locked** (`Version v0.1.1600`) — copy verbatim per locale;
  only translate the bullet text. Keep brand/tech tokens (`Copilot`, `CLI`,
  `Agent pane`, `PowerShell`) in English.
- **Product name IS localized in body text.** `apply.mjs` replaces "Intelligent
  Terminal" inside Description/ReleaseNotes/Features with the locale's AppName
  (matching the Title), e.g. de-DE "Intelligentes Terminal", zh-CN "智能终端".
  This is URL-safe (the `github.com/microsoft/intelligent-terminal` URL is
  untouched). Don't hand-keep the English name in translated bullets.
- **ReleaseNotes has a hard 1500-character limit PER LOCALE** (Description
  10000, ShortDescription 1000). Translations expand vs. English by up to
  ~1.3–1.4×, so a 1280-char en-US ReleaseNotes produced 1680-char translations
  that **failed the whole import mid-run** ("Processing failed" → View errors →
  "ReleaseNotes is too long"). Keep en-US ReleaseNotes ≤ ~1100 chars and tell
  the translation sub-agents the ≤1400 target explicitly. `apply.mjs` now
  runs the length guard **before writing** and refuses to emit an over-limit
  CSV (exits non-zero, no output file) — so a known-bad file can't be imported.

## Troubleshooting

| Issue | Solution |
|-------|----------|
| Playwright (`playwright/*`) MCP tools not available | The Playwright MCP didn't load — restart Copilot CLI; confirm the `playwright` entry in `~/.copilot/mcp-config.json`. |
| Export lands on a login page | Corp SSO session expired — complete the interactive Microsoft login + MFA in the Edge window, then retry. |
| Profile-lock / "browser in use" error | `--user-data-dir` points at the live Edge profile; use the dedicated `msstore-playwright-profile` dir. |
| Import rejected / mojibake | Output must be UTF-8 **BOM** + CRLF with unchanged col/row counts — `apply.mjs` does this; don't re-save the CSV in another editor. |
| A locale shows the old version number | The field wasn't marked changed — re-run `apply.mjs` with `--enus`/`--changed-fields`. |

## References

- [partner-center-mcp-workflow.md](./references/partner-center-mcp-workflow.md) — Playwright MCP export/import steps, auth, selectors.
- [listing-csv-format.md](./references/listing-csv-format.md) — CSV schema and field handling.
- [localization-rules.md](./references/localization-rules.md) — terminology, locked tokens, `.resw` alignment.
