// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// Pure functions for building Windows commandline strings.
// Extracted from shell_manager for testability.

/// Error returned by [`build_wt_commandline`] when the input cannot be
/// encoded as a valid Windows commandline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildCommandlineError {
    /// The program path (argv[0]) contains a literal `"`. There is no
    /// `CommandLineToArgvW`-compatible way to escape it.
    QuoteInProgram,
    /// The program path contains a NUL byte.
    NulInProgram,
    /// An argument contains a NUL byte.
    NulInArgument,
}

impl std::fmt::Display for BuildCommandlineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::QuoteInProgram => {
                f.write_str("executable path cannot contain a literal double quote")
            }
            Self::NulInProgram => f.write_str("executable path cannot contain a NUL byte"),
            Self::NulInArgument => f.write_str("argument cannot contain a NUL byte"),
        }
    }
}

impl std::error::Error for BuildCommandlineError {}

/// Quote the program path (argv[0]). `CommandLineToArgvW` treats the first
/// token specially: backslashes are literal, and the first unescaped `"`
/// ends argv[0]. There is no way to escape `"` inside it, so we reject
/// inputs containing `"`.
fn append_program(cmdline: &mut String, value: &str) -> Result<(), BuildCommandlineError> {
    if value.contains('\0') {
        return Err(BuildCommandlineError::NulInProgram);
    }
    if value.contains('"') {
        return Err(BuildCommandlineError::QuoteInProgram);
    }
    cmdline.push('"');
    cmdline.push_str(value);
    cmdline.push('"');
    Ok(())
}

/// Append a non-first argument, quoting per `CommandLineToArgvW` rules.
/// Always quotes unconditionally — a `needs_quotes` heuristic is fragile
/// because the OS parser splits on more than just space/tab.
fn append_arg(cmdline: &mut String, value: &str) -> Result<(), BuildCommandlineError> {
    if value.contains('\0') {
        return Err(BuildCommandlineError::NulInArgument);
    }
    cmdline.push('"');
    let mut backslashes: usize = 0;
    for ch in value.chars() {
        match ch {
            '\\' => backslashes += 1,
            '"' => {
                // Double the backslashes before a `"`, then escape the `"`.
                for _ in 0..(backslashes * 2 + 1) {
                    cmdline.push('\\');
                }
                cmdline.push('"');
                backslashes = 0;
            }
            _ => {
                for _ in 0..backslashes {
                    cmdline.push('\\');
                }
                backslashes = 0;
                cmdline.push(ch);
            }
        }
    }
    // Trailing backslashes must be doubled (they precede the closing `"`).
    for _ in 0..(backslashes * 2) {
        cmdline.push('\\');
    }
    cmdline.push('"');
    Ok(())
}

