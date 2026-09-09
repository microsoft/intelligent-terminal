//! On-demand local command resolution and near-match recall.
//!
//! When a command fails with a "not found" error, the autofix agent used to
//! give generic advice without knowing whether the command even exists on the
//! user's machine — so it never suggested the *local* PowerShell scripts and
//! programs on PATH that the user most likely mistyped.
//!
//! `wta resolve-command` calls this module when the agent requests local
//! evidence. Prompt construction never invokes it. Near-match lookup checks
//! PATH, then enumerates PowerShell commands and ranks missing names by
//! Damerau-Levenshtein ([`rank_near_matches`]). Enumeration is uncached so
//! each explicit query observes the current environment.
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

#[cfg(test)]
pub(crate) mod probe_observer {
    use std::cell::RefCell;
    use std::marker::PhantomData;
    use std::rc::Rc;

    thread_local! {
        static ATTEMPTS: RefCell<Option<Vec<(&'static str, bool)>>> = const { RefCell::new(None) };
    }

    /// Observe only this test thread. Async callers must use a current-thread
    /// runtime (and LocalSet for spawned tasks), never spawn_blocking.
    pub(crate) struct ProbeObserver(PhantomData<Rc<()>>);

    impl ProbeObserver {
        pub(crate) fn start() -> Self {
            ATTEMPTS.with(|attempts| {
                let mut attempts = attempts.borrow_mut();
                assert!(attempts.is_none(), "probe observer scopes must not overlap");
                *attempts = Some(Vec::new());
            });
            Self(PhantomData)
        }

        pub(crate) fn attempts(&self) -> Vec<(&'static str, bool)> {
            ATTEMPTS.with(|attempts| attempts.borrow().as_ref().unwrap().clone())
        }
    }

    impl Drop for ProbeObserver {
        fn drop(&mut self) {
            ATTEMPTS.with(|attempts| *attempts.borrow_mut() = None);
        }
    }

    pub(super) fn record(operation: &'static str, load_profile: bool) {
        ATTEMPTS.with(|attempts| {
            if let Some(attempts) = attempts.borrow_mut().as_mut() {
                attempts.push((operation, load_profile));
            }
        });
    }
}

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
        // and direct byte slicing would panic and crash command resolution.
        if name
            .get(split..)
            .is_some_and(|tail| tail.eq_ignore_ascii_case(ext))
        {
            return &name[..split];
        }
    }
    name
}

