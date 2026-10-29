---
name: ensure-localization
description: 'Inspect, author, repair, and review customer-facing RESW and localization YAML resources. Use shared product terminology, translator context, lock annotations, and six file-based checks; the caller supplies the goal and scope.'
---

# Ensure Localization

Use this shared procedure for customer-facing localization. The caller supplies
the goal, scope, and permitted edits; PR or workflow context is not required.

## Product consistency

Keep terminology consistent within each locale and across RESW and localization
YAML for the same product. Grammar may vary with context, but established product
keywords should not alternate between unrelated translations.

## Language-quality acceptance

For every scoped customer-facing value in a real non-English locale, the final
text must actually be in that locale's target language. Copying the English
source or leaving a provisional English fallback is not a repair, even when all
six checks pass. English locales, source-authoritative locks, and legitimate
identical names or technical terms are allowed exceptions; translate the
unlocked surrounding words.

## Caller context

The caller owns repository or PR context, trusted revisions, skip policy,
automation allowlists, and safe-output rules. This skill owns only the reusable
file-based localization procedure.

## Resources

- File checker: [`./scripts/localization_checks.ps1`](./scripts/localization_checks.ps1)

## Supported files

The skill applies to localization resources regardless of their location. The
checker supports `.resw` and flat locale `.yml` files; other YAML structures need
an appropriate parser, not conversion merely to satisfy this checker.

Discover source and target files using the caller's context and existing layout.
In this repository, `en-US` files and direct resource files are common source
authorities; these are conventions, not mandatory checker paths.

Do not edit source-authority files just to make translations pass.

## Source-authoring guidance

- Add translator context when wording is ambiguous or depends on UI usage, such
  as a noun versus a command. Explain the meaning and placeholder roles; avoid
  boilerplate comments for obvious strings.
- Use `{Locked}` when the whole value must remain literal, and
  `{Locked="token"}` for invariant parts such as product names, protocol names,
  commands, or paths. Use locale-scoped locks only when the exception genuinely
  applies to those locales; do not exempt entire pseudo-locales by default.
- Put guidance in the resource's `<comment>` for RESW or its associated YAML
  comment. Preserve annotations in target files where the format requires them.
- When source edits are not permitted, report missing or contradictory source
  guidance instead of silently changing source files or inventing meaning.
  Mechanical checks preserve declared locks; they cannot decide which
  annotations a new string needs.

## Six checks

| Check | Required args | Purpose |
| --- | --- | --- |
| `Test-ResourceSyntax` | `-File` | Parse supported XML or flat locale YAML shape |
| `Test-ResourceEncoding` | `-File` (`-OriginalFile` optional) | Enforce UTF-8 and expected BOM behavior |
| `Test-RequiredKeys` | `-SourceFile -TargetFile` (`-Keys` optional for PowerShell callers; CLI uses `-KeysJson` when scoping) | Missing and stale key parity |
| `Test-PlaceholderParity` | `-SourceFile -TargetFile` (`-Keys` optional for PowerShell callers; CLI uses `-KeysJson` when scoping) | Placeholder identity and count parity |
| `Test-LockedContent` | `-SourceFile -TargetFile` (`-Locale` sometimes required, `-Keys` optional for PowerShell callers; CLI uses `-KeysJson` when scoping) | Preserve locked values and tokens |
| `Test-PseudoLocale` | `-SourceFile -TargetFile -Locale` (`-Keys` optional for PowerShell callers; CLI uses `-KeysJson` when scoping) | Catch English fallback and pseudo-style mistakes |

Example commands:

```powershell
pwsh .github/skills/ensure-localization/scripts/localization_checks.ps1 -Check Test-ResourceSyntax -File <file>
pwsh .github/skills/ensure-localization/scripts/localization_checks.ps1 -Check Test-ResourceEncoding -File <file> [-OriginalFile <original-file>]
pwsh .github/skills/ensure-localization/scripts/localization_checks.ps1 -Check Test-RequiredKeys -SourceFile <source-file> -TargetFile <target-file> [-KeysJson '["key1","key2"]']
pwsh .github/skills/ensure-localization/scripts/localization_checks.ps1 -Check Test-PlaceholderParity -SourceFile <source-file> -TargetFile <target-file> [-KeysJson '["key1","key2"]']
pwsh .github/skills/ensure-localization/scripts/localization_checks.ps1 -Check Test-LockedContent -SourceFile <source-file> -TargetFile <target-file> [-Locale <locale>] [-KeysJson '["key1","key2"]']
pwsh .github/skills/ensure-localization/scripts/localization_checks.ps1 -Check Test-PseudoLocale -SourceFile <source-file> -TargetFile <target-file> -Locale <pseudo-locale> [-KeysJson '["key1","key2"]']
```

The script writes one JSON bundle to stdout and exits with the actual status
mapping:

- `0` → `PASS`
- `20` → `FIXABLE`
- `30` → `BLOCKED`
- `64` → `INVALID_INPUT`

## Batched checks

For larger scopes, use one `pwsh` process per batch: dot-source
`localization_checks.ps1` once and call its public functions directly with
native PowerShell arguments. Assess the actual returned statuses and findings;
do not fabricate results or generate another checker.

Run initial checks before repairs and rerun the affected checks after repairs.
For a read-only review, reuse successful check results instead of repeating the
same batch. Inspect summaries and non-PASS findings rather than printing every
bundle into the model context. No separate aggregate checker-report file is
required.

The agent owns scope completeness and language judgment. Mechanical checks
support that assessment; they cannot establish that every affected resource
was discovered or that a translation is correct.

## Reusable procedure

1. Read the exact original localization patch content against the
   caller-supplied immutable endpoints before choosing keys or counterpart
   files. Use:
   `git diff --no-ext-diff --unified=3 <comparison-base> <immutable-head> -- <resource paths>`.
   Supporting summaries such as `git diff --stat`, `--name-only`,
   `--name-status`, `--numstat`, `git status`, or a worktree-only
   `git diff -- <file>` are useful for discovery but are not sufficient to
   derive scoped keys, values, or review coverage. If the diff output is
   truncated, continue fetching the remaining hunks until you have read the
   full patch for every relevant resource file. Derive the precise added or
   updated source keys, values, and surrounding context from that exact patch;
   do not infer scope from file prefixes, neighboring samples, unchanged source
   lines, or PR summaries.
   - Source-authority add or update: expand to every shipped localized
     counterpart that should carry the affected file or keys, even when those
     target files are unchanged in the git diff.
   - Target-only edit: inspect that localized target against its source
     authority.
   - Source key or file removal: inspect obsolete shipped counterparts with
     ordinary git and file review. Handle removal or cleanup explicitly instead
     of claiming `PASS` because no file-pair check ran.
2. Map each localized target to its source-authority file. In this repository,
   `.resw` targets map to `.../Resources/en-US/*.resw` or direct
   `.../Resources/*.resw`, and WTA locale targets map to
   `tools/wta/locales/en-US.yml`.
3. Before translating changed terms, inspect existing translations in the same
   locale across both `.resw` files and `tools/wta/locales/*.yml`. Reuse
   established product terminology when the meaning and UI context match. If no
   repository precedent exists, prefer Microsoft localized terms and flag
   conflicting existing translations instead of inventing synonyms or rewriting
   unrelated strings.
4. Use `-Keys` only for native PowerShell function calls, or `-KeysJson` when
   invoking the checker script from the CLI. Optional key scoping exists to
   avoid fixing unrelated old debt, not to hide source additions that the
   procedure already put in scope. Omit the scope parameter entirely for a
   whole-file check.
5. Run `Test-ResourceSyntax` and `Test-ResourceEncoding` on every file you
   actually inspect.
6. For each localized target, run `Test-RequiredKeys` against the full scoped
   source-present change list, then run `Test-PlaceholderParity`,
   `Test-LockedContent`, and, for pseudo-locales, `Test-PseudoLocale` only for
   keys that are present in both the source-authority file and that target.
   If a scoped key is missing on either side, keep the `Test-RequiredKeys`
   finding and do not force the dependent checks across that absent entry. If
   the source key or file was removed, handle that cleanup with step 1's
   explicit review instead of treating the deleted source name as a scoped
   `-Keys` item. If no comparable keys remain for a scoped target pair, skip
   those dependent checks for that pair.
