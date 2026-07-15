//! Local command recall for autofix (issue #287).
//!
//! When a command fails with a "not found" error, the autofix agent used to
//! give generic advice without knowing whether the command even exists on the
//! user's machine — so it never suggested the *local* PowerShell scripts and
//! programs on PATH that the user most likely mistyped.
//!
//! This module computes "did you mean" near-matches grounded in the user's
//! real environment. The flow (PowerShell only in v1) is:
//!
//! 1. [`extract_command_token`] pulls the executable name out of the failing
//!    command line (the first line of the captured `[command + output]`
//!    buffer — see `ControlCore::ReadLastPrompt`, which starts at the FTCS
//!    command mark, so there is no prompt prefix to strip).
//! 2. A cheap in-process `which` pre-gate: if the token resolves as a plain
//!    PATH program, the failure was *not* a not-found, so nothing is injected
//!    and no subprocess is spawned (the common case — failed build/test/git).
//! 3. Otherwise, enumerate the shell's real command list once
//!    (`Get-Command …`) and, if the token still doesn't resolve, rank the
//!    list by Damerau-Levenshtein ([`rank_near_matches`]) to surface the
//!    closest existing commands.
//!
//! The gate is locale-independent: it asks the shell "does this command
//! exist", never matches the (localized) error text. The `which` pre-gate
//! skips the enumerate subprocess only for tokens that resolve as plain PATH
//! programs; a failing cmdlet / alias / function token — which `which` can't
//! see — still spawns the enumerate and then bails out via the existence
//! gate. So the subprocess runs for any token that *looks* not-found to PATH,
//! not only a genuine not-found.
//!
//! Profile-defined aliases/functions (issue #286): the enumerate loads the
//! user's interactive profile first, so an alias set only in `$PROFILE` (e.g.
//! `which` → `where.exe`) is enumerated and recognized. Because a profile runs
//! arbitrary user code that can be slow or block, the profile enumerate is
//! bounded by [`PROFILE_ENUMERATE_TIMEOUT`] and falls back to a fast
//! `-NoProfile` enumerate on timeout/failure (see
//! [`enumerate_powershell_commands`]). Still-uncovered: aliases/functions
//! defined *ad hoc* in the running interactive session (never persisted to a
//! profile) — those live only in that session's memory, which a separate
//! subprocess can't observe.

#[cfg(windows)]
/// `CREATE_NO_WINDOW` — keep the enumerate subprocess from flashing a console
/// window over the TUI. (`tokio::process::Command::creation_flags` is an
/// inherent Windows method, so no `CommandExt` import is needed.)
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Windows executable / script extensions stripped before comparison and
/// display, so `git.exe` reads as `git` and the edit distance stays honest
/// (`gti` vs `git` = one transposition, not three edits against `git.exe`).
const EXE_EXTS: [&str; 6] = [".exe", ".cmd", ".bat", ".com", ".ps1", ".msc"];

/// Max number of near-matches to surface.
const MAX_NEAR_MATCHES: usize = 5;

/// Max time to wait for the profile-loading enumerate before falling back to a
/// `-NoProfile` enumerate. A user's interactive profile (oh-my-posh, module
/// imports, PSReadLine, network calls) can be slow or, worst case, block; the
/// bound keeps near-match recall responsive. The timed-out child is reaped, not
/// leaked, via `kill_on_drop` in [`run_enumerate`].
const PROFILE_ENUMERATE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(4);

/// Marker printed as the first line of the enumerate `-Command`, so any stdout a
/// profile emits (a `Write-Output` on the success stream) during profile load —
/// which happens *before* `-Command` runs — is discarded rather than mistaken
/// for a command name. Unlikely to collide with any real command name.
const ENUM_SENTINEL: &str = "__WTA_CMD_ENUM__";

/// True when `shell` names a PowerShell host. v1 only recalls for PowerShell
/// panes.
///
/// The pane's reported shell comes from one of two sources (see
/// `shell_from_active`), and we must accept both forms:
/// - the **OSC 9001 ShellType** value when shell integration is installed —
///   the *common* case — which is the bare name `pwsh` / `powershell`;
/// - the pid-based process image name fallback, `pwsh.exe` / `powershell.exe`
///   (possibly a full path).
///
/// So match on the leaf with any trailing `.exe` stripped — otherwise the
/// feature silently never runs on the common shell-integration path.
pub fn is_powershell(shell: &str) -> bool {
    let lower = shell.to_ascii_lowercase();
    let leaf = lower.rsplit(['\\', '/']).next().unwrap_or(lower.as_str());
    let leaf = leaf.strip_suffix(".exe").unwrap_or(leaf);
    leaf == "pwsh" || leaf == "powershell"
}

