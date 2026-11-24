---
name: store-listing-localizer
description: 'Localize Microsoft Store release notes, descriptions, screenshot captions, and features for Intelligent Terminal. Export/import listingData across Store locales using Playwright MCP with the Edge extension and verify the saved draft.'
license: Complete terms in LICENSE.txt
---

# Store Listing Localizer

Update only the requested listing fields, translate them into every target
locale, and verify the saved Partner Center draft.

## Prerequisites

Examples assume PowerShell on Windows; the current browser workflow uses Edge on Windows.

- Playwright MCP configured with `--extension --browser msedge`, reached through
  the configured Shared MCP Gateway, and the Playwright extension in Edge.
- Node.js; the bundled scripts use built-ins only.
- The bundled [AppName table](./references/intelligent-terminal-translations.md),
  used by `apply.mjs` for localized product names.

## Workflow

Track progress with a TODO list. Keep run artifacts under
`Generated Files\store-listing-localizer\<YYYY-MM-DD>` beneath the repository
root; copy tool-saved exports into this run folder if output routing saves them
elsewhere.

1. **Connect and export** using the
   [browser workflow](./references/partner-center-mcp-workflow.md). Initiate the
   Edge connection before translation work so the user can approve it early.
   Preserve the current draft as `listing-original.csv`.
2. **Prepare English first.** Condense the supplied release notes without
   changing their meaning or qualifications. Confirm the release version
   against the intended package. Put supplied or revised source fields in
   `enus.json`, for example `{ "ReleaseNotes": "Version v<x.y.z>\n\n- ..." }`.
   Keep English release notes around 1,100 characters to allow translation
   expansion. Omit `--enus` below when using the unchanged exported source.
3. **Extract and translate.** Run the extraction command below, then follow
   [localization-rules.md](./references/localization-rules.md) for terminology,
   locked tokens, product names, translation, and independent review. Write
   `translations.json` as `{ "<locale>": { "<Field>": "<text>" } }`.
   Require every target locale from the fresh export; do not accept English
   fallbacks as completed translations. Preserve the export's convention for
   English and pseudo-locales.
4. **Merge and scope.** Run `apply.mjs` below to create `listing-merged.csv`.
   Copy the original records and replace only the requested fields' locale
   cells from that merge. Write this scoped result as `listing-import.csv`
   using `csvlib.mjs`.
5. **Verify and import `listing-import.csv`.** Check exact locale coverage,
   unchanged row/column structure and metadata, no unrelated cell changes, and
   [Store character limits](./references/listing-csv-format.md). Complete the
   browser reference's import and persisted verification steps. Leave the
   submission in draft unless the user explicitly authorizes publishing.

Run these commands from this skill's directory, using absolute artifact paths:

```powershell
$run = '<absolute run folder>'
node scripts\extract.mjs --csv "$run\listing-original.csv" --enus "$run\enus.json" --out "$run\work-order.json"
node scripts\apply.mjs --csv "$run\listing-original.csv" --enus "$run\enus.json" --translations "$run\translations.json" --out "$run\listing-merged.csv"
```

Both scripts support `--help`. When the exported English already contains a
change, pass `--changed-fields <Field1,Field2>` to mark its translations stale.

## Gotchas

- **`--changed-fields` is not a write-scope filter.** `apply.mjs` also fills
  unrelated empty cells and applies AppNames. Upload the scoped
  `listing-import.csv`, not its raw merge output.
- **Successful import is not sufficient.** Partner Center can reorder existing
  screenshots. The browser reference owns the checks and restoration procedure.

## References

- [partner-center-mcp-workflow.md](./references/partner-center-mcp-workflow.md) — browser connection, export, upload, and persisted verification.
- [listing-csv-format.md](./references/listing-csv-format.md) — CSV schema and field handling.
- [localization-rules.md](./references/localization-rules.md) — terminology, locked tokens, `.resw` alignment.