7. Same-repo repair: run the checks before editing, repair only localized
   targets, preserve the original patch-derived scope through the final rerun,
   recompute the comparable key subset from the final on-disk source/target
   files after every edit, include any newly translated source-added keys in
   that rebuilt comparable set, rerun the same relevant checks, then finish
   with one independent read-only review before requesting any branch write.
   Do not swap in a different guessed key set or reuse a stale pre-edit
   comparable subset after a missing key has been added.
8. Read-only review or fork guidance: stay read-only; cover every scoped target,
   including real locales, not only pseudo-locales. Assess actual target-language
   content rather than only key parity, parent-key lists, or checker bundles;
   reject obvious English sentences, stubs, and provisional fallbacks unless a
   declared exception applies. Report only the actual `PASS`, `FIXABLE`,
   `BLOCKED`, or `INVALID_INPUT` outcomes plus concise human review.

## Gotchas

- The public checker API is file-only. Do not pass PR numbers, SHAs, repo
  names, comparison-base discovery, or workflow `Mode` values.
- `Test-RequiredKeys` can report missing entries and stale keys for a provided
  source/target file pair, but it does not discover which untouched locale
  files or deleted source-file counterparts must be inspected. The caller or
  agent must do that repository discovery.
- A clean working tree or a bare `git diff -- <file>` can hide the authored
  base-to-head change set you are supposed to review. Always inspect the full
  `<comparison-base> <immutable-head>` patch when deriving scope.
- `Test-SourceUnchanged`, `Gate`, and `Validate` are intentionally gone. Skip
  decisions such as “formatting-only, do not spend AI” belong to the
  caller/workflow.
- `Test-ResourceEncoding` is the only check that reasons about BOM
  preservation. Keep source-preservation and safe-write policy in the caller,
  and supply `-OriginalFile` whenever a comparison snapshot exists.
- When batching in one process, carry forward genuine pre-edit snapshot paths
  for existing files. If a file has no snapshot, omit `OriginalFile` rather
  than pointing it at the rewritten file.
- When the caller scopes keys, do not automatically reuse that same list for
  `Test-PlaceholderParity`, `Test-LockedContent`, or `Test-PseudoLocale`.
  Those APIs deliberately return `BLOCKED` when a scoped key is absent from the
  source or target file. Use the full scoped list for `Test-RequiredKeys`, then
  derive a comparable-only subset for the dependent checks.
- Do not put source-removed names into scoped `-Keys` just to chase stale
  target entries. That is explicit review-and-cleanup work from step 1. A
  source-absent scoped key is a genuine `BLOCKED` input, not a fixable stale
  entry.
- If the entire scoped source/target file pair was removed and no current file
  remains to feed the checker, materialize immutable pre-deletion snapshots for
  those exact files and run syntax and encoding checks on the snapshots.
  Treat the resulting bundles as historical evidence only, and separately use
  git inspection to confirm that the live tree really removed the files.
- After repairing a previously missing scoped key, recompute that
  comparable-only subset from the final on-disk files before the final rerun.
  A stale pre-edit empty comparable subset can incorrectly skip
  placeholder, lock, or pseudo-locale validation for the new translated
  source-added entry.
- If your tool output truncates the original patch, keep reading until every
  localization hunk has been seen. Never guess key scope from file names,
  prefixes, or partial excerpts.
- For in-process batching, use the dot-sourced public functions with a real
  PowerShell key array such as `@('key1', 'key2')`. `-KeysJson` is only for
  `pwsh -File` CLI calls.
- When you invoke `localization_checks.ps1` from `pwsh -File`, use `-KeysJson`
  for scoped keys. `-Keys key1,key2` is ambiguous at the CLI layer and is not
  the supported script interface.
- A source-only, blocked, or excluded-file result is not a successful repair.
  Comment or fail honestly instead of pretending `PASS`.