/// Extract the command token (executable name) from a captured
/// `[command + output]` buffer.
///
/// Returns `None` when there is no usable token, or when the (post-`&`) token
/// is an explicit path invocation (`.\x.ps1`, `C:\x.exe`) — a PATH-lookup
/// near-match wouldn't apply to those. A leading PowerShell call operator
/// (`& cmd`, `&cmd`) is peeled first, since `&` still performs normal command
/// resolution on the command it invokes.
pub fn extract_command_token(content: &str) -> Option<String> {
    let first_line = content.lines().map(str::trim).find(|l| !l.is_empty())?;
    let mut tokens = first_line.split_whitespace();
    let mut token = tokens.next()?;
    // PowerShell call operator: `& cmd ...` (or `&cmd`) still performs normal
    // command resolution, so peel a leading `&` and look at the command it
    // invokes — a not-found `& gti` is just as correctable as `gti`.
    if token == "&" {
        token = tokens.next()?;
    } else if let Some(rest) = token.strip_prefix('&') {
        token = rest;
    }
    let token = token.trim_matches(|c| c == '"' || c == '\'');
    // A command chained without whitespace (`gti;git`, `gti|less`, `gti&&echo`)
    // leaves the statement/pipeline separator stuck to the token. Keep only the
    // command name so the existence gate and near-match ranking aren't thrown
    // off by trailing punctuation (command names never contain `;` `|` `&`).
    let token = token.split([';', '|', '&']).next().unwrap_or(token);
    // After peeling `&`, an explicit / relative path is still not a bare PATH
    // command, so a near-match suggestion wouldn't apply.
    if token.is_empty()
        || token.starts_with('.')
        || token.contains('\\')
        || token.contains('/')
    {
        return None;
    }
    Some(token.to_string())
}

/// Strip a trailing Windows executable extension (case-insensitive). Returns
/// the input unchanged when it has no such extension.
pub fn strip_exe_ext(name: &str) -> &str {
    for ext in EXE_EXTS {
        if name.len() <= ext.len() {
            continue;
        }
        let split = name.len() - ext.len();
        // `get` guards the slice boundary: a non-ASCII command name
        // (functions/aliases can be Unicode) could put `split` mid-char,
        // and direct byte slicing would panic and crash prompt assembly.
        if name.get(split..).is_some_and(|tail| tail.eq_ignore_ascii_case(ext)) {
            return &name[..split];
        }
    }
    name
}

/// True when `token` matches a known command `name` (case-insensitive, after
/// extension stripping). Used as the existence gate: a hit means the failure
/// wasn't a not-found, so no near-matches should be injected.
///
/// The token is extension-stripped too, so an explicitly-typed extension
/// (`deploy-it.ps1`) still matches the stripped candidate (`deploy-it`).
pub fn command_exists(token: &str, names: &[String]) -> bool {
    let t = strip_exe_ext(token).to_ascii_lowercase();
    names.iter().any(|n| strip_exe_ext(n).eq_ignore_ascii_case(&t))
}

/// Rank `names` by Damerau-Levenshtein distance to `token`, returning up to
/// [`MAX_NEAR_MATCHES`] closest unique display names (extension-stripped),
/// nearest first, ties broken alphabetically. Anything beyond an adaptive
/// distance threshold is dropped so a wild typo doesn't surface noise.
pub fn rank_near_matches(token: &str, names: &[String], max: usize) -> Vec<String> {
    // Strip the token's own extension so a typed `deploit.ps1` ranks against
    // the stripped candidates on equal footing.
    let t = strip_exe_ext(token).to_ascii_lowercase();
    // Tolerate more edits for longer tokens, but cap at 3 so a long random
    // string doesn't pull in unrelated commands.
    let threshold = (t.chars().count() / 3 + 1).min(3);

    let mut scored: Vec<(usize, u8, String)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let token_sorted = sorted_chars(&t);
    for n in names {
        let display = strip_exe_ext(n);
        let key = display.to_ascii_lowercase();
        if key == t {
            continue; // identical — shouldn't happen post-gate, but be safe
        }
        if !seen.insert(key.clone()) {
            continue; // dedup (e.g. git.exe + git-gui.exe variants, repeats)
        }
        let d = strsim::damerau_levenshtein(&t, &key);
        if d <= threshold {
            // Tie-break: at equal edit distance, a candidate that is an
            // anagram of the token (a pure transposition like `gti`→`git`)
            // is the most likely intended command, so rank it ahead of an
            // equidistant substitution (`gti`→`gci`).
            let anagram_rank: u8 = if sorted_chars(&key) == token_sorted { 0 } else { 1 };
            scored.push((d, anagram_rank, display.to_string()));
        }
    }
    scored.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.cmp(&b.2))
    });
    scored.into_iter().take(max).map(|(_, _, n)| n).collect()
}

/// Lowercase characters of `s` sorted, for cheap anagram comparison.
fn sorted_chars(s: &str) -> Vec<char> {
    let mut v: Vec<char> = s.chars().collect();
    v.sort_unstable();
    v
}

