//! Splitting/formatting helpers for shell-command display, shared by the
//! chat tool-call card (`ui/chat.rs`) and the permission dialog
//! (`ui/permission.rs`).
//!
//! Agents often bundle several PowerShell statements into one
//! `raw_input.command` (e.g. `winget list ...; winget list ...` or
//! `$paths = @(...); Get-ChildItem ...`) to batch multiple checks into a
//! single tool call. Rendering that as one long line — which then wraps at
//! the terminal edge with no hanging indent — reads as an unreadable wall
//! of text with the continuation flush-left and disconnected from the `$`
//! marker. Splitting on top-level `;` separators turns it back into the
//! sequence of discrete steps the agent actually intended, one per line.

/// Cap on the number of command lines shown per card. An agent can chain
/// many statements in one command; past this the remainder folds into a
/// single `"… (+N more)"` line rather than growing the card indefinitely.
const MAX_COMMAND_LINES: usize = 3;

/// Cap on characters kept per *individual* statement line (after
/// splitting) — long enough for a typical one-liner, short enough that a
/// single pathological statement (e.g. an inlined array literal, which
/// isn't split further since it wouldn't be safe to split inside `@(...)`)
/// can't still blow up one row into a multi-line wrap.
const MAX_STATEMENT_CHARS: usize = 100;

/// Splits a command string on top-level `;` statement separators — i.e.
/// semicolons outside quoted strings (`'...'`/`"..."`) and outside
/// brackets/parens/braces, so a PowerShell array literal like
/// `@('a;b', 'c')` isn't split mid-literal. Falls back to the whole string
/// as a single "statement" when there's nothing to split (including when
/// unbalanced quotes/brackets mean no top-level semicolon was ever found).
fn split_statements(command: &str) -> Vec<String> {
    let mut statements = Vec::new();
    let mut current = String::new();
    let mut depth: i32 = 0;
    let mut quote: Option<char> = None;

    for c in command.chars() {
        if let Some(q) = quote {
            current.push(c);
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' => {
                quote = Some(c);
                current.push(c);
            }
            '(' | '[' | '{' => {
                depth += 1;
                current.push(c);
            }
            ')' | ']' | '}' => {
                depth -= 1;
                current.push(c);
            }
            ';' if depth <= 0 => {
                let trimmed = current.trim();
                if !trimmed.is_empty() {
                    statements.push(trimmed.to_string());
                }
                current.clear();
            }
            _ => current.push(c),
        }
    }
    let trimmed = current.trim();
    if !trimmed.is_empty() {
        statements.push(trimmed.to_string());
    }

    if statements.is_empty() {
        vec![command.to_string()]
    } else {
        statements
    }
}

fn truncate_statement(s: &str) -> String {
    if s.chars().count() > MAX_STATEMENT_CHARS {
        let head: String = s.chars().take(MAX_STATEMENT_CHARS).collect();
        format!("{head}…")
    } else {
        s.to_string()
    }
}

/// One rendered row for a command target: either a concrete statement
/// (rendered with the `$ ` shell-prompt prefix) or a folded "+N more"
/// summary row (rendered without it — it isn't a command).
pub(crate) enum CommandLine {
    Statement(String),
    Folded { remaining: usize },
}

/// Formats a (possibly multi-statement) command into the display rows a
/// card should show: one per top-level statement, each truncated
/// individually, capped at `MAX_COMMAND_LINES` with any remainder folded
/// into a single trailing `Folded` row.
pub(crate) fn command_display_lines(command: &str) -> Vec<CommandLine> {
    let statements = split_statements(command);
    if statements.len() <= MAX_COMMAND_LINES {
        statements
            .iter()
            .map(|s| CommandLine::Statement(truncate_statement(s)))
            .collect()
    } else {
        let mut lines: Vec<CommandLine> = statements[..MAX_COMMAND_LINES - 1]
            .iter()
            .map(|s| CommandLine::Statement(truncate_statement(s)))
            .collect();
        let remaining = statements.len() - (MAX_COMMAND_LINES - 1);
        lines.push(CommandLine::Folded { remaining });
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A single-statement command (the common case) round-trips as one
    /// unfolded `Statement` line.
    #[test]
    fn single_statement_is_one_line() {
        let lines = command_display_lines("cargo test --workspace");
        assert_eq!(lines.len(), 1);
        assert!(matches!(&lines[0], CommandLine::Statement(s) if s == "cargo test --workspace"));
    }

    /// Top-level `;` separators split into one line per statement — this
    /// is the screenshot bug: `winget list ...; winget list ...` must not
    /// render as one crammed line.
    #[test]
    fn splits_on_top_level_semicolons() {
        let lines = command_display_lines(
            "winget list --name PowerToys 2>$null; winget list --name Foundry 2>$null",
        );
        assert_eq!(lines.len(), 2);
        assert!(matches!(&lines[0], CommandLine::Statement(s) if s == "winget list --name PowerToys 2>$null"));
        assert!(matches!(&lines[1], CommandLine::Statement(s) if s == "winget list --name Foundry 2>$null"));
    }

    /// A `;` inside a quoted string (or inside a PowerShell array literal)
    /// must NOT be treated as a statement separator — otherwise splitting
    /// would corrupt the command's meaning, not just its display.
    #[test]
    fn semicolon_inside_quotes_or_brackets_is_not_a_split_point() {
        let lines = command_display_lines(r#"Write-Output 'a;b'; @('x;y', 'z')"#);
        assert_eq!(lines.len(), 2);
        assert!(matches!(&lines[0], CommandLine::Statement(s) if s == "Write-Output 'a;b'"));
        assert!(matches!(&lines[1], CommandLine::Statement(s) if s == "@('x;y', 'z')"));
    }

    /// More statements than `MAX_COMMAND_LINES` fold the remainder into a
    /// single `Folded` row instead of growing the card unboundedly.
    #[test]
    fn excess_statements_fold_into_a_remainder_line() {
        let lines = command_display_lines("a; b; c; d; e");
        assert_eq!(lines.len(), MAX_COMMAND_LINES);
        assert!(matches!(&lines[0], CommandLine::Statement(s) if s == "a"));
        assert!(matches!(&lines[1], CommandLine::Statement(s) if s == "b"));
        assert!(matches!(&lines[2], CommandLine::Folded { remaining: 3 }));
    }

    /// A single statement longer than `MAX_STATEMENT_CHARS` (e.g. an
    /// inlined array literal, which we deliberately don't split inside)
    /// still gets truncated so it can't blow up into a multi-row wrap.
    #[test]
    fn long_single_statement_is_truncated() {
        let long = "a".repeat(150);
        let lines = command_display_lines(&long);
        assert_eq!(lines.len(), 1);
        match &lines[0] {
            CommandLine::Statement(s) => {
                assert_eq!(s.chars().count(), MAX_STATEMENT_CHARS + 1); // + the '…' marker
                assert!(s.ends_with('…'));
            }
            CommandLine::Folded { .. } => panic!("expected a Statement line"),
        }
    }
}
