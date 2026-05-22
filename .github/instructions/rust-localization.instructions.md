---
applyTo: "wta/locales/**/*.yml"
---

# Localization Instructions for Rust WTA (YAML Locale Files)

## Overview

The WTA (Windows Terminal Agent) Rust project uses **`rust-i18n` v3** with YAML locale files. Translations are compiled into the binary at build time — there is no runtime file loading.

### File structure

```
wta/locales/
├── en-US.yml          ← source of truth (developers edit this first)
├── zh-CN.yml          ← Simplified Chinese
├── zh-TW.yml          ← Traditional Chinese
├── ja-JP.yml          ← Japanese
├── ... (locale set MUST match Terminal's .resw locale folders)
```

> **Important:** The set of `.yml` locale files must always match the set of `.resw` locale folders in `src/cascadia/TerminalApp/Resources/`. If Terminal adds or removes a locale, WTA must follow. Never hardcode the count — discover it dynamically from the `.resw` folders.

### File format

- One YAML file per locale, filename = locale code (e.g., `de-DE.yml`)
- Flat dot-separated keys (no nested objects)

- UTF-8 encoding (no BOM needed — unlike `.resw`)
- Strings wrapped in double quotes

## The `en-US.yml` Source File

`en-US.yml` is the **source of truth**. All other locale files translate from it.

