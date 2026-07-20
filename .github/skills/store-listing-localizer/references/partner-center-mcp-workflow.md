# Partner Center automation via the Playwright MCP

The export/import steps drive `https://partner.microsoft.com` in a real Edge
browser through the **Playwright MCP** (`@playwright/mcp`). Partner Center is
gated by Microsoft corp SSO + MFA, so automation relies on a **persistent
browser profile** that holds the signed-in session — you log in once, then every
later run reuses the cookies. There is no password or secret in this skill.

## One-time setup

1. The MCP is registered in `~/.copilot/mcp-config.json` as `playwright` with:
   - `--browser msedge`
   - `--user-data-dir C:\Users\<you>\AppData\Local\msstore-playwright-profile`
     (a **dedicated** profile dir — never your live Edge `User Data`, which is
     locked while Edge runs).
   - `--output-dir <repo>\Generated Files\playwright-mcp` (the gitignored
     `**/Generated Files/` root). This is a **static root** shared by every
     Playwright use — see **Output location** below for the per-run nesting.
2. **Restart Copilot CLI** after the config changes — MCP servers load at
   startup, so a newly-added server is not available mid-session.
3. First run only: navigate to the dashboard and complete the interactive
   Microsoft login + MFA in the launched Edge window. The session persists in
   the profile dir for subsequent runs.

## Output location

`--output-dir` is a **single static root** set at MCP launch, so every Playwright
purpose would otherwise dump files into the same flat folder. Keep runs isolated
by passing a **relative subpath** in the `filename` argument of the Playwright
MCP (`playwright/*`) save tools (e.g. the PDF-save / screenshot tools) — the MCP
resolves it under the root and creates subdirectories as needed. Convention:

```
<output-dir>/<skill-name>/<YYYY-MM-DD>/<file>
e.g. store-listing-localizer/2026-06-16/listingData.pdf
```

So a save call uses `filename: "store-listing-localizer/2026-06-16/<file>"`.
This keeps each skill + execution-date in its own folder under the shared,
gitignored `Generated Files\playwright-mcp` root.

## Gotchas

- **Restart required.** Adding/editing the MCP entry does nothing until Copilot
  CLI restarts. If the Playwright (`playwright/*`) tools aren't listed, the MCP
  isn't loaded.
- **Dedicated profile dir.** Pointing `--user-data-dir` at the live Edge profile
  (`...\Edge\User Data`) fails with a profile-lock error whenever Edge is open.
- **Session expiry.** Corp SSO cookies expire (often daily). When a run lands on
  the login page instead of the dashboard, just complete the interactive login
  again — the automation can pause for it.
- **Downloads location.** "Export listings" saves the `.csv` to the browser's
  configured download folder. With `--output-dir` set (see **Output location**),
  the MCP writes it under that root (`<repo>\Generated Files\playwright-mcp\…`),
  **not** `~/Downloads`. Capture the download event or the newest
  `listingData-9NMQC2SSJX24-*.csv` under the output root and copy it into a
  clean working dir before processing, so re-runs don't pick up a stale file.
- **The `Updated`/`Unchanged` pill** on the Store listings row reflects pending
  edits; after a successful import it flips to `Updated`. Don't treat the pill
  as a success signal for the *submission* — importing only stages the listing,
  it does not publish.

## Workflow (agent-driven, using the Playwright MCP `playwright/*` tools)

### Export
1. Navigate (Playwright MCP) to
   `https://partner.microsoft.com/en-us/dashboard/products/9NMQC2SSJX24/overview`
   (Store ID `9NMQC2SSJX24`). If redirected to login, complete it interactively.
2. Locate the **Store listings** section and click the **Export listings** link.
3. Wait for the `.csv` to finish downloading (under the `--output-dir` root, e.g.
   `<repo>\Generated Files\playwright-mcp\`).
4. Copy the newest `listingData-9NMQC2SSJX24-*.csv` into a working dir for
   processing (keep the original download untouched as a backup).

### Import
1. From the same overview page, click **Import listings**.
2. In the file picker, choose the localized CSV
   (`<stem>-localized.csv`) produced by `apply.mjs`.
3. Confirm the upload. Verify the Store listings row shows **Updated** and spot
   check a couple of locales in the UI before publishing the submission.

> Importing **stages** the localized listings on the draft submission. Review
> and **publish the submission** separately (in the UI or via StoreBroker /
> msstore-cli) to push it live.

## Selectors

Partner Center markup changes over time, so prefer **role/text-based** locators
(`getByRole('link', { name: 'Export listings' })`) over brittle CSS/XPath. The
three controls on the Store listings row are the links **Add/remove languages**,
**Export listings**, and **Import listings**.