/// True when `token` matches a known command `name` (case-insensitive, after
/// extension stripping). Used as the existence gate: a hit means the failure
/// wasn't a not-found, so no near-matches should be returned.
///
/// The token is extension-stripped too, so an explicitly-typed extension
/// (`deploy-it.ps1`) still matches the stripped candidate (`deploy-it`).
pub fn command_exists(token: &str, names: &[String]) -> bool {
    let t = strip_exe_ext(token).to_ascii_lowercase();
    names
        .iter()
        .any(|n| strip_exe_ext(n).eq_ignore_ascii_case(&t))
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
            let anagram_rank: u8 = if sorted_chars(&key) == token_sorted {
                0
            } else {
                1
            };
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
    // A plain PATH program needs no enumeration.
    if which::which(token).is_ok() {
        return None;
    }

    let names = enumerate_powershell_commands(shell_exe).await?;

    // Full existence gate: the token may resolve as a cmdlet / function /
    // alias / external `.ps1` that `which` can't see. If so, it wasn't a
    // not-found — return no suggestions.
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
/// token is `WildcardPattern::Escape`d before `Get-Command -Name`, so wildcard
/// metacharacters (`* ? [ ]`) are matched literally rather than expanding to a
/// large command set (which would falsely report `exists` and dump huge
/// output). The script prints the sentinel first (so profile stdout noise is
/// separable), then one tab-separated `type<TAB>name<TAB>target` line per
/// `Get-Command -All` result. `target` is whitespace-collapsed so a multi-line
/// function body can't break line parsing.
const RESOLVE_SCRIPT_TEMPLATE: &str = r#"$ErrorActionPreference='SilentlyContinue'
Write-Output '{sentinel}'
$n = [System.Management.Automation.WildcardPattern]::Escape($env:WTA_RESOLVE_TOKEN)
Get-Command -Name $n -All | ForEach-Object {
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
    #[cfg(test)]
    probe_observer::record("resolve", true);
    tracing::debug!(
        target: "wta.command_resolution",
        operation = "resolve",
        load_profile = true,
        shell_exe = exe,
        "powershell_command_probe_started"
    );
    let output = match tokio::time::timeout(PROFILE_ENUMERATE_TIMEOUT, cmd.output()).await {
        Ok(Ok(output)) => output,
        _ => return ResolveOutcome::Indeterminate,
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Distinguish "the probe ran and found nothing" from "the probe never ran"
    // using the SENTINEL, not the exit code: `-Command` prints the sentinel only
    // AFTER the profile loads, so its absence means the profile aborted / the
    // host died before `Get-Command` — Indeterminate, not a false NotFound. The
    // exit code can't tell these apart, because a genuinely not-found token
    // *also* makes pwsh exit non-zero.
    if !stdout.lines().any(|l| l.trim() == ENUM_SENTINEL) {
        return ResolveOutcome::Indeterminate;
    }
    match parse_resolve_output(&stdout) {
        Some(resolutions) => ResolveOutcome::Resolved(resolutions),
        None => ResolveOutcome::NotFound,
    }
}

/// Parse [`RESOLVE_SCRIPT_TEMPLATE`] stdout into resolutions, discarding profile
/// noise before [`ENUM_SENTINEL`]. Pure, so parsing is unit-testable without a
/// shell.
///
/// The resolve command always prints the sentinel first, so its **absence**
/// means the probe never completed — returns `None` (consistent with
/// [`parse_enumerate_output`] and this function's documentation) rather than
/// parsing profile error text as bogus resolutions. When present, the **last**
/// occurrence wins (any earlier one is profile stdout noise); `None` is also
/// returned when the sentinel is present but no data rows follow it.
fn parse_resolve_output(stdout: &str) -> Option<Vec<CommandResolution>> {
    let lines: Vec<&str> = stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let sentinel_idx = lines.iter().rposition(|l| *l == ENUM_SENTINEL)?;
    let resolutions: Vec<CommandResolution> = lines[sentinel_idx + 1..]
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

    #[cfg(test)]
    probe_observer::record("enumerate", load_profile);
    tracing::debug!(
        target: "wta.command_resolution",
        operation = "enumerate",
        load_profile,
        shell_exe = exe,
        "powershell_command_probe_started"
    );
    let output = cmd.output().await.ok()?;
    parse_enumerate_output(&String::from_utf8_lossy(&output.stdout))
}

/// Parse the enumerate subprocess stdout into the command-name list, discarding
/// any profile noise printed before [`ENUM_SENTINEL`]. Pure so the noise
/// handling is unit-testable without spawning a shell.
///
/// The enumerate command always prints the sentinel first, so its **absence**
/// means the subprocess never completed the enumerate (e.g. the profile aborted
/// before `-Command` ran) — treated as a failed enumerate (`None`) so the
/// caller falls back (e.g. to a `-NoProfile` enumerate) instead of parsing
/// profile error text as bogus command names. When the sentinel is present, the
/// **last** occurrence wins (any earlier one is profile stdout noise).
fn parse_enumerate_output(stdout: &str) -> Option<Vec<String>> {
    let lines: Vec<&str> = stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let sentinel_idx = lines.iter().rposition(|l| *l == ENUM_SENTINEL)?;
    let names: Vec<String> = lines[sentinel_idx + 1..]
        .iter()
        .map(|s| s.to_string())
        .collect();

    if names.is_empty() {
        None
    } else {
        Some(names)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn autofix_probe_observer_detects_real_query_attempts() {
        let observer = probe_observer::ProbeObserver::start();
        // A child of an existing executable file cannot be a shell. Exercise
        // actual spawn failures deterministically, without loading user profiles.
        let missing_shell = std::env::current_exe().unwrap().join("not-a-shell.exe");
        let shell = missing_shell.to_str().unwrap();
        assert_eq!(
            powershell_resolve(shell, "Get-Item").await,
            ResolveOutcome::Indeterminate
        );
        assert!(enumerate_powershell_commands(shell).await.is_none());
        assert_eq!(
            observer.attempts(),
            [("resolve", true), ("enumerate", true), ("enumerate", false)]
        );
        drop(observer);
        assert!(probe_observer::ProbeObserver::start().attempts().is_empty());
    }

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
        assert_eq!(
            sorted.len(),
            got.len(),
            "must not contain duplicates: {got:?}"
        );
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
    fn parse_output_none_when_sentinel_absent() {
        // The enumerate always prints the sentinel; its absence means the probe
        // never completed, so return None (caller falls back) rather than
        // parsing profile error text as bogus command names.
        let raw = ["git", "Get-ChildItem"].join("\n");
        assert!(parse_enumerate_output(&raw).is_none());
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
        assert_eq!(
            got[0],
            CommandResolution {
                command_type: "Alias".into(),
                name: "which".into(),
                target: "where.exe".into(),
            }
        );
        assert_eq!(got[1].command_type, "Application");
        assert_eq!(got[1].target, "C:\\Windows\\system32\\where.exe");
    }

    #[test]
    fn parse_resolve_output_none_when_no_rows() {
        // Sentinel only (token didn't resolve) → None.
        assert!(parse_resolve_output(ENUM_SENTINEL).is_none());
    }

    #[test]
    fn parse_resolve_output_none_when_sentinel_absent() {
        // No sentinel means the probe never completed; don't parse whatever
        // stdout is there (e.g. profile error text) as resolutions.
        let raw = ["Exception: boom", "at line 1"].join("\n");
        assert!(parse_resolve_output(&raw).is_none());
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
            names
                .iter()
                .any(|n| n.eq_ignore_ascii_case("Get-ChildItem")),
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
            names
                .iter()
                .any(|n| n.eq_ignore_ascii_case("Get-ChildItem")),
            "expected Get-ChildItem from a bare-name enumerate"
        );
    }

    #[tokio::test]
    async fn near_matches_none_for_an_existing_cmdlet() {
        let Some(shell) = powershell_host() else {
            eprintln!("no PowerShell host installed; skipping");
            return;
        };
        // `Get-ChildItem` is a core cmdlet — always present regardless of the
        // contributor's profile, and not a PATH file (so the in-process `which`
        // pre-gate passes). The full enumerate gate must still recognize it and
        // suppress near-match injection.
        let got = powershell_near_matches(&shell, "Get-ChildItem").await;
        assert!(
            got.is_none(),
            "expected None for the existing cmdlet, got {got:?}"
        );
    }

    #[tokio::test]
    async fn resolve_reports_a_local_script_on_path() {
        let Some(shell) = powershell_host() else {
            eprintln!("no PowerShell host installed; skipping");
            return;
        };

        // Self-made, deterministic fixture: drop a uniquely-named script into a
        // fresh dir on PATH, so resolve doesn't depend on whatever aliases the
        // contributor's profile happens to define (a built-in alias like `gci`
        // can be removed/redefined). The resolve subprocess inherits this PATH
        // and reports the file as an ExternalScript with its full path. The
        // Alias `type`/`target` shape is covered separately by the pure
        // `parse_resolve_output_*` tests.
        let _guard = PATH_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("resolve_fixture_{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp script dir");
        std::fs::write(dir.join("resolve-fixture.ps1"), "Write-Host hi").expect("write script");

        let original_path = std::env::var_os("PATH");
        let mut prepended = std::ffi::OsString::from(&dir);
        if let Some(existing) = &original_path {
            prepended.push(";");
            prepended.push(existing);
        }
        std::env::set_var("PATH", &prepended);

        // External scripts need the extension for an exact `Get-Command -Name`.
        let result = powershell_resolve(&shell, "resolve-fixture.ps1").await;

        // Always restore PATH and remove the temp dir *before* asserting.
        match original_path {
            Some(p) => std::env::set_var("PATH", p),
            None => std::env::remove_var("PATH"),
        }
        let _ = std::fs::remove_dir_all(&dir);

        // A slow/hanging profile can legitimately time out → Indeterminate
        // (part of the contract); skip rather than fail on such machines.
        let got = match result {
            ResolveOutcome::Resolved(got) => got,
            ResolveOutcome::Indeterminate => {
                eprintln!("resolve was indeterminate (slow profile?); skipping");
                return;
            }
            ResolveOutcome::NotFound => panic!("the local script should resolve, got NotFound"),
        };
        let hit = got
            .iter()
            .find(|r| r.name.eq_ignore_ascii_case("resolve-fixture.ps1"))
            .unwrap_or_else(|| panic!("expected a resolution for the fixture script, got {got:?}"));
        assert!(
            hit.command_type.eq_ignore_ascii_case("ExternalScript"),
            "expected ExternalScript, got {hit:?}"
        );
        assert!(
            hit.target
                .to_ascii_lowercase()
                .ends_with("resolve-fixture.ps1"),
            "target should be the script's path, got {hit:?}"
        );
    }

    #[tokio::test]
    async fn resolve_not_found_for_unknown_token() {
        let Some(shell) = powershell_host() else {
            eprintln!("no PowerShell host installed; skipping");
            return;
        };
        let got = powershell_resolve(&shell, "no-such-command").await;
        // Skip on Indeterminate (slow-profile timeout is part of the contract).
        if got == ResolveOutcome::Indeterminate {
            eprintln!("resolve was indeterminate (slow profile?); skipping");
            return;
        }
        assert_eq!(
            got,
            ResolveOutcome::NotFound,
            "expected NotFound for a nonexistent command, got {got:?}"
        );
    }

    #[tokio::test]
    async fn resolve_treats_wildcards_literally() {
        let Some(shell) = powershell_host() else {
            eprintln!("no PowerShell host installed; skipping");
            return;
        };
        // `gc*` would match many real commands if `-Name` did wildcard
        // expansion. Escaped, it's a literal name that doesn't exist → NotFound,
        // so a wildcard token can't falsely report `exists` / dump a huge set.
        let got = powershell_resolve(&shell, "gc*").await;
        // Skip on Indeterminate (slow-profile timeout is part of the contract).
        if got == ResolveOutcome::Indeterminate {
            eprintln!("resolve was indeterminate (slow profile?); skipping");
            return;
        }
        assert_eq!(
            got,
            ResolveOutcome::NotFound,
            "wildcard token must be matched literally, got {got:?}"
        );
    }

    #[tokio::test]
    async fn near_matches_observes_script_changes_without_path_changes() {
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
        let renamed = std::fs::rename(dir.join("wtdeployit.ps1"), dir.join("wtdeployit-new.ps1"));
        let refreshed = powershell_near_matches(&shell, "wtdeployt-new").await;

        // Always restore PATH and remove the temp dir *before* asserting, so a
        // failed assertion can never leak state into other tests.
        match original_path {
            Some(p) => std::env::set_var("PATH", p),
            None => std::env::remove_var("PATH"),
        }
        let _ = std::fs::remove_dir_all(&dir);

        renamed.expect("rename script without changing PATH");
        let matches = result.expect("expected near-matches for a local script typo");
        assert!(
            matches.iter().any(|m| m.eq_ignore_ascii_case("wtdeployit")),
            "expected the local script `wtdeployit` among near-matches, got {matches:?}"
        );
        let matches = refreshed.expect("expected fresh near-matches after renaming the script");
        assert!(
            matches
                .iter()
                .any(|m| m.eq_ignore_ascii_case("wtdeployit-new")),
            "expected the renamed script among near-matches, got {matches:?}"
        );
        assert!(
            !matches.iter().any(|m| m.eq_ignore_ascii_case("wtdeployit")),
            "a removed script must not survive in cached results"
        );
    }
}
