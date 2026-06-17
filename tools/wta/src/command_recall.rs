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
//! 3. Otherwise enumerate the shell's real command list once
//!    (`Get-Command …`) and, if the token still doesn't resolve, rank the
//!    list by Damerau-Levenshtein ([`rank_near_matches`]) to surface the
//!    closest existing commands.
//!
//! The gate is locale-independent: it asks the shell "does this command
//! exist", never matches the (localized) error text. The enumerate cost is
//! only paid on a genuine not-found, which is rare.
//!
//! Known blind spot (accepted for v1): the enumerate subprocess runs with
//! `-NoProfile`, so it sees PATH executables and external scripts (the
//! issue's concern) but not functions/aliases defined only in the user's
//! interactive profile.

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

/// True when `shell_exe` is a PowerShell host (`pwsh.exe` / `powershell.exe`),
/// matched on the leaf name so a full path also works. v1 only recalls for
/// PowerShell panes.
pub fn is_powershell(shell_exe: &str) -> bool {
    let lower = shell_exe.to_ascii_lowercase();
    let leaf = lower.rsplit(['\\', '/']).next().unwrap_or(lower.as_str());
    leaf == "pwsh.exe" || leaf == "powershell.exe"
}

/// Extract the command token (executable name) from a captured
/// `[command + output]` buffer.
///
/// Returns `None` when there is no usable token, or when the token is an
/// explicit path / call-operator invocation (`.\x.ps1`, `C:\x.exe`, `& x`) —
/// a PATH-lookup near-match wouldn't apply to those.
pub fn extract_command_token(content: &str) -> Option<String> {
    let first_line = content.lines().map(str::trim).find(|l| !l.is_empty())?;
    let token = first_line.split_whitespace().next()?;
    let token = token.trim_matches(|c| c == '"' || c == '\'');
    // Explicit path, relative path, or call operator → not a bare PATH command.
    if token.is_empty()
        || token.starts_with('&')
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
        if name.len() > ext.len() && name[name.len() - ext.len()..].eq_ignore_ascii_case(ext) {
            return &name[..name.len() - ext.len()];
        }
    }
    name
}

/// True when `token` matches a known command `name` (case-insensitive, after
/// extension stripping). Used as the existence gate: a hit means the failure
/// wasn't a not-found, so no near-matches should be injected.
pub fn command_exists(token: &str, names: &[String]) -> bool {
    let t = token.to_ascii_lowercase();
    names.iter().any(|n| strip_exe_ext(n).eq_ignore_ascii_case(&t))
}

/// Rank `names` by Damerau-Levenshtein distance to `token`, returning up to
/// [`MAX_NEAR_MATCHES`] closest unique display names (extension-stripped),
/// nearest first, ties broken alphabetically. Anything beyond an adaptive
/// distance threshold is dropped so a wild typo doesn't surface noise.
pub fn rank_near_matches(token: &str, names: &[String], max: usize) -> Vec<String> {
    let t = token.to_ascii_lowercase();
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

    let names = enumerate_powershell_commands(shell_exe).await?;

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

/// Enumerate the shell's command names (applications, external scripts,
/// functions, aliases) in one `-NoProfile` subprocess. Cmdlets are
/// intentionally excluded — they roughly double the enumerate time and the
/// issue is about PATH scripts/programs.
async fn enumerate_powershell_commands(shell_exe: &str) -> Option<Vec<String>> {
    let exe = if shell_exe.trim().is_empty() {
        "powershell.exe"
    } else {
        shell_exe
    };

    let mut cmd = tokio::process::Command::new(exe);
    cmd.args([
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        "Get-Command -CommandType Application,ExternalScript,Function,Alias | \
         Select-Object -ExpandProperty Name",
    ])
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let output = cmd.output().await.ok()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let names: Vec<String> = stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
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

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn is_powershell_matches_leaf_name_and_full_path() {
        assert!(is_powershell("pwsh.exe"));
        assert!(is_powershell("powershell.exe"));
        assert!(is_powershell(r"C:\Program Files\PowerShell\7\pwsh.exe"));
        assert!(is_powershell("PWSH.EXE")); // case-insensitive
        assert!(!is_powershell("bash.exe"));
        assert!(!is_powershell("cmd.exe"));
        assert!(!is_powershell("wsl.exe"));
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
    fn extract_token_rejects_explicit_paths_and_call_operator() {
        // Explicit paths and call-operator invocations are not PATH lookups,
        // so a near-match suggestion wouldn't apply.
        assert_eq!(extract_command_token(r".\build.ps1"), None);
        assert_eq!(extract_command_token(r"C:\tools\x.exe -a"), None);
        assert_eq!(extract_command_token("/usr/bin/foo"), None);
        assert_eq!(extract_command_token("& somecmd"), None);
        assert_eq!(extract_command_token("   "), None);
        assert_eq!(extract_command_token(""), None);
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
}