When adding new strings:
1. Add the key + English value to `en-US.yml`
2. Add `{Locked}` comments for non-translatable tokens (see [Non-Translatable Token Rules](#non-translatable-token-rules) below)
3. Translate to all other locale files following [Terminology Alignment](#terminology-alignment) rules (see [Step 1: Determine the target locale set](#step-1-determine-the-target-locale-set) below)
4. Rebuild (`cargo build`) — translations are baked in at compile time

## Non-Translatable Token Rules

Since YAML has no structured comment system like `.resw`, we use `#` comments with `{Locked}` syntax borrowed from .resw. There are three scope levels:

### File-level (top of file, applies to everything)

Place at the very top of the file, before any section headers. These define constraints that apply to **every string** in the file.

```yaml
# ── File-level locks ─────────────────────────────────────────────────────────
# {Locked=qps-ploc,qps-ploca,qps-plocm} "Intelligent Terminal" is the product name
# "Agent" refers to an AI agent (e.g. Copilot, Claude, Gemini), not a human representative or proxy

# ── Setup screen titles ─────────────────────────────────────────────────────
setup.title.first_run: "Welcome to Intelligent Terminal!"
```

### Section-level (applies to all strings in a group)

Place immediately before a group of related strings. Applies until the next section comment.

```yaml
# ── Hooks status output ─────────────────────────────────────────────────────
# {Locked="CLI","PATH","wt-agent-hooks","wta"} - tool/CLI names, env vars
hooks.cli_not_on_path: "✗ CLI not on PATH"
hooks.installed: "✓ installed"
```

### Line-level (applies to one specific string)

```yaml
hint.confirm: "Press Enter to confirm"  # {Locked="Enter"} keyboard key name
```

### Rules

- **File-level** `{Locked}` comment → applies to all strings in the entire file
- **Section-level** `{Locked}` comment → applies to all strings below until the next section comment
- **Line-level** `{Locked}` comment → applies only to that line
- Multiple locked tokens: `# {Locked="Copilot","CLI"}`
- Full lock (entire value must not be translated): `# {Locked}`
- Pseudo-locale-only lock (real locales translate, pseudo-locales keep English): `# {Locked=qps-ploc,qps-ploca,qps-plocm}`
  - Use this for product names like "Intelligent Terminal" that real locales should translate but pseudo-locales should not mangle. This mirrors the `.resw` `{Locked=qps-ploc,qps-ploca,qps-plocm}` convention.
- **Translation rule:** Tokens marked with `{Locked}` (full lock) or `{Locked="token"}` (token lock) must appear **verbatim** (in English) in all locale files. Tokens marked with `{Locked=qps-ploc,qps-ploca,qps-plocm}` (pseudo-locale-only lock) must be kept verbatim only in pseudo-locale files — real locales should translate them. See `.github/instructions/localization.instructions.md` for the full list of non-translatable terms.

## Context Comments for Ambiguous Strings (REQUIRED)

Short strings (1–2 words) and strings with unclear context **must** have a comment explaining when and where the label is shown to the user. Without context, translators will guess — and guesses lead to translation errors that can be politically embarrassing or internationally offensive.

### When is a context comment required?

A context comment is **mandatory** when ANY of these are true:

- The value is **1–2 words** and is not `{Locked}` (e.g., `"installed"`, `"Switch agent"`)
- The value is **ambiguous out of context** — the same word means different things in different domains (e.g., "Agent" = AI agent vs. spy vs. broker vs. proxy)
- The value contains **domain-specific jargon** that a general translator may not recognize (e.g., `"bundle source"`, `"marketplace path stale"`)
- The value contains **UI actions** that need spatial/interaction context (e.g., is "Switch" a noun label or a verb command?)

### Format

```yaml
hooks.installed: "✓ installed"  # Shown when a shell hook is successfully installed
action.retry: "Retry"  # Button label — retry a failed AI agent connection
status.stale: "stale"  # Badge in hooks-status table when the install path is outdated
```

### ⚠️ Mistranslation Risk Table

When writing or reviewing strings, assess the **mistranslation risk** if context is missing. Use this table to prioritize adding comments:

| Risk level | Confidence the translation will go wrong | When to worry | Examples |
|------------|------------------------------------------|---------------|----------|
| 🔴 **Critical** | >90% — the word has a dominant meaning in most languages that is **wrong** for this context | The word is a false friend, or its primary meaning is political, military, or socially sensitive | "Agent" (= spy in many languages), "Execute" (= kill), "Abort" (= politically charged in some cultures), "Master/Slave" (= offensive) |
| 🟠 **High** | 70–90% — the word is ambiguous and translators will likely pick the **more common but wrong** sense | The word has 2+ common meanings and no surrounding sentence to disambiguate | "Terminal" (= airport terminal vs. CLI), "Host" (= party host vs. server), "Shell" (= seashell vs. command shell), "Port" (= harbor vs. network port) |
| 🟡 **Medium** | 40–70% — the word is technical jargon that a **general translator** may not know | Domain-specific terms that don't appear in everyday language | "Hook" (= fishing hook vs. shell hook), "Bundle" (= package vs. software bundle), "Pane" (= window pane vs. UI pane) |
| 🟢 **Low** | <40% — the word is clear enough or already a global loanword | Common tech terms that most translators handle correctly | "Error", "OK", "Settings", "Profile" |

**Rule of thumb:** If a string scores 🔴 or 🟠, a missing context comment is a **bug** — treat it the same as a missing `{Locked}` directive. Fix it before shipping.

### Real-world examples of what goes wrong

| String | Missing context | What happens |
|--------|----------------|--------------|
| `"Agent not found"` | No comment explaining AI agent | zh-TW translated as "proxy not found" (代理), el-GR as "spy not found" (πράκτορας) |
| `"Switch agent"` | No comment explaining it's a UI action | Could be translated as a noun phrase ("agent switch/toggle device") instead of a verb command |
| `"installed"` | No comment explaining it's a hook status | Could be translated as past tense ("was installed") vs. adjective state ("currently installed") |
| `"stale"` | No comment explaining marketplace path | Could be translated as "old bread" or "not fresh" instead of "outdated" in tech sense |

### Applying to existing strings

When adding new strings or reviewing existing ones, scan for any that lack comments and score 🟡 or above. Add context comments early — it is far cheaper to write a 10-word comment now than to debug a politically embarrassing translation error across 85+ locales later.

## Terminology Alignment

Align translations using these sources **in priority order**:

1. **Existing `.resw` translations** in this repo — ensures consistency between C++ and Rust UI
   - `src/cascadia/TerminalApp/Resources/{locale}/Resources.resw`
   - `src/cascadia/TerminalSettingsEditor/Resources/{locale}/Resources.resw`
   - `src/cascadia/TerminalSettingsModel/Resources/{locale}/Resources.resw`
2. **Existing `.yml` translations** in `wta/locales/` — ensures internal consistency within WTA
3. **Microsoft Learn localized documentation** — for new terms not yet in `.resw` or `.yml`, check the official localized docs (`learn.microsoft.com/{locale}/...`) for Microsoft-standard translations

### Process

1. Before translating, check if the term already exists in `.resw` for that locale
2. If it does → use the same translation (consistency across C++ and Rust UI)
3. If not in `.resw`, check if similar terms exist in other `.yml` locale files
4. If still not found → check Microsoft Learn localized docs (`learn.microsoft.com/{locale}/...`)
5. If no established term exists → research community usage (Wikipedia, academic, developer forums)
6. If the native word means something unrelated to the concept → use English or a phonetic transliteration

## Adding or Updating Localized Strings

### Step 0: Discover what needs localization

Before translating, determine which strings are new or changed:

```bash
# New strings: keys in en-US.yml that don't exist in other locale files
# Changed strings: diff en-US.yml against the main branch
git diff main -- wta/locales/en-US.yml
```

If a key was **added** → it must be translated in all locale files.
If a key was **modified** → its translation must be updated in all locale files.
If a key was **removed** → remove it from all locale files.

### Step 1: Determine the target locale set

The locale set is **not hardcoded**. Derive it from the `.resw` locale folders:

```bash
# List all locale folders that .resw uses (this is the authoritative set)
ls src/cascadia/TerminalApp/Resources/
```

Every folder there (except `en-US`) must have a corresponding `wta/locales/{locale}.yml`. If a new `.resw` locale folder was added (e.g., Terminal now supports a new language), create a new `.yml` file for it.

### Step 2: Translate to all locales

The developer has already added/modified strings in `en-US.yml` as part of their feature work. Step 0 discovers those changes. Now translate them:

Use per-language sub-agents that:
1. Read `en-US.yml` for source strings (only the new/changed keys from Step 0)
2. Read existing translations in target locale for tone/style consistency
3. Follow the [Terminology Alignment](#terminology-alignment) process for term choices
4. Follow the [Non-Translatable Token Rules](#non-translatable-token-rules) — locked tokens must appear verbatim
5. Produce translations

### Step 3: QA review

Use a separate reviewer sub-agent that checks:
- **`{Locked}` tokens** preserved verbatim (see [Non-Translatable Token Rules](#non-translatable-token-rules))
- **Terminology** aligned with existing translations (see [Terminology Alignment](#terminology-alignment))
- RTL languages have correct logical string order
- No mojibake or encoding issues

### Step 4: Rebuild

```bash
cd wta
cargo build --target x86_64-pc-windows-msvc
```

Translations are only picked up after rebuild (compile-time codegen).

## Runtime Behavior

- `rust-i18n` detects the OS locale via `sys-locale` crate at startup
- Fallback chain: exact locale → strip territory (e.g., `de-AT` → `de`) → `en-US`
- **MRT parity:** Windows MRT treats `de-DE` as the canonical "German" resource — any `de-*` locale automatically matches. We replicate this behavior via `normalize_locale()` in `main.rs`, which maps unmatched locales to the closest available regional variant (e.g., `de-AT` → `de-DE`) before calling `set_locale()`.
- **Limitation — hardcoded affinity table:** Unlike MRT (which uses a full BCP-47 language distance matrix from Windows), our `normalize_locale()` uses a manually maintained affinity table covering multi-variant languages (zh, en, es, fr, pt, sr). If a new locale is added that introduces a second regional variant for an existing language (e.g., adding `de-CH.yml` alongside `de-DE.yml`), you **must** also update the affinity table in `normalize_locale()` (`wta/src/main.rs`) to specify which unlisted `de-*` regions map to which variant. Without this, the prefix-based fallback (step 3) picks non-deterministically.
- `t!()` macro returns `Cow<'_, str>` — use `.into_owned()` when `String` is needed

## Common Pitfalls

| Issue | Solution |
|-------|----------|
| Translation not appearing at runtime | Rebuild after changing YAML files |
| `t!()` type mismatch | Use `.into_owned()` or `.to_string()` |
| Indic scripts transliterating product name | Keep `{Locked}` tokens in Latin script |
| PowerShell corrupts Unicode when writing YAML | Use Node.js with `\uXXXX` escapes instead |
| Terminology inconsistency with Terminal UI | Check `.resw` for that locale first |