/// Compute local near-matches for `token` when it does not resolve on the
/// user's machine. PowerShell-only (v1).
///
/// Returns `Some(matches)` only when the token is a genuine not-found AND at
/// least one close existing command was found; `None` otherwise (the token
/// exists, or nothing is close enough).
pub async fn powershell_near_matches(shell_exe: &str, token: &str) -> Option<Vec<String>> {
    // Cheap in-process pre-gate: a plain PATH program resolves here without
    // spawning anything, so the common autofix case (a failed build/test/git
    // where the program exists) never pays the enumerate cost.
    if which::which(token).is_ok() {
        return None;
    }

    let names = cached_powershell_commands(shell_exe).await?;

    // Full existence gate: the token may resolve as a cmdlet / function /
    // alias / external `.ps1` that `which` can't see. If so, it wasn't a
    // not-found — inject nothing.
    if command_exists(token, &names) {
        return None;
    }

    let matches = rank_near_matches(token, &names, MAX_NEAR_MATCHES);
    if matches.is_empty() {
        None
    } else {
        Some(matches)
    }
}

/// How a token resolves on the user's machine: its PowerShell command type
/// (`Alias`, `Function`, `Cmdlet`, `Application`, `ExternalScript`, …), the
/// resolved name, and a short target (an alias's target name, or an
/// application/script's full path; empty for cmdlets/functions).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandResolution {
    pub command_type: String,
    pub name: String,
    pub target: String,
}

/// Injection-safe resolver script **template**. The token is passed via the
/// `WTA_RESOLVE_TOKEN` environment variable (never string-interpolated into the
/// command), so a hostile token can't inject PowerShell. `{sentinel}` is
/// substituted with [`ENUM_SENTINEL`] by [`resolve_script`] so the printed
/// marker can never drift from the one [`parse_resolve_output`] keys off. The
/// script prints the sentinel first (so profile stdout noise is separable),
/// then one tab-separated `type<TAB>name<TAB>target` line per `Get-Command
/// -All` result. `target` is whitespace-collapsed so a multi-line function body
/// can't break line parsing.
const RESOLVE_SCRIPT_TEMPLATE: &str = r#"$ErrorActionPreference='SilentlyContinue'
Write-Output '{sentinel}'
Get-Command -Name $env:WTA_RESOLVE_TOKEN -All | ForEach-Object {
  $c = $_
  $d = switch ($c.CommandType) {
    'Alias' { $c.Definition }
    'Application' { $c.Source }
    'ExternalScript' { $c.Source }
    default { '' }
  }
  ($c.CommandType, $c.Name, ($d -replace '\s+',' ')) -join [char]9
}"#;

/// [`RESOLVE_SCRIPT_TEMPLATE`] with the live [`ENUM_SENTINEL`] substituted, so
/// the emitted marker and the parser stay in sync automatically.
fn resolve_script() -> String {
    RESOLVE_SCRIPT_TEMPLATE.replace("{sentinel}", ENUM_SENTINEL)
}

/// Outcome of [`powershell_resolve`]. Distinguishes a clean "the shell ran and
/// the token resolves to nothing" ([`ResolveOutcome::NotFound`]) from "we
/// couldn't determine it" ([`ResolveOutcome::Indeterminate`], e.g. the profile
/// probe timed out or failed to spawn) — so callers never report a false "does
/// not exist" just because a slow/hanging profile blew the timeout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveOutcome {
    /// The token resolves to one or more commands (guaranteed non-empty).
    Resolved(Vec<CommandResolution>),
    /// The probe ran cleanly and the token resolves to nothing.
    NotFound,
    /// Existence couldn't be determined (timeout / spawn / IO error).
    Indeterminate,
}

/// Resolve what `token` actually is on the user's machine (profile-aware).
///
/// Unlike [`powershell_near_matches`] (typo "did you mean" for a *not-found*
/// token), this answers "what is this command" for an *existing* one — the
/// issue #286 scenario where the user asks about a command (`which`) that is a
/// profile-defined alias. The subprocess loads the user's profile (no
/// `-NoProfile`), so profile aliases/functions resolve; a bare `-NoProfile`
/// probe — which the agent tends to run itself — would miss them.
///
/// The profile load is bounded by [`PROFILE_ENUMERATE_TIMEOUT`]. On timeout or
/// spawn/IO failure the result is [`ResolveOutcome::Indeterminate`] (**not**
/// `NotFound`), so a hanging profile can't be mistaken for a missing command.
/// PowerShell-only (v1).
pub async fn powershell_resolve(shell_exe: &str, token: &str) -> ResolveOutcome {
    let exe = if shell_exe.trim().is_empty() {
        "powershell.exe"
    } else {
        shell_exe
    };

    let mut cmd = tokio::process::Command::new(exe);
    cmd.args(["-NonInteractive", "-Command", &resolve_script()])
        .env("WTA_RESOLVE_TOKEN", token)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    // Bound the profile load; on timeout the child is reaped via kill_on_drop.
    // A timeout or spawn/IO error is Indeterminate — the shell may never have
    // reached `Get-Command`, so we cannot conclude the token is absent.
    let output = match tokio::time::timeout(PROFILE_ENUMERATE_TIMEOUT, cmd.output()).await {
        Ok(Ok(output)) => output,
        _ => return ResolveOutcome::Indeterminate,
    };
    match parse_resolve_output(&String::from_utf8_lossy(&output.stdout)) {
        Some(resolutions) => ResolveOutcome::Resolved(resolutions),
        None => ResolveOutcome::NotFound,
    }
}

