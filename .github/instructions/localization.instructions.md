---
applyTo: "src/cascadia/**/Resources/**/*.resw"
---

# Localization Instructions for .resw Resource Files

## Overview

This project uses `.resw` (XML resource) files for localization. Resources live under each component's `Resources/{locale}/Resources.resw` path.

> **Important:** The set of locale folders is the authoritative source for how many languages we support. Never hardcode the count — discover it dynamically from the folders. The Rust WTA project (`wta/locales/*.yml`) must always match this same set.

### Components with localized resources

| Component | Path | Key count (approx) |
|-----------|------|---------------------|
| TerminalApp | `src/cascadia/TerminalApp/Resources/` | 300+ |
| TerminalSettingsModel | `src/cascadia/TerminalSettingsModel/Resources/` | 270+ |
| TerminalSettingsEditor | `src/cascadia/TerminalSettingsEditor/Resources/` | 700+ |
| CascadiaPackage | `src/cascadia/CascadiaPackage/Resources/` | brand names only |
| ContextMenu | `src/cascadia/ContextMenu/Resources/` | brand names only |

### Locale categories

- **Major translation locales** — have full translations for TerminalApp, TerminalSettingsModel, and TerminalSettingsEditor (e.g., `de-DE`, `ja-JP`, `zh-CN`, etc.)
- **Pseudo-locales** — `qps-ploc`, `qps-ploca`, `qps-plocm` — use English fallback text (not real translations)
- **Minor locales** — only have `ContextMenu/Resources.resw` and `CascadiaPackage/Resources.resw` with `{Locked}` brand keys

To determine the full locale set:

```powershell
# Authoritative locale list (from TerminalApp)
Get-ChildItem "src/cascadia/TerminalApp/Resources/" -Directory | Select-Object -ExpandProperty Name
```

## Adding or Updating Localized Strings

### Step 0: Discover what needs localization

Before translating, determine which strings are new or changed:

```powershell
# Compare en-US keys against a locale to find missing keys
$enUS = [xml](Get-Content "src/cascadia/TerminalApp/Resources/en-US/Resources.resw")
$target = [xml](Get-Content "src/cascadia/TerminalApp/Resources/zh-CN/Resources.resw")
$enKeys = $enUS.root.data | ForEach-Object { $_.name }
$targetKeys = $target.root.data | ForEach-Object { $_.name }
$missing = $enKeys | Where-Object { $_ -notin $targetKeys }
$missing  # Keys that need translation
```

For changed strings, diff against main:

```powershell
git diff main -- src/cascadia/TerminalApp/Resources/en-US/Resources.resw
```

Repeat for each component.

### Step 1: Determine the target locale set

The locale set is **not hardcoded**. Derive it from the locale folders:

```powershell
# List all locale folders (this is the authoritative set)
Get-ChildItem "src/cascadia/TerminalApp/Resources/" -Directory | Select-Object -ExpandProperty Name
```

- If a key is a brand name / technical identifier / format string → mark `{Locked}` and copy verbatim to **all** locales
- If a key contains user-visible text → translate to all **major** locales (those with full Resources.resw for that component)
- Always add to pseudo-locales (`qps-ploc`, `qps-ploca`, `qps-plocm`) with the English fallback text
- Do NOT add to minor locales unless they already have a `Resources.resw` for that component

### Step 2: Translate to all locales

