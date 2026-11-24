# Partner Center browser workflow

Use the configured route:
**Copilot → Shared MCP Gateway → Playwright MCP → Edge extension → signed-in Edge**.
MCP remains the control transport; `--extension --browser msedge` selects the
existing browser instead of launching a separate profile.

Product overview:
`https://partner.microsoft.com/en-us/dashboard/products/9NMQC2SSJX24/overview`.

## Connection preflight

1. Discover the configured Playwright backend and tool schemas through the
   gateway. Claim its exclusive lease, then **call** `browser_tabs` or
   `browser_snapshot` to initiate the extension handshake.
2. If Edge prompts, ask the user to approve the connection and select the
   Partner Center tab. Chat approval does not approve the browser connection.
   An already authorized connection may not prompt; confirm actual tab access.
3. Open the overview and complete any interactive sign-in/MFA while the user is
   available. Confirm the product and intended draft submission.

Do not switch to a new profile, extract credentials, or change global routing.
Release the lease after browser work. For gateway errors, follow the
`mcp-gateway-setup` skill's recovery rules; in particular, do not retry, release,
or bypass an exclusive backend reporting `server_outcome_unknown`. Recovery
requires coordinating an idle-time restart, not assuming a client reload worked.

## Export

Click **Export listings** and observe the response. The verified UI posts to
`/exportlistings` and returns a JSON array of column objects, not a Playwright
download event. Save the response body using the browser network tools.
The first objects' `field` values are `Field`, `ID`, `Type (Type)`, and `default`,
followed by locale codes. Check this structure and consistent object keys,
then transpose:

```javascript
const keys = Object.keys(columns[0]);
const records = keys.map(key =>
  columns.map(column => String(column[key] ?? '')));
```

Use `csvlib.mjs`'s `indexListing` to validate the records and `writeCsv` to
produce `listing-original.csv`. Preserve the raw response too. If the UI instead
downloads a CSV, preserve that download. Use only a fresh export from the
current draft as the import baseline.

Resolve save paths from the current tool schema and output configuration;
do not assume Downloads or that explicit paths are relative to `--output-dir`.
Save into the run folder defined by `SKILL.md`.

## Import

1. Open **Import listings** and choose **Upload .csv**. Upload only the scoped
   `listing-import.csv` prepared and verified by `SKILL.md`.
2. If the picker reports `DOM.setFileInputFiles: Not allowed`, use
   [extension-compatible upload](#extension-compatible-upload).
3. Keep the page open until processing reports **Complete**. The UI saves
   languages sequentially (`Saving N of ... languages`); use bounded waits
   below the gateway's downstream timeout. Inspect **View errors** on failure.
4. Perform [persisted verification](#persisted-verification). An **Updated**
   status alone is not proof that the content saved correctly.

### Extension-compatible upload

Discover `browser_evaluate` and `browser_drop`. Verify the current CSV input in
the page DOM before reusing the observed ID `listing-import-filename`.
Add a one-use drop handler on the DOM `main` element (CSS selector `main`,
not a browser tab handle)
that prevents the default drop, verifies one file with the expected name,
assigns `event.dataTransfer.files` to that input, and dispatches a bubbling
`change` event. Use `browser_drop` on `main` with the exact import file path.
This invokes the page's normal
import validation without the unsupported file-picker operation.

## Persisted verification

Re-export the draft and compare all cells with `listing-import.csv`. Require
exact requested values and no unrelated changes. Check screenshot positions,
URLs, and captions as well as text. Spot-check English and representative
localized release-note controls in the UI, then confirm the submission state.

### Screenshot-order restoration

CSV import has reordered existing screenshots while preserving image/caption
pairs. If verification finds only this side effect, restore positions from the
fresh backup; do not assume a fixed image order or re-import the CSV.

Read each affected locale's current listing and verify its asset/caption set
matches the expected result before swapping. Use the existing screenshot swap
operation: POST to the current submission's `/listingasset/swap` with
`listingId`, `listingAssetId1`, and `listingAssetId2` read from that listing.
Re-read after each swap, checking the exact order and unchanged listing
properties. Use small bounded batches; after an interruption, inspect current
state before resuming. Finally re-export and repeat the full comparison.