/// Parse [`RESOLVE_SCRIPT_TEMPLATE`] stdout into resolutions, discarding profile
/// noise before [`ENUM_SENTINEL`]. Pure, so parsing is unit-testable without a
/// shell. Returns `None` when nothing resolves (no data lines after the
/// sentinel).
fn parse_resolve_output(stdout: &str) -> Option<Vec<CommandResolution>> {
    let lines: Vec<&str> = stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    // Use the LAST sentinel, not the first: the real marker is printed by
    // `-Command` *after* the profile has finished loading, so any earlier
    // sentinel-looking line is profile stdout noise and must be skipped past.
    let start = lines
        .iter()
        .rposition(|l| *l == ENUM_SENTINEL)
        .map_or(0, |i| i + 1);
    let resolutions: Vec<CommandResolution> = lines[start..]
        .iter()
        .filter_map(|line| {
            let mut parts = line.split('\t');
            let command_type = parts.next()?.trim().to_string();
            let name = parts.next()?.trim().to_string();
            let target = parts.next().unwrap_or("").trim().to_string();
            if command_type.is_empty() || name.is_empty() {
                None
            } else {
                Some(CommandResolution {
                    command_type,
                    name,
                    target,
                })
            }
        })
        .collect();

    if resolutions.is_empty() {
        None
    } else {
        Some(resolutions)
    }
}
/// current `PATH`. Enumerating the shell costs a profile-loading `pwsh`
/// subprocess (the profile can take up to [`PROFILE_ENUMERATE_TIMEOUT`]); the
/// command set is effectively static for the helper's lifetime, so cache it —
/// the profile cost is paid once per pane, not per query. By design we do NOT
/// detect mid-session installs — a newly added command shows up only after the
/// tab/helper restarts. Keying on `PATH` keeps tests isolated (each sets its
/// own `PATH` → fresh key).
static COMMAND_CACHE: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<Vec<String>>>>,
> = std::sync::OnceLock::new();

/// Cached wrapper over [`enumerate_powershell_commands`]; see [`COMMAND_CACHE`].
async fn cached_powershell_commands(shell_exe: &str) -> Option<std::sync::Arc<Vec<String>>> {
    let cache = COMMAND_CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let key = format!(
        "{}|{}",
        shell_exe.to_ascii_lowercase(),
        std::env::var("PATH").unwrap_or_default()
    );
    if let Some(hit) = cache.lock().ok().and_then(|m| m.get(&key).cloned()) {
        return Some(hit);
    }
    let names = std::sync::Arc::new(enumerate_powershell_commands(shell_exe).await?);
    if let Ok(mut m) = cache.lock() {
        m.insert(key, names.clone());
    }
    Some(names)
}

/// Enumerate the shell's command names (cmdlets, applications, external
/// scripts, functions, aliases).
///
/// Runs the user's interactive profile first so profile-defined aliases and
/// functions are visible (issue #286 — e.g. a `which` → `where.exe` alias set
/// in `$PROFILE`). Because loading a profile runs arbitrary user code that can
/// be slow or block, the profile enumerate is bounded by
/// [`PROFILE_ENUMERATE_TIMEOUT`]; on timeout / failure / empty output it falls
/// back to a `-NoProfile` enumerate (PATH programs, external scripts, cmdlets,
/// and the shell's built-in aliases/functions — issue #287). Cmdlets are
/// included so the existence gate doesn't misclassify a failing cmdlet
/// invocation (e.g. `Get-Item` with a missing path) as a not-found command.
async fn enumerate_powershell_commands(shell_exe: &str) -> Option<Vec<String>> {
    let exe = if shell_exe.trim().is_empty() {
        "powershell.exe"
    } else {
        shell_exe
    };

    // Profile-loading enumerate, time-bounded. On success it already contains
    // the built-in commands too, so it fully supersedes the fallback.
    if let Ok(Some(names)) =
        tokio::time::timeout(PROFILE_ENUMERATE_TIMEOUT, run_enumerate(exe, true)).await
    {
        return Some(names);
    }

    // Fallback (timeout / spawn failure / empty): no profile — always fast and
    // runs no user code. This is the pre-#286 behavior.
    run_enumerate(exe, false).await
}

/// Spawn a single PowerShell enumerate subprocess. With `load_profile == false`
/// it adds `-NoProfile` (fast, no user code); with it `true` the user's profile
/// runs so profile-defined aliases/functions are enumerated. `kill_on_drop`
/// guarantees a profile that hangs past the caller's timeout is reaped when the
/// timed-out future is dropped, never left as an orphaned host.
async fn run_enumerate(exe: &str, load_profile: bool) -> Option<Vec<String>> {
    let mut cmd = tokio::process::Command::new(exe);
    if !load_profile {
        cmd.arg("-NoProfile");
    }
    cmd.args([
        "-NonInteractive",
        "-Command",
        // Print the sentinel first so any profile stdout (emitted during
        // profile load, before this runs) is separated from the command list.
        &format!(
            "Write-Output '{ENUM_SENTINEL}'; \
             Get-Command -CommandType Cmdlet,Application,ExternalScript,Function,Alias | \
             Select-Object -ExpandProperty Name"
        ),
    ])
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::null())
    .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let output = cmd.output().await.ok()?;
    parse_enumerate_output(&String::from_utf8_lossy(&output.stdout))
}

