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

Since YAML has no structured comment system like `.resw`, we use `#` comments with `{Locked}` syntax borrowed from .resw:

### Section-level (applies to all strings in a group)

```yaml
# {Locked="Intelligent Terminal"} - product name, do not translate
setup.title.first_run: "Welcome to Intelligent Terminal!"
setup.title.agent_missing: "Agent not found"
```

### Line-level (applies to one specific string)

```yaml
setup.title.first_run: "Welcome to Intelligent Terminal!"  # {Locked="Intelligent Terminal"}
```

### Rules

- **Section-level** `{Locked}` comment → applies to all strings below until the next section comment
- **Line-level** `{Locked}` comment → applies only to that line
- Multiple locked tokens: `# {Locked="Copilot","CLI"}`
- Full lock (entire value must not be translated): `# {Locked}`
- **Translation rule:** Locked tokens must appear **verbatim** (in English) in all locale files. This matches the `.resw` `{Locked}` convention — see `.github/instructions/localization.instructions.md` for the full list of non-translatable terms.

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
