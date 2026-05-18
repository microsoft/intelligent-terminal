---
applyTo: "src/cascadia/**/Resources/**/*.resw"
---

# Localization Instructions for .resw Resource Files

## Overview

This project uses `.resw` (XML resource) files for localization across **88 locale folders**. Resources live under each component's `Resources/{locale}/Resources.resw` path.

### Components with localized resources

| Component | Path | Key count (approx) |
|-----------|------|---------------------|
| TerminalApp | `src/cascadia/TerminalApp/Resources/` | 300+ |
| TerminalSettingsModel | `src/cascadia/TerminalSettingsModel/Resources/` | 270+ |
| TerminalSettingsEditor | `src/cascadia/TerminalSettingsEditor/Resources/` | 700+ |
| CascadiaPackage | `src/cascadia/CascadiaPackage/Resources/` | brand names only |
| ContextMenu | `src/cascadia/ContextMenu/Resources/` | brand names only |

### Major translation locales (12)

`de-DE`, `es-ES`, `fr-FR`, `it-IT`, `ja-JP`, `ko-KR`, `pt-BR`, `ru-RU`, `sr-Cyrl-RS`, `uk-UA`, `zh-CN`, `zh-TW`

### Pseudo-locales (3)

`qps-ploc`, `qps-ploca`, `qps-plocm` — use English fallback text (not real translations).

### Minor locales (~73)

Only have `ContextMenu/Resources.resw` and `CascadiaPackage/Resources.resw` with `{Locked}` brand keys.

## Finding Missing Localizations

When new English keys are added to `en-US/Resources.resw`, they must be localized. To find gaps:

```powershell
# Compare en-US keys against a locale to find missing keys
$enUS = [xml](Get-Content "src/cascadia/TerminalApp/Resources/en-US/Resources.resw")
$target = [xml](Get-Content "src/cascadia/TerminalApp/Resources/zh-CN/Resources.resw")
$enKeys = $enUS.root.data | ForEach-Object { $_.name }
$targetKeys = $target.root.data | ForEach-Object { $_.name }
$missing = $enKeys | Where-Object { $_ -notin $targetKeys }
$missing  # Keys that need translation
```

Repeat for each component and each of the 12 major locales.

## Translation Process

### Approach: per-language sub-agents

Use **one translator sub-agent per language** (not bulk scripts). Each agent:
1. Reads the en-US source strings
2. Reads existing translations in the target locale for context/consistency
3. Translates missing keys
4. Writes them into the target `.resw` file

Then use **one reviewer sub-agent per language** to verify quality.

### File format requirements

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

## Terminology — Mandatory Rules

### "Agent" (= Intelligent Agent / AI Agent)

The term "Agent" in this product refers to an **AI intelligent agent**. Translate per locale, aligned with **Microsoft Azure AI Foundry documentation**:

| Locale | Translation | Notes |
|--------|-------------|-------|
| zh-CN | 智能体 | NOT 代理 (that means proxy). NOT AI 智能体 (redundant — 智能体 already implies AI). |
| zh-TW | 代理 / AI 代理 | Standard in Taiwan per Microsoft docs. NOT 智慧體 or 智能體. |
| ja-JP | エージェント | Katakana loanword |
| ko-KR | 에이전트 | Hangul transcription |
| de-DE | Agent | German noun, capitalized |
| fr-FR | agent | Native French word, lowercase |
| es-ES | agente | Native Spanish |
| pt-BR | agente | Native Portuguese |
| it-IT | agente | Native Italian |
| ru-RU | агент | Cyrillic loanword |
| uk-UA | агент | Cyrillic loanword |
| sr-Cyrl-RS | агент | Cyrillic loanword |

### Terms that must NOT be translated (keep English)

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

### `{Locked}` annotations

Keys containing brand names, technical identifiers, or format strings should have `{Locked}` in their `<comment>` element:

```xml
<data name="BrandName" xml:space="preserve">
  <value>Intelligent Terminal</value>
  <comment>{Locked} Product name — do not translate.</comment>
</data>
```

For partial locking (e.g., a sentence containing a brand name):
```xml
<comment>{Locked="Copilot","CLI"} Translate the sentence but keep these terms in English.</comment>
```

### Translator context comments

For keys where "Agent" appears, add a comment in en-US to guide translators:

```xml
<comment>Here "agent" refers to an AI intelligent agent (e.g., Copilot, Claude), not a proxy or human agent.</comment>
```

## Adding a New Localized Key

When adding a new key to en-US:

1. **Add to `en-US/Resources.resw`** with appropriate `{Locked}` annotation and translator comments
2. **Determine if the key needs translation:**
   - If the value is a brand name, technical identifier, or format string → mark `{Locked}` and copy verbatim to all locales
   - If the value contains user-visible text → translate to all 12 major locales
3. **Add to pseudo-locales** (`qps-ploc`, `qps-ploca`, `qps-plocm`) with the English fallback text
4. **Do NOT add** to the 73 minor locales unless they already have a `Resources.resw` for that component

## Validation

After modifying `.resw` files, validate XML well-formedness:

```powershell
$xml = New-Object System.Xml.XmlDocument
$xml.Load($path)  # Throws if malformed
```

Also verify no `AI 智能体` exists in zh-CN (redundant prefix):

```powershell
$content = [System.IO.File]::ReadAllText($path)
if ($content -match 'AI\s+智能体') { Write-Warning "Redundant AI prefix found" }
```