/// Parse the enumerate subprocess stdout into the command-name list, discarding
/// any profile noise printed before [`ENUM_SENTINEL`]. Pure so the noise
/// handling is unit-testable without spawning a shell. Everything up to and
/// including the sentinel is dropped; if the sentinel is somehow absent, every
/// non-empty line is kept rather than dropping the whole result.
fn parse_enumerate_output(stdout: &str) -> Option<Vec<String>> {
    let lines: Vec<&str> = stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    // Use the LAST sentinel, not the first: the real marker is printed by
    // `-Command` *after* the profile has finished loading, so any earlier
    // sentinel-looking line is profile stdout noise and must be skipped past.
    let start = lines
        .iter()
        .rposition(|l| *l == ENUM_SENTINEL)
        .map_or(0, |i| i + 1);
    let names: Vec<String> = lines[start..].iter().map(|s| s.to_string()).collect();

    if names.is_empty() {
        None
    } else {
        Some(names)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn is_powershell_matches_leaf_name_and_full_path() {
        // Process image-name form (pid-based fallback).
        assert!(is_powershell("pwsh.exe"));
        assert!(is_powershell("powershell.exe"));
        assert!(is_powershell(r"C:\Program Files\PowerShell\7\pwsh.exe"));
        assert!(is_powershell("PWSH.EXE")); // case-insensitive
        // OSC 9001 ShellType form (the common shell-integration case) — bare
        // name, no `.exe`. Regressing this silently disables the whole feature.
        assert!(is_powershell("pwsh"));
        assert!(is_powershell("powershell"));
        assert!(is_powershell("PowerShell")); // case-insensitive
        assert!(!is_powershell("bash.exe"));
        assert!(!is_powershell("bash"));
        assert!(!is_powershell("cmd.exe"));
        assert!(!is_powershell("wsl.exe"));
        assert!(!is_powershell("wsl:Ubuntu")); // OSC ShellType for a WSL pane
        assert!(!is_powershell(""));
    }

    #[test]
    fn extract_token_takes_first_token_of_command_line() {
        // Buffer is "command line\n<output>" — the first line is what the
        // user typed; the rest is the (possibly localized) error.
        let buf = "deploit -Target prod\ndeploit: The term 'deploit' is not recognized...";
        assert_eq!(extract_command_token(buf).as_deref(), Some("deploit"));
    }

    #[test]
    fn extract_token_strips_quotes_and_leading_blank_lines() {
        assert_eq!(extract_command_token("\n\n  gti status\n").as_deref(), Some("gti"));
        // A surrounding quote the user typed is stripped from the token.
        assert_eq!(extract_command_token("'gti' foo").as_deref(), Some("gti"));
    }

    #[test]
    fn extract_token_rejects_explicit_paths() {
        // Explicit / relative paths are not PATH lookups, so a near-match
        // suggestion wouldn't apply.
        assert_eq!(extract_command_token(r".\build.ps1"), None);
        assert_eq!(extract_command_token(r"C:\tools\x.exe -a"), None);
        assert_eq!(extract_command_token("/usr/bin/foo"), None);
        assert_eq!(extract_command_token("   "), None);
        assert_eq!(extract_command_token(""), None);
    }

    #[test]
    fn extract_token_peels_powershell_call_operator() {
        // `& cmd` (or `&cmd`) still performs normal command resolution, so a
        // not-found `& gti` is just as correctable — extract the invoked name.
        assert_eq!(extract_command_token("& gti status").as_deref(), Some("gti"));
        assert_eq!(extract_command_token("&gti").as_deref(), Some("gti"));
        assert_eq!(extract_command_token("& 'gti'").as_deref(), Some("gti"));
        // But after the operator, an explicit path is still not a PATH-style
        // lookup → None.
        assert_eq!(extract_command_token(r"& .\build.ps1"), None);
        assert_eq!(extract_command_token(r"& C:\tools\x.exe"), None);
        // A bare `&` with nothing after it is nothing to suggest.
        assert_eq!(extract_command_token("&"), None);
        assert_eq!(extract_command_token("& "), None);
    }

    #[test]
    fn extract_token_strips_chained_separators() {
        // A command chained without whitespace keeps the separator stuck to the
        // token; only the command name should survive so the gate/ranking stay
        // clean.
        assert_eq!(extract_command_token("gti;git status").as_deref(), Some("gti"));
        assert_eq!(extract_command_token("gti| less").as_deref(), Some("gti"));
        assert_eq!(extract_command_token("gti|less").as_deref(), Some("gti"));
        assert_eq!(extract_command_token("gti&&echo done").as_deref(), Some("gti"));
        // Trailing separator with a space still resolves to the bare command.
        assert_eq!(extract_command_token("gti ; git").as_deref(), Some("gti"));
        // A leading separator leaves nothing to suggest.
        assert_eq!(extract_command_token(";foo"), None);
    }

    #[test]
    fn strip_exe_ext_removes_known_extensions_case_insensitively() {
        assert_eq!(strip_exe_ext("git.exe"), "git");
        assert_eq!(strip_exe_ext("Build.CMD"), "Build");
        assert_eq!(strip_exe_ext("deploy-it.ps1"), "deploy-it");
        assert_eq!(strip_exe_ext("git"), "git"); // no extension
        assert_eq!(strip_exe_ext("a.exe"), "a");
        assert_eq!(strip_exe_ext(".exe"), ".exe"); // not longer than the ext
    }

    #[test]
    fn strip_exe_ext_does_not_panic_on_non_ascii_boundary() {
        // A multi-byte char can place `len - ext.len()` mid-character; the
        // boundary-checked slice must return the name unchanged, not panic.
        assert_eq!(strip_exe_ext("€€"), "€€");
        assert_eq!(strip_exe_ext("café"), "café");
        // A real extension after a non-ASCII prefix still strips cleanly.
        assert_eq!(strip_exe_ext("café.exe"), "café");
    }

    #[test]
    fn command_exists_when_token_carries_explicit_extension() {
        // User typed the extension explicitly; the gate must still match the
        // stripped candidate so it isn't misreported as not-found.
        let cmds = names(&["deploy-it.ps1", "git.exe"]);
        assert!(command_exists("deploy-it.ps1", &cmds));
        assert!(command_exists("GIT.EXE", &cmds));
    }

    #[test]
    fn rank_strips_token_extension_before_ranking() {
        // `deploit.ps1` should rank against `deploy-it` as if the extension
        // weren't typed — distance is measured on the base names.
        let cmds = names(&["deploy-it.ps1", "deploy.exe", "git.exe"]);
        let got = rank_near_matches("deploit.ps1", &cmds, 5);
        assert!(
            got.contains(&"deploy-it".to_string()),
            "expected deploy-it among near-matches, got {got:?}"
        );
    }

    #[test]
    fn command_exists_is_case_insensitive_and_extension_aware() {
        let cmds = names(&["git.exe", "Get-Item", "deploy-it.ps1"]);
        assert!(command_exists("git", &cmds));
        assert!(command_exists("GIT", &cmds));
        assert!(command_exists("get-item", &cmds));
        assert!(command_exists("deploy-it", &cmds));
        assert!(!command_exists("deploit", &cmds));
    }

    #[test]
    fn rank_suggests_git_for_transposition_typo() {
        // The canonical CLI typo: adjacent transposition. Damerau-Levenshtein
        // ranks `git` at distance 1, so it must be the top suggestion.
        let cmds = names(&["git.exe", "gh.exe", "gci", "Get-Item", "where.exe"]);
        let got = rank_near_matches("gti", &cmds, 5);
        assert_eq!(got.first().map(String::as_str), Some("git"));
    }

    #[test]
    fn rank_suggests_local_script_for_typo() {
        // The issue's core case: a local PATH script the user mistyped.
        let cmds = names(&["deploy-it.ps1", "deploy-iis.exe", "deploy.exe", "git.exe"]);
        let got = rank_near_matches("deploit", &cmds, 5);
        assert!(
            got.contains(&"deploy-it".to_string()),
            "expected deploy-it among near-matches, got {got:?}"
        );
    }

    #[test]
    fn rank_prefers_transposition_over_equidistant_substitution() {
        // `gti` is distance 1 from both `git` (transposition) and `gci`
        // (substitution). The anagram tie-break must rank the transposition
        // first — it's the far more likely intended command.
        let cmds = names(&["gci", "git.exe", "gco"]);
        let got = rank_near_matches("gti", &cmds, 5);
        assert_eq!(got.first().map(String::as_str), Some("git"));
    }

    #[test]
    fn rank_returns_empty_for_a_wild_unrelated_typo() {
        // A long random string must not pull in unrelated commands — the
        // adaptive threshold rejects everything.
        let cmds = names(&["git.exe", "cargo.exe", "dotnet.exe", "Get-Item"]);
        assert!(rank_near_matches("xqzwvbnmlkjh", &cmds, 5).is_empty());
    }

    #[test]
    fn rank_dedups_and_caps_at_max() {
        // Duplicate display names (git.exe + git) collapse; result honors max.
        let cmds = names(&["git.exe", "git", "gid", "gut", "got", "gtt", "gib"]);
        let got = rank_near_matches("gut", &cmds, 3);
        assert!(got.len() <= 3, "must cap at max, got {got:?}");
        let mut sorted = got.clone();
        sorted.dedup();
        assert_eq!(sorted.len(), got.len(), "must not contain duplicates: {got:?}");
    }

    #[test]
    fn parse_output_strips_profile_noise_before_sentinel() {
        // A profile that writes to the success stream (`Write-Output`) prints
        // before the sentinel; those lines must not be mistaken for commands.
        // Built with join() so the source has no `\n`-glued tokens.
        let raw = [
            "Loading my profile...",
            "oh-my-posh init noise",
            ENUM_SENTINEL,
            "which",
            "Get-ChildItem",
            "git",
        ]
        .join("\n");
        let got = parse_enumerate_output(&raw).expect("names after the sentinel");
        assert_eq!(got, names(&["which", "Get-ChildItem", "git"]));
    }

    #[test]
    fn parse_output_keeps_all_lines_when_sentinel_absent() {
        // Defensive: a missing sentinel must not drop the whole command list.
        let raw = ["git", "Get-ChildItem"].join("\n");
        let got = parse_enumerate_output(&raw).expect("all lines kept");
        assert_eq!(got, names(&["git", "Get-ChildItem"]));
    }

    #[test]
    fn parse_output_none_when_only_sentinel() {
        // Sentinel present but no commands after it → nothing to offer.
        assert!(parse_enumerate_output(ENUM_SENTINEL).is_none());
    }

    #[test]
    fn parse_output_uses_last_sentinel_when_noise_contains_one() {
        // A profile that echoes the sentinel string as stdout noise must not
        // fool the parser: the real marker is the LAST one, printed after the
        // profile loads. Everything up to and including it (incl. the fake) is
        // dropped, so the fake sentinel never leaks in as a command name.
        let raw = [
            "profile prints the marker as noise:",
            ENUM_SENTINEL,
            "still profile noise",
            ENUM_SENTINEL,
            "git",
            "Get-ChildItem",
        ]
        .join("\n");
        let got = parse_enumerate_output(&raw).expect("names after the last sentinel");
        assert_eq!(got, names(&["git", "Get-ChildItem"]));
    }

    #[test]
    fn parse_resolve_output_parses_tab_rows_after_sentinel() {
        // Rows built with join("\t") so the source has no `\t`-glued tokens.
        let alias_row = ["Alias", "which", "where.exe"].join("\t");
        let app_row = [
            "Application",
            "where.exe",
            "C:\\Windows\\system32\\where.exe",
        ]
        .join("\t");
        let raw = ["profile noise line", ENUM_SENTINEL, &alias_row, &app_row].join("\n");
        let got = parse_resolve_output(&raw).expect("resolutions after sentinel");
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], CommandResolution {
            command_type: "Alias".into(),
            name: "which".into(),
            target: "where.exe".into(),
        });
        assert_eq!(got[1].command_type, "Application");
        assert_eq!(got[1].target, "C:\\Windows\\system32\\where.exe");
    }

    #[test]
    fn parse_resolve_output_none_when_no_rows() {
        // Sentinel only (token didn't resolve) → None.
        assert!(parse_resolve_output(ENUM_SENTINEL).is_none());
    }

    #[test]
    fn parse_resolve_output_uses_last_sentinel_when_noise_contains_one() {
        // Same last-sentinel guarantee as the enumerate parser: a profile that
        // echoes the sentinel as noise must not shift the data window.
        let row = ["Alias", "which", "where.exe"].join("\t");
        let raw = [ENUM_SENTINEL, "profile noise", ENUM_SENTINEL, &row].join("\n");
        let got = parse_resolve_output(&raw).expect("resolutions after the last sentinel");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "which");
        assert_eq!(got[0].target, "where.exe");
    }

    #[test]
    fn parse_resolve_output_tolerates_missing_target_column() {
        // Cmdlets/functions have no target column; a two-field row is still valid.
        let raw = [ENUM_SENTINEL, &["Cmdlet", "Get-ChildItem"].join("\t")].join("\n");
        let got = parse_resolve_output(&raw).expect("one resolution");
        assert_eq!(got[0].command_type, "Cmdlet");
        assert_eq!(got[0].name, "Get-ChildItem");
        assert_eq!(got[0].target, "");
    }
}

