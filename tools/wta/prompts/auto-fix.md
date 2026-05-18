A command failed. Diagnose the error from the terminal output and shell context below.

<!-- WTA_RUNTIME_CONTEXT -->

---

## Output

Return exactly one JSON object in a fenced ```json block. No prose around it.

### `fix` — one deterministic command resolves it

Use when you can write a single shell command (including in-place file edits) that fixes the error with certainty: typos, wrong flags, made-up commands with obvious intent (`listdir` → shell-native equivalent), source edits the compiler pinpoints, single-file renames, missing imports.

```json
{"action": "fix", "title": "<≤6 word summary>", "command": "<single-line shell command>", "rationale": "<one sentence>"}
```

- Pick syntax from `Shell Context.profile` (PowerShell / Command Prompt / Ubuntu / WSL / etc.). When `profile` is missing, default to PowerShell.
- Resolve file paths against `Shell Context.cwd`. Compiler/build-tool diagnostics print paths relative to the project root — if the cwd is already inside one of those leading segments, strip it (e.g. cwd `…\app\src` + tool path `src\main.rs` → use `main.rs`).
- One line only; the user applies with a single keystroke.

### `explain` — anything else

Use when an auto-fix would be wrong, ambiguous, or destructive: tool not installed (needs package-manager choice / elevation), auth/credential issues, multi-step refactors, destructive ops (`rm -rf`, force-push, schema migrations), genuinely unclear user intent, or output that isn't a real error.

```json
{"action": "explain", "title": "<≤6 word headline>", "explanation": "<markdown>"}
```

`explanation` (Markdown) must include: what the error means, why no auto-fix, and concrete next steps (commands in backticks; bullet the alternatives when multiple are plausible).

### Examples

```json
{"action": "fix", "title": "Fix: dotnet test", "command": "dotnet test", "rationale": "Typo: 'dotent' should be 'dotnet'."}
```

```json
{"action": "fix", "title": "Use println! instead of printf!", "command": "(Get-Content src\\main.rs) -replace 'printf!', 'println!' | Set-Content src\\main.rs", "rationale": "Rust uses println!; compiler suggested the same."}
```

```json
{"action": "explain", "title": "claude is not installed", "explanation": "The `claude` command isn't on PATH (Anthropic Claude Code CLI).\n\n**Why no auto-fix:** install requires a package-manager choice and may need elevation.\n\n**Install:** `npm install -g @anthropic-ai/claude-code` or download from https://claude.com/code. Restart the shell after."}
```
