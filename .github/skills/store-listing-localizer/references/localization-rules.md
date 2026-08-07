# Localization rules for Store listing text

Store-listing localization reuses the **same terminology and locked-token
discipline** as the repo's `.resw` UI localization. The CSV is a different file
format, but the *word choices* must stay consistent with the shipped UI so the
product reads the same in the Store and in the app.

## Source of truth, in priority order

When translating a term, resolve it using these sources **in order** (this
mirrors `.github/instructions/localization.instructions.md`):

1. **Existing `.resw` translations** in this repo — the authoritative term bank:
   - `src/cascadia/TerminalApp/Resources/<locale>/Resources.resw`
   - `src/cascadia/TerminalSettingsEditor/Resources/<locale>/Resources.resw`
   - `src/cascadia/TerminalSettingsModel/Resources/<locale>/Resources.resw`
2. **Rust WTA translations** — `tools/wta/locales/<locale>.yml`.
3. **Microsoft Learn localized docs** — `learn.microsoft.com/<locale>/...` for
   Microsoft-standard terms not yet in the repo.
4. Community/established usage; if no native term fits, keep English or
   transliterate.

> **Locale code mapping.** The CSV uses lowercase hyphen codes (`zh-cn`,
> `pt-br`); `.resw` folders use mixed case (`zh-CN`, `pt-BR`). Match them
> case-insensitively when borrowing a term.

## Locked tokens — copy verbatim in every locale

Never translate these (same list as the `.resw` rules):

| Token | Why |
|-------|-----|
| `Copilot`, `Claude`, `Gemini` | Brand names |
| `CLI`, `ACP`, `PATH` | Technical acronyms / env var |
| `PowerShell` | Product name |
| `JSON`, `YAML`, `XML` | Formats |
| `Hooks` / `Hook` | Technical term |
| `Agent pane` | Product feature name — keep "Agent" as the AI-agent sense |
| `Version v<x.y.zzzz>` | Release-notes version string |

> **"Agent" means an AI agent**, not a proxy or human agent. Each locale may
> differ — e.g. zh-CN uses 智能体 (per Azure AI Foundry docs), **not** 代理.
> Always verify against the `.resw` term bank for that locale.

## Product name

The localized product name per locale comes from
`intelligent-terminal-translations.md` (the AppName table). `apply.mjs` fills
`Title` from it automatically, **and also localizes the product name inside
translatable body text** (Description, ReleaseNotes, Features) so the body
matches the Title — e.g. de-DE "Intelligentes Terminal", zh-CN "智能终端",
fr-FR "Terminal Intelligent". Some locales keep the English "Intelligent
Terminal" (e.g. `en-GB`, `da-DK`); that is intentional — the table value is used
verbatim.

> **URL-safe.** The replacement only touches the spaced, capitalized product
> name "Intelligent Terminal". The GitHub URL form
> `github.com/microsoft/intelligent-terminal` (hyphenated, lowercase) is never
> altered. Disable the behavior with `apply.mjs --no-localize-product-name`.
>
> **Therefore: do NOT keep "Intelligent Terminal" English inside release-note
> bullets when translating** — let `apply.mjs` substitute the localized AppName.
> Translators should translate the surrounding sentence and may leave the
> product name as the en-US "Intelligent Terminal" placeholder; the script
> localizes it.

## Per-language workflow (for the agent)

For each target locale that needs a changed field translated:
1. Read the new en-US source (from the work order or the prompt).
2. Pull established terms from the `.resw` term bank for that locale (step 1
   above) so wording matches the shipped UI.
3. Translate the free text; keep every locked token in English; keep the
   `Version v…` string verbatim.
4. For RTL locales (`ar-SA`, `he-IL`, `fa-IR`, `ur-PK`), keep logical order;
   don't reorder the version string or punctuation manually.
5. Emit the result into the `translations.json` consumed by `apply.mjs`, keyed
   `{ "<locale>": { "<Field>": "<text>" } }`.

Use per-language sub-agents for breadth (80+ locales), then a reviewer pass that
checks: locked tokens intact, version string verbatim, terminology aligned with
`.resw`, no encoding mojibake.

## What not to localize

Screenshot/logo/trailer URLs, `OverrideLogosForWin10`, and other asset/boolean
fields are `verbatim` — see `listing-csv-format.md`. Screenshot **captions**,
however, *are* translatable text.