/// Integration tests that spawn a **real** PowerShell host to exercise the
/// subprocess-backed paths ([`enumerate_powershell_commands`] and
/// [`powershell_near_matches`]) end-to-end. The pure-function unit tests above
/// can't cover these because the behaviour depends on the live shell.
///
/// They are Windows-only and skip themselves (no-op `return`) when no
/// PowerShell host is installed, so a bare CI image without `pwsh`/`powershell`
/// doesn't fail — it just doesn't exercise them.
#[cfg(all(test, windows))]
mod integration_tests {
    use super::*;
    use std::sync::Mutex;

    /// Serializes the test(s) that mutate the process-wide `PATH`. Cargo runs
    /// unit tests on parallel threads inside one process, so an unguarded
    /// `set_var("PATH", …)` could race a concurrent test that reads it.
    static PATH_GUARD: Mutex<()> = Mutex::new(());

    /// First installed PowerShell host, or `None` to skip the test.
    fn powershell_host() -> Option<String> {
        ["pwsh.exe", "powershell.exe"]
            .into_iter()
            .find(|exe| which::which(exe).is_ok())
            .map(String::from)
    }

    /// First PowerShell host resolvable by its **bare** name (no `.exe`) — the
    /// form `shell_from_active` reports from the OSC 9001 ShellType in the
    /// common shell-integration case.
    fn powershell_host_bare() -> Option<String> {
        ["pwsh", "powershell"]
            .into_iter()
            .find(|exe| which::which(exe).is_ok())
            .map(String::from)
    }