Use per-language sub-agents that:
1. Read the en-US source strings (only the new/changed keys from Step 0)
2. Read existing translations in the target locale for tone/style consistency
3. Follow the [Terminology Alignment](#terminology-alignment) process for term choices
4. Follow the [Resource Comments](#resource-comments) rules — locked tokens must appear verbatim
5. Write translations into the target `.resw` file

### Step 3: QA review

Use a separate reviewer sub-agent per language that checks:
- **`{Locked}` tokens** preserved verbatim (see [Resource Comments](#resource-comments))
- **Terminology** aligned with existing translations (see [Terminology Alignment](#terminology-alignment))
- RTL languages have correct logical string order
- No mojibake or encoding issues
- XML well-formedness preserved

### Step 4: Validate

After modifying `.resw` files, validate XML well-formedness:

```powershell
$xml = New-Object System.Xml.XmlDocument
$xml.Load($path)  # Throws if malformed
```

## File Format Requirements

- `.resw` files are XML with **UTF-8 BOM** encoding — always save with BOM
- Use `XmlDocument` (PowerShell) or `fs.readFileSync`/`writeFileSync` (Node.js) — **never use text-based edit tools** on `.resw` files (they corrupt XML)
- Node.js is more reliable than PowerShell for CJK content (PowerShell 5.1 has encoding issues with Chinese characters)
- Preserve `xml:space="preserve"` attributes — use namespace-aware setter:
  ```powershell
  $data.SetAttribute('xml:space', 'http://www.w3.org/XML/1998/namespace', 'preserve')
  ```
- Preserve BOM when using Node.js:
  ```js
  let content = fs.readFileSync(path, 'utf8');
  if (!content.startsWith('\uFEFF')) content = '\uFEFF' + content;
  fs.writeFileSync(path, content, 'utf8');
  ```

## Terminology Alignment

Align translations using these sources **in priority order**:

1. **Existing `.resw` translations** in this repo — ensures consistency across components
   - `src/cascadia/TerminalApp/Resources/{locale}/Resources.resw`
   - `src/cascadia/TerminalSettingsEditor/Resources/{locale}/Resources.resw`
   - `src/cascadia/TerminalSettingsModel/Resources/{locale}/Resources.resw`
2. **Existing Rust WTA translations** in `wta/locales/{locale}.yml` — ensures consistency between C++ and Rust UI
3. **Microsoft Learn localized documentation** — for new terms not yet in `.resw` or `.yml`, check the official localized docs (`learn.microsoft.com/{locale}/...`) for Microsoft-standard translations

### Process

1. Before translating, check if the term already exists in `.resw` for that locale (in another component)
2. If it does → use the same translation (cross-component consistency)
3. If not in `.resw`, check if the term exists in `wta/locales/{locale}.yml`
4. If still not found → check Microsoft Learn localized docs (`learn.microsoft.com/{locale}/...`)
5. If no established term exists → research community usage (Wikipedia, academic, developer forums)
6. If the native word means something unrelated to the concept → use English or a phonetic transliteration

> **Example — "Agent":** In this product, "Agent" means an AI intelligent agent (not a proxy or human agent). Apply the process above — e.g., for zh-CN, Microsoft Azure AI Foundry docs use 智能体 (NOT 代理, which means proxy). Each locale may differ; always verify against the priority sources.

## Resource Comments

The `.resw` `<comment>` element serves multiple purposes for translators. Always provide comments that help produce correct translations.

### Locked tokens

Use `{Locked}` in comments to mark content that must NOT be translated:

- **Full lock** — entire value is non-translatable:
  ```xml
  <comment>{Locked} Product name — do not translate.</comment>
  ```
- **Partial lock** — specific tokens within a translatable sentence:
  ```xml
  <comment>{Locked="Copilot","CLI"} Translate the sentence but keep these terms in English.</comment>
  ```

Terms that should always be locked:

| Term | Reason |
|------|--------|
| Hooks, Hook | Technical term (shell hooks) |
| CLI | Technical acronym |
| ACP | Technical acronym (Agent Control Protocol) |
| PATH | Environment variable |
| PowerShell | Product name |
| Copilot, Claude, Gemini | Brand names |
| Intelligent Terminal | Product name |
| JSON, YAML, XML | Technical formats |

### Translator guidance comments

Use comments to disambiguate meaning and prevent mistranslation:

```xml
<!-- Disambiguate a term with multiple meanings -->
<comment>Here "agent" refers to an AI intelligent agent (e.g., Copilot, Claude), not a proxy or human agent.</comment>

<!-- Explain UI context -->
<comment>This text appears as a button label in the settings page.</comment>

<!-- Clarify format placeholders -->
<comment>{0} is replaced with the profile name at runtime.</comment>
```

Good translator comments:
- Explain **which meaning** of an ambiguous word is intended
- Describe **where in the UI** the string appears (button, tooltip, title, error message)
- Document **format placeholders** (`{0}`, `{1}`, etc.) and what they represent
- Note **character length constraints** if the UI has limited space

## Common Pitfalls

| Issue | Solution |
|-------|----------|
| XML corruption | Never use text-based edit tools; use XmlDocument or Node.js |
| Missing BOM | Always save with UTF-8 BOM (`\uFEFF` prefix) |
| PowerShell CJK issues | Use Node.js for Chinese/Japanese/Korean content |
| `xml:space` lost | Use namespace-aware `SetAttribute` |
| Terminology inconsistency with WTA | Check `wta/locales/{locale}.yml` for that term |
