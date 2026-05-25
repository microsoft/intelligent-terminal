//! Slash-command system for the agent pane.
//!
//! The user types `/foo` in the input box; on Enter, [`parse`] resolves the
//! input to a [`ParsedCommand`] (or returns `None`, in which case the line is
//! sent as a normal prompt). The autocomplete popup uses [`matches`] for
//! prefix-filtered suggestions.
//!
//! See `tools/wta/src/app.rs` `App::handle_slash_command` for dispatch and
//! `tools/wta/src/ui/command_popup.rs` for rendering.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandKind {
    Help,
    Clear,
    Stop,
    New,
    Restart,
    Sessions,
}

#[derive(Debug, Clone, Copy)]
pub struct CommandSpec {
    pub name: &'static str,
    /// rust-i18n key for the user-facing description. Resolved at render
    /// time so the popup follows the current locale.
    pub summary_key: &'static str,
    pub kind: CommandKind,
    /// True if this command takes free-form arguments after the name.
    /// MVP commands are all zero-arg; the field exists so the popup
    /// knows whether to leave a trailing space after Tab-completion.
    pub takes_args: bool,
}

impl CommandSpec {
    /// Look up the localized summary at render time.
    ///
    /// Returns `Cow<'static, str>` so the common case where rust-i18n's
    /// store contains the key as a `&'static str` (the typical compile-
    /// time-embedded yml) avoids an allocation on every render — the
    /// command popup re-fetches summaries per frame and per row.
    pub fn summary(&self) -> std::borrow::Cow<'static, str> {
        rust_i18n::t!(self.summary_key)
    }
}

/// Static registry. Order is the display order in `/help`.
pub const REGISTRY: &[CommandSpec] = &[
    CommandSpec {
        name: "help",
        summary_key: "commands.help.summary",
        kind: CommandKind::Help,
        takes_args: false,
    },
    CommandSpec {
        name: "clear",
        summary_key: "commands.clear.summary",
        kind: CommandKind::Clear,
        takes_args: false,
    },
    CommandSpec {
        name: "new",
        summary_key: "commands.new.summary",
        kind: CommandKind::New,
        takes_args: false,
    },
    CommandSpec {
        name: "restart",
        summary_key: "commands.restart.summary",
        kind: CommandKind::Restart,
        takes_args: false,
    },
    CommandSpec {
        name: "stop",
        summary_key: "commands.stop.summary",
        kind: CommandKind::Stop,
        takes_args: false,
    },
    CommandSpec {
        name: "sessions",
        summary_key: "commands.sessions.summary",
        kind: CommandKind::Sessions,
        takes_args: false,
    },
];

#[derive(Debug, Clone)]
pub struct ParsedCommand {
    pub kind: CommandKind,
    pub spec: &'static CommandSpec,
    /// Anything after the command name (trimmed, may be empty).
    /// MVP commands ignore this; reserved for future `/model`, `/cwd`, …
    #[allow(dead_code)]
    pub rest: String,
}

/// Parse an input line as a slash-command, or return `None` if the line
/// should be sent as a normal prompt.
///
/// Rules:
/// - Leading whitespace is allowed (`" /stop"` parses as `/stop`).
/// - Command name is matched case-insensitively.
/// - `//literal` is *not* a command (escape for `/etc/hosts` etc.) — falls
///   through to `None`. Stripping the leading `/` is the dispatcher's job.
/// - Unknown command names also return `None` so the line still goes through
///   as a prompt; the caller is responsible for the "Unknown command"
///   advisory message.
pub fn parse(input: &str) -> Option<ParsedCommand> {
    let trimmed = input.trim_start();
    let rest = trimmed.strip_prefix('/')?;

    // `//literal` → not a command.
    if rest.starts_with('/') {
        return None;
    }

    let (name, args) = match rest.find(char::is_whitespace) {
        Some(idx) => (&rest[..idx], rest[idx..].trim()),
        None => (rest, ""),
    };

    if name.is_empty() {
        return None;
    }

    let spec = lookup(name)?;
    Some(ParsedCommand {
        kind: spec.kind,
        spec,
        rest: args.to_string(),
    })
}