    #[tokio::test]
    async fn enumerate_returns_builtin_cmdlets() {
        let Some(shell) = powershell_host() else {
            eprintln!("no PowerShell host installed; skipping");
            return;
        };
        let names = enumerate_powershell_commands(&shell)
            .await
            .expect("enumerate should return a non-empty command list");
        // `Get-ChildItem` is present in every PowerShell host, profile or not.
        assert!(
            names.iter().any(|n| n.eq_ignore_ascii_case("Get-ChildItem")),
            "expected the Get-ChildItem cmdlet in the enumerated list"
        );
    }

    #[tokio::test]
    async fn enumerate_accepts_a_bare_osc_shell_name() {
        // In the common case `shell_from_active` reports the OSC 9001 ShellType
        // (`pwsh` / `powershell`, no `.exe`). The enumerate subprocess must
        // still spawn from that bare name — proving the whole near-match path
        // works on the shell-integration path, not just the `.exe` fallback.
        let Some(shell) = powershell_host_bare() else {
            eprintln!("no bare PowerShell host on PATH; skipping");
            return;
        };
        let names = enumerate_powershell_commands(&shell)
            .await
            .expect("enumerate should spawn from a bare shell name");
        assert!(
            names.iter().any(|n| n.eq_ignore_ascii_case("Get-ChildItem")),
            "expected Get-ChildItem from a bare-name enumerate"
        );
    }