/// Build a commandline string from a command and its arguments.
///
/// The result is compatible with `CommandLineToArgvW` / `CreateProcess`.
/// There is no shell in this pipeline, so metacharacters like `&` / `|`
/// are not special.
pub fn build_wt_commandline(
    command: &str,
    args: &[String],
) -> Result<String, BuildCommandlineError> {
    let mut cmdline = String::new();
    append_program(&mut cmdline, command)?;
    for arg in args {
        cmdline.push(' ');
        append_arg(&mut cmdline, arg)?;
    }
    Ok(cmdline)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    /// Parse a commandline string through the real Windows OS parser.
    /// This is the ground truth — if our output round-trips through
    /// `CommandLineToArgvW` and matches the original input, we're correct.
    fn parse_via_os(cmdline: &str) -> Vec<String> {
        use std::ffi::OsStr;
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Foundation::LocalFree;
        use windows_sys::Win32::UI::Shell::CommandLineToArgvW;

        let wide: Vec<u16> = OsStr::new(cmdline)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        let mut argc: i32 = 0;
        let argv = unsafe { CommandLineToArgvW(wide.as_ptr(), &mut argc) };
        assert!(!argv.is_null(), "CommandLineToArgvW returned null");

        let mut parsed = Vec::with_capacity(argc as usize);
        for i in 0..argc as isize {
            let ptr = unsafe { *argv.offset(i) };
            let mut len = 0isize;
            while unsafe { *ptr.offset(len) } != 0 {
                len += 1;
            }
            let slice = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
            parsed.push(String::from_utf16(slice).expect("invalid UTF-16 from OS"));
        }
        unsafe { LocalFree(argv as _) };
        parsed
    }

    /// Helper: build commandline, parse via OS, assert round-trip matches.
    fn assert_roundtrip(command: &str, args: &[&str]) {
        let args_owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let cmdline =
            build_wt_commandline(command, &args_owned).expect("build_wt_commandline failed");
        let parsed = parse_via_os(&cmdline);

        let mut expected = vec![command.to_string()];
        expected.extend(args_owned);

        assert_eq!(
            parsed, expected,
            "\n  cmdline = {:?}\n  parsed  = {:?}\n  expected= {:?}",
            cmdline, parsed, expected,
        );
    }

    // ── Basic cases ────────────────────────────────────────────────

    #[test]
    fn simple_command_no_args() {
        assert_roundtrip("pwsh.exe", &[]);
    }

    #[test]
    fn simple_command_with_args() {
        assert_roundtrip("pwsh.exe", &["-c", "git status"]);
    }

    #[test]
    fn command_with_spaces_in_path() {
        assert_roundtrip("C:\\Program Files\\my tool\\run.exe", &["--verbose"]);
    }

    // ── Quoting edge cases ─────────────────────────────────────────

    #[test]
    fn arg_with_embedded_double_quote() {
        assert_roundtrip("cmd.exe", &["/c", "echo \"hello\""]);
    }

    #[test]
    fn arg_with_only_double_quotes() {
        assert_roundtrip("test.exe", &["\"\"\""]);
    }

    #[test]
    fn arg_with_spaces_and_quotes() {
        assert_roundtrip("test.exe", &["hello \"world\" foo"]);
    }

    // ── Backslash edge cases ───────────────────────────────────────

    #[test]
    fn arg_with_trailing_backslash() {
        assert_roundtrip("test.exe", &["C:\\path\\"]);
    }

    #[test]
    fn arg_with_trailing_backslashes() {
        assert_roundtrip("test.exe", &["C:\\path\\\\\\"]);
    }

    #[test]
    fn arg_with_backslash_before_quote() {
        assert_roundtrip("test.exe", &["foo\\\"bar"]);
    }

    #[test]
    fn arg_with_multiple_backslashes_before_quote() {
        assert_roundtrip("test.exe", &["foo\\\\\"bar"]);
    }

    #[test]
    fn arg_backslashes_not_before_quote() {
        assert_roundtrip("test.exe", &["C:\\Users\\test\\file.txt"]);
    }

    // ── Whitespace edge cases ──────────────────────────────────────

    #[test]
    fn arg_with_tab() {
        assert_roundtrip("test.exe", &["hello\t world"]);
    }

    #[test]
    fn arg_with_newline() {
        assert_roundtrip("test.exe", &["line1\n line2"]);
    }

    #[test]
    fn arg_with_carriage_return() {
        assert_roundtrip("test.exe", &["line1\r line2"]);
    }

    #[test]
    fn empty_arg() {
        assert_roundtrip("test.exe", &[""]);
    }

    #[test]
    fn multiple_empty_args() {
        assert_roundtrip("test.exe", &["", "", ""]);
    }

    // ── Shell metacharacters (should be literal, no shell) ─────────

    #[test]
    fn arg_with_pipe() {
        assert_roundtrip("test.exe", &["foo|bar"]);
    }

    #[test]
    fn arg_with_ampersand() {
        assert_roundtrip("test.exe", &["foo&bar"]);
    }

    #[test]
    fn arg_with_percent() {
        assert_roundtrip("test.exe", &["%PATH%"]);
    }

    #[test]
    fn arg_with_caret() {
        assert_roundtrip("test.exe", &["foo^bar"]);
    }

    // ── Error cases ────────────────────────────────────────────────

    #[test]
    fn rejects_quote_in_program() {
        let result = build_wt_commandline("bad\"path.exe", &[]);
        assert_eq!(result, Err(BuildCommandlineError::QuoteInProgram));
    }

    #[test]
    fn rejects_nul_in_program() {
        let result = build_wt_commandline("bad\0path.exe", &[]);
        assert_eq!(result, Err(BuildCommandlineError::NulInProgram));
    }

    #[test]
    fn rejects_nul_in_argument() {
        let args = vec!["hello\0world".to_string()];
        let result = build_wt_commandline("test.exe", &args);
        assert_eq!(result, Err(BuildCommandlineError::NulInArgument));
    }

    // ── Stress / combo cases ───────────────────────────────────────

    #[test]
    fn many_args_mixed() {
        assert_roundtrip(
            "C:\\Program Files\\app.exe",
            &[
                "--flag",
                "simple",
                "has spaces",
                "has\"quote",
                "trailing\\",
                "back\\\"slash-quote",
                "",
                "\\\\server\\share\\",
                "multi\n line\t tab",
            ],
        );
    }

    #[test]
    fn realistic_agent_command() {
        assert_roundtrip(
            "pwsh.exe",
            &["-NoProfile", "-Command", "& { git log --oneline -5 }"],
        );
    }

    #[test]
    fn realistic_npx_adapter() {
        assert_roundtrip(
            "npx",
            &["-y", "@zed-industries/claude-code-acp"],
        );
    }

    // ── JSON-in-commandline-arg round-trip ──────────────────────────
    // These simulate the WT→WTA flow: WT serializes agent config as JSON,
    // passes it as a single `--agent-config` argument. We verify the JSON
    // survives the commandline quoting + CommandLineToArgvW round-trip and
    // can be deserialized back to the original structured data.

    #[test]
    fn json_arg_simple() {
        let json = r#"{"agent":"copilot --acp --stdio","agentId":"copilot"}"#;
        assert_roundtrip("wta.exe", &["--agent-config", json]);
    }

    #[test]
    fn json_arg_with_quotes_in_value() {
        // Agent command contains quotes (e.g. a path with spaces that was quoted)
        let json = r#"{"agent":"\"C:\\Program Files\\agent.exe\" --acp","agentId":"custom"}"#;
        assert_roundtrip("wta.exe", &["--agent-config", json]);
    }

    #[test]
    fn json_arg_with_backslashes() {
        // Windows paths with backslashes inside JSON values
        let json = r#"{"agent":"C:\\Users\\test\\copilot.exe --acp --stdio","agentId":"copilot"}"#;
        assert_roundtrip("wta.exe", &["--agent-config", json]);
    }

    #[test]
    fn json_arg_all_fields() {
        let json = r#"{"agent":"copilot --acp --stdio","agentId":"copilot","delegateAgent":"codex","delegateModel":"gpt-4o","acpModel":"claude-sonnet"}"#;
        assert_roundtrip("wta.exe", &["--agent-config", json]);
    }

    #[test]
    fn json_arg_unicode_values() {
        let json = r#"{"agent":"copilot --acp","agentId":"自定义代理"}"#;
        assert_roundtrip("wta.exe", &["--agent-config", json]);
    }

    #[test]
    fn json_arg_with_special_chars_in_model() {
        // Model name with characters that could trip up naive escaping
        let json = r#"{"agent":"copilot","acpModel":"org/model-name:v1.0 (beta)"}"#;
        assert_roundtrip("wta.exe", &["--agent-config", json]);
    }

    #[test]
    fn json_arg_adversarial_injection_attempt() {
        // Attacker tries to break out of JSON arg to inject --no-autofix
        let json = r#"{"agent":"evil\" --no-autofix --agent \"pwned"}"#;
        assert_roundtrip("wta.exe", &["--agent-config", json]);
    }

    #[test]
    fn json_arg_deserialization_roundtrip() {
        // Full end-to-end: build commandline with JSON arg, parse via OS,
        // then deserialize the JSON to verify structured data survives.
        use serde_json::Value;

        let config = serde_json::json!({
            "agent": "copilot --acp --stdio",
            "agentId": "copilot",
            "delegateAgent": "codex --model gpt-4",
            "delegateModel": "gpt-4o",
            "acpModel": "claude-3.5-sonnet"
        });
        let json_str = serde_json::to_string(&config).unwrap();

        let args = vec![
            "--agent-config".to_string(),
            json_str.clone(),
        ];
        let cmdline = build_wt_commandline("wta.exe", &args).unwrap();
        let parsed = parse_via_os(&cmdline);

        // parsed[0] = "wta.exe", parsed[1] = "--agent-config", parsed[2] = json
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[1], "--agent-config");

        // The JSON must deserialize back to the same structured data
        let recovered: Value = serde_json::from_str(&parsed[2]).unwrap();
        assert_eq!(recovered, config);
    }
}