/// Return the [`CommandSpec`] for the given name (case-insensitive), or
/// `None` if unknown.
pub fn lookup(name: &str) -> Option<&'static CommandSpec> {
    REGISTRY
        .iter()
        .find(|spec| spec.name.eq_ignore_ascii_case(name))
}

/// Prefix-match against the registry (case-insensitive). An empty prefix
/// returns the full registry in declaration order. Used by the autocomplete
/// popup.
pub fn matches(prefix: &str) -> Vec<&'static CommandSpec> {
    let needle = prefix.trim().to_ascii_lowercase();
    REGISTRY
        .iter()
        .filter(|spec| spec.name.starts_with(&needle))
        .collect()
}

/// True if the input *looks like* a slash-command attempt (starts with `/`
/// and has no whitespace yet). Used to decide whether to show the popup.
pub fn is_command_prefix(input: &str) -> bool {
    let trimmed = input.trim_start();
    let Some(rest) = trimmed.strip_prefix('/') else {
        return false;
    };
    if rest.starts_with('/') {
        return false;
    }
    !rest.contains(char::is_whitespace)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_parses() {
        let p = parse("/help").unwrap();
        assert_eq!(p.kind, CommandKind::Help);
        assert_eq!(p.rest, "");
    }

    #[test]
    fn case_insensitive() {
        assert_eq!(parse("/HELP").unwrap().kind, CommandKind::Help);
        assert_eq!(parse("/StOp").unwrap().kind, CommandKind::Stop);
    }

    #[test]
    fn sessions_parses() {
        assert_eq!(parse("/sessions").unwrap().kind, CommandKind::Sessions);
        // /se prefix-completes to /sessions but full /se is not a registered name.
        assert!(parse("/se").is_none());
        // Both /stop and /sessions begin with `s` — matches() should surface both.
        let s_matches: Vec<&str> = matches("s").into_iter().map(|c| c.name).collect();
        assert!(s_matches.contains(&"stop"));
        assert!(s_matches.contains(&"sessions"));
    }

    #[test]
    fn rest_is_captured() {
        let p = parse("/help me please").unwrap();
        assert_eq!(p.kind, CommandKind::Help);
        assert_eq!(p.rest, "me please");
    }

    #[test]
    fn leading_whitespace_ok() {
        assert_eq!(parse("   /stop").unwrap().kind, CommandKind::Stop);
    }

    #[test]
    fn unknown_command_falls_through() {
        assert!(parse("/no-such-command").is_none());
        assert!(parse("/no-such-command foo").is_none());
    }

    #[test]
    fn double_slash_is_not_a_command() {
        assert!(parse("//etc/hosts").is_none());
        assert!(parse("//literal").is_none());
    }

    #[test]
    fn no_slash_prefix_is_not_a_command() {
        assert!(parse("hello").is_none());
        assert!(parse("prompt /stop in middle").is_none());
    }

    #[test]
    fn empty_slash_is_not_a_command() {
        assert!(parse("/").is_none());
        assert!(parse("/  ").is_none());
    }

    #[test]
    fn matches_filters_by_prefix() {
        let all = matches("");
        assert_eq!(all.len(), REGISTRY.len());

        let h = matches("h");
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].name, "help");

        let none = matches("zzz");
        assert!(none.is_empty());
    }

    #[test]
    fn matches_case_insensitive() {
        let h = matches("HE");
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].name, "help");
    }

    #[test]
    fn is_command_prefix_basic() {
        assert!(is_command_prefix("/"));
        assert!(is_command_prefix("/h"));
        assert!(is_command_prefix("/help"));
        assert!(is_command_prefix("   /help"));
        // Once the user types a space, we're in argument-land — popup hides.
        assert!(!is_command_prefix("/help "));
        assert!(!is_command_prefix("/help me"));
        // Not a command shape.
        assert!(!is_command_prefix("//"));
        assert!(!is_command_prefix("hello"));
        assert!(!is_command_prefix(""));
    }
}