    #[tokio::test]
    async fn near_matches_none_for_a_real_builtin_alias() {
        let Some(shell) = powershell_host() else {
            eprintln!("no PowerShell host installed; skipping");
            return;
        };
        // `gci` is a built-in alias for Get-ChildItem. `which` can't see it
        // (it's not a PATH file), so the in-process pre-gate passes — but the
        // full enumerate gate must still recognize it and suppress injection.
        let got = powershell_near_matches(&shell, "gci").await;
        assert!(got.is_none(), "expected None for the real alias `gci`, got {got:?}");
    }

    #[tokio::test]
    async fn resolve_reports_a_real_builtin_alias() {
        let Some(shell) = powershell_host() else {
            eprintln!("no PowerShell host installed; skipping");
            return;
        };
        // `gci` → Alias for Get-ChildItem. Profile-loaded resolve must report it
        // as an Alias with a non-empty target (the #286 "what is X" answer).
        let ResolveOutcome::Resolved(got) = powershell_resolve(&shell, "gci").await else {
            panic!("gci should resolve");
        };
        assert!(
            got.iter().any(|r| r.command_type.eq_ignore_ascii_case("Alias")
                && r.name.eq_ignore_ascii_case("gci")
                && !r.target.is_empty()),
            "expected gci as an Alias with a target, got {got:?}"
        );
    }

    #[tokio::test]
    async fn resolve_not_found_for_unknown_token() {
        let Some(shell) = powershell_host() else {
            eprintln!("no PowerShell host installed; skipping");
            return;
        };
        let got = powershell_resolve(&shell, "no-such-command").await;
        assert_eq!(
            got,
            ResolveOutcome::NotFound,
            "expected NotFound for a nonexistent command, got {got:?}"
        );
    }

    #[tokio::test]
    async fn near_matches_suggests_a_local_script_typo_on_path() {
        let Some(shell) = powershell_host() else {
            eprintln!("no PowerShell host installed; skipping");
            return;
        };

        // Drop a uniquely-named script into a fresh dir and put it on PATH. The
        // enumerate subprocess inherits this process's PATH, so it sees the
        // file as an ExternalScript — mirroring a user's own local PATH script
        // (the core issue #287 scenario).
        let _guard = PATH_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("cmdrecall_it_{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp script dir");
        std::fs::write(dir.join("wtdeployit.ps1"), "Write-Host hi").expect("write script");

        let original_path = std::env::var_os("PATH");
        let mut prepended = std::ffi::OsString::from(&dir);
        if let Some(existing) = &original_path {
            prepended.push(";");
            prepended.push(existing);
        }
        std::env::set_var("PATH", &prepended);

        // `wtdeployt` is the script name with one character dropped — a genuine
        // not-found whose closest existing command is the script itself.
        let result = powershell_near_matches(&shell, "wtdeployt").await;

        // Always restore PATH and remove the temp dir *before* asserting, so a
        // failed assertion can never leak state into other tests.
        match original_path {
            Some(p) => std::env::set_var("PATH", p),
            None => std::env::remove_var("PATH"),
        }
        let _ = std::fs::remove_dir_all(&dir);

        let matches = result.expect("expected near-matches for a local script typo");
        assert!(
            matches.iter().any(|m| m.eq_ignore_ascii_case("wtdeployit")),
            "expected the local script `wtdeployit` among near-matches, got {matches:?}"
        );
    }
}
