use std::borrow::Cow;
use std::ops::Range;

use crate::app::{ToolCallKind, ToolCallLocation};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolPhase<'a> {
    Pending,
    Running,
    Succeeded,
    Failed(Option<&'a str>),
    Canceled,
    Unknown(&'a str),
}

impl<'a> ToolPhase<'a> {
    pub(crate) fn from_status(status: &'a str) -> Self {
        if status.eq_ignore_ascii_case("pending") {
            Self::Pending
        } else if status.eq_ignore_ascii_case("inprogress")
            || status.eq_ignore_ascii_case("running")
        {
            Self::Running
        } else if status.eq_ignore_ascii_case("completed")
            || status.eq_ignore_ascii_case("exited (0)")
        {
            Self::Succeeded
        } else if status.eq_ignore_ascii_case("failed") {
            Self::Failed(None)
        } else if let Some((kind, reason)) = status.split_once(':') {
            if kind.eq_ignore_ascii_case("failed") {
                Self::Failed(Some(reason.trim()))
            } else {
                Self::Unknown(status)
            }
        } else if status
            .get(.."exited (".len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("exited ("))
        {
            Self::Failed(Some(status))
        } else if status.eq_ignore_ascii_case("cancelled")
            || status.eq_ignore_ascii_case("canceled")
        {
            Self::Canceled
        } else {
            Self::Unknown(status)
        }
    }

    pub(crate) fn is_active(self) -> bool {
        matches!(self, Self::Pending | Self::Running)
    }

    pub(crate) fn is_successful(self) -> bool {
        self == Self::Succeeded
    }
}

pub(crate) struct ToolPresentation<'a> {
    pub(crate) phase: ToolPhase<'a>,
    pub(crate) kind: ToolCallKind,
    pub(crate) title: &'a str,
    pub(crate) target: Option<&'a str>,
    pub(crate) target_is_command: bool,
    pub(crate) cwd: Option<&'a str>,
    pub(crate) exit_code: Option<i64>,
}

impl<'a> ToolPresentation<'a> {
    pub(crate) fn new(
        title: &'a str,
        status: &'a str,
        kind: ToolCallKind,
        target: Option<&'a str>,
        target_is_command: bool,
        cwd: Option<&'a str>,
        exit_code: Option<i64>,
        locations: &'a [ToolCallLocation],
    ) -> Self {
        Self {
            phase: ToolPhase::from_status(status),
            kind,
            title,
            target: target.or_else(|| locations.first().map(|location| location.path.as_str())),
            target_is_command,
            cwd,
            exit_code,
        }
    }

    pub(crate) fn display_title(&self) -> Cow<'a, str> {
        let Some(target) = self.target.filter(|target| !target.is_empty()) else {
            return Cow::Borrowed(self.title);
        };
        let compact = compact_target(self.kind, target);
        let Some(range) = self.target_range_in_title(target) else {
            return Cow::Borrowed(self.title);
        };
        let mut title =
            String::with_capacity(self.title.len().saturating_sub(target.len()) + compact.len());
        title.push_str(&self.title[..range.start]);
        title.push_str(&compact);
        title.push_str(&self.title[range.end..]);
        Cow::Owned(title)
    }

    pub(crate) fn display_target(&self) -> Option<Cow<'a, str>> {
        self.target
            .filter(|target| !target.is_empty())
            .map(|target| compact_target(self.kind, target))
    }

    pub(crate) fn kind_label(&self) -> Cow<'static, str> {
        let key = match self.kind {
            ToolCallKind::Execute => "chat.tool_kind.run",
            ToolCallKind::Read => "chat.tool_kind.read",
            ToolCallKind::Search => "chat.tool_kind.search",
            ToolCallKind::Edit => "chat.tool_kind.edit",
            ToolCallKind::Delete => "chat.tool_kind.delete",
            ToolCallKind::Move => "chat.tool_kind.move",
            ToolCallKind::Fetch => "chat.tool_kind.fetch",
            ToolCallKind::Think => "chat.tool_kind.think",
            ToolCallKind::SwitchMode => "chat.tool_kind.mode",
            ToolCallKind::Other => "chat.tool_kind.other",
        };
        rust_i18n::t!(key)
    }

    pub(crate) fn primary_text(&self, compact: bool) -> Cow<'a, str> {
        match self.kind {
            ToolCallKind::Execute if compact => self
                .inline_compact_command()
                .map(Cow::Borrowed)
                .unwrap_or_else(|| self.display_title()),
            ToolCallKind::Read
            | ToolCallKind::Edit
            | ToolCallKind::Delete
            | ToolCallKind::Fetch => self
                .display_target()
                .unwrap_or_else(|| self.display_title()),
            _ => self.display_title(),
        }
    }

    pub(crate) fn secondary_target(&self) -> Option<Cow<'a, str>> {
        let kind_uses_title = matches!(
            self.kind,
            ToolCallKind::Search
                | ToolCallKind::Move
                | ToolCallKind::Think
                | ToolCallKind::SwitchMode
                | ToolCallKind::Other
        );
        (kind_uses_title && !self.title_contains_target())
            .then(|| self.display_target())
            .flatten()
    }

    pub(crate) fn title_contains_target(&self) -> bool {
        self.target
            .filter(|target| !target.is_empty())
            .is_some_and(|target| self.target_range_in_title(target).is_some())
    }

    pub(crate) fn target_name(&self) -> Option<&'a str> {
        self.target
            .filter(|target| !target.is_empty())
            .and_then(|target| {
                target
                    .rsplit(['\\', '/'])
                    .find(|component| !component.is_empty())
            })
    }

    pub(crate) fn inline_compact_command(&self) -> Option<&'a str> {
        let command = self
            .target
            .filter(|_| self.target_is_command)
            .filter(|command| !command.is_empty())?;
        (command.chars().count() <= 48 && !command.contains([';', '\n', '\r'])).then_some(command)
    }

    pub(crate) fn visible_exit_code(self: &Self) -> Option<i64> {
        if self.kind != ToolCallKind::Execute && !self.target_is_command {
            return None;
        }
        if matches!(self.phase, ToolPhase::Failed(Some(_))) {
            return None;
        }
        self.exit_code.filter(|code| *code != 0)
    }

    fn target_range_in_title(&self, target: &str) -> Option<Range<usize>> {
        if matches!(
            self.kind,
            ToolCallKind::Read
                | ToolCallKind::Edit
                | ToolCallKind::Delete
                | ToolCallKind::Move
                | ToolCallKind::Search
        ) {
            ascii_case_insensitive_range(self.title, target)
        } else {
            self.title
                .find(target)
                .map(|start| start..start + target.len())
        }
    }
}

fn ascii_case_insensitive_range(haystack: &str, needle: &str) -> Option<Range<usize>> {
    if needle.is_empty() {
        return Some(0..0);
    }

    haystack.char_indices().find_map(|(start, _)| {
        let end = start.checked_add(needle.len())?;
        // `get` rejects an end in the middle of a UTF-8 code point, so the
        // returned range is always safe to use for later string slicing.
        haystack
            .get(start..end)
            .filter(|candidate| candidate.eq_ignore_ascii_case(needle))
            .map(|_| start..end)
    })
}

fn compact_target(kind: ToolCallKind, target: &str) -> Cow<'_, str> {
    if kind == ToolCallKind::Fetch {
        return compact_fetch_target(target);
    }
    if !matches!(
        kind,
        ToolCallKind::Read
            | ToolCallKind::Edit
            | ToolCallKind::Delete
            | ToolCallKind::Move
            | ToolCallKind::Search
    ) {
        return Cow::Borrowed(target);
    }

    let parts: Vec<&str> = target
        .split(['\\', '/'])
        .filter(|part| !part.is_empty())
        .collect();
    let keep = if kind == ToolCallKind::Search { 1 } else { 2 };
    if parts.len() <= keep {
        return Cow::Borrowed(target);
    }
    let separator = if target.contains('\\') { "\\" } else { "/" };
    Cow::Owned(parts[parts.len() - keep..].join(separator))
}

fn compact_fetch_target(target: &str) -> Cow<'_, str> {
    let trimmed = target.trim();
    let without_scheme = trimmed
        .split_once("://")
        .map_or(trimmed, |(_, remainder)| remainder);
    let safe_end = without_scheme
        .find(['?', '#'])
        .unwrap_or(without_scheme.len());
    let safe = &without_scheme[..safe_end];
    let (authority, path) = safe.split_once('/').unwrap_or((safe, ""));
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let path_parts = path
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();

    let compact = match path_parts.as_slice() {
        [] => authority.to_string(),
        [only] => format!("{authority}/{only}"),
        [first, second] => format!("{authority}/{first}/{second}"),
        parts => format!(
            "{authority}/…/{}/{}",
            parts[parts.len() - 2],
            parts[parts.len() - 1]
        ),
    };
    if compact == target {
        Cow::Borrowed(target)
    } else {
        Cow::Owned(compact)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_provider_statuses() {
        assert_eq!(ToolPhase::from_status("Pending"), ToolPhase::Pending);
        assert_eq!(ToolPhase::from_status("running"), ToolPhase::Running);
        assert_eq!(ToolPhase::from_status("Completed"), ToolPhase::Succeeded);
        assert_eq!(
            ToolPhase::from_status("Failed: denied"),
            ToolPhase::Failed(Some("denied"))
        );
        assert_eq!(
            ToolPhase::from_status("exited (2)"),
            ToolPhase::Failed(Some("exited (2)"))
        );
        assert_eq!(ToolPhase::from_status("Cancelled"), ToolPhase::Canceled);
    }

    #[test]
    fn compacts_file_and_search_targets() {
        assert_eq!(
            compact_target(ToolCallKind::Read, r"C:\project\src\main.rs"),
            r"src\main.rs"
        );
        assert_eq!(
            compact_target(ToolCallKind::Search, r"C:\project\rust-app"),
            "rust-app"
        );
        assert_eq!(
            compact_target(ToolCallKind::Execute, "cargo test"),
            "cargo test"
        );
    }

    #[test]
    fn compacts_fetch_targets_without_credentials_or_query_data() {
        assert_eq!(
            compact_target(
                ToolCallKind::Fetch,
                "https://user:secret@api.example.com/v1/repos/terminal/issues?token=secret#result"
            ),
            "api.example.com/…/terminal/issues"
        );
        assert_eq!(
            compact_target(ToolCallKind::Fetch, "https://example.com/status"),
            "example.com/status"
        );
    }

    #[test]
    fn replaces_provider_embedded_path_with_compact_target() {
        let locations = vec![ToolCallLocation {
            path: r"C:\project\src\main.rs".into(),
            line: None,
        }];
        let presentation = ToolPresentation::new(
            r"Viewing C:\project\src\main.rs",
            "Completed",
            ToolCallKind::Read,
            None,
            false,
            None,
            None,
            &locations,
        );

        assert_eq!(presentation.display_title(), r"Viewing src\main.rs");
        assert!(presentation.title_contains_target());
        assert_eq!(presentation.target_name(), Some("main.rs"));
        assert_eq!(presentation.primary_text(true), r"src\main.rs");
    }

    #[test]
    fn mixed_case_embedded_path_compacts_without_secondary_target() {
        let locations = [];
        let presentation = ToolPresentation::new(
            r"🔎 Searching C:\Project\Rust-App",
            "Completed",
            ToolCallKind::Search,
            Some(r"c:\project\rust-app"),
            false,
            None,
            None,
            &locations,
        );

        assert_eq!(presentation.display_title(), "🔎 Searching rust-app");
        assert!(presentation.title_contains_target());
        assert_eq!(presentation.secondary_target(), None);
    }

    #[test]
    fn command_targets_remain_case_sensitive() {
        let locations = [];
        let presentation = ToolPresentation::new(
            "Run INDIVIDUAL_ANCHORED_TOOL",
            "Completed",
            ToolCallKind::Execute,
            Some("run individual"),
            true,
            None,
            Some(0),
            &locations,
        );

        assert_eq!(presentation.display_title(), "Run INDIVIDUAL_ANCHORED_TOOL");
        assert!(!presentation.title_contains_target());
    }

    #[test]
    fn unmatched_title_remains_borrowed() {
        let locations = [];
        let presentation = ToolPresentation::new(
            "Search source",
            "Completed",
            ToolCallKind::Search,
            Some(r"C:\project\rust-app"),
            false,
            None,
            None,
            &locations,
        );

        assert!(matches!(presentation.display_title(), Cow::Borrowed(_)));
    }

    #[test]
    fn visible_exit_code_avoids_failed_detail_duplication() {
        let locations = [];
        let with_detail = ToolPresentation::new(
            "Run tests",
            "Failed: exit code 1",
            ToolCallKind::Execute,
            Some("cargo test"),
            true,
            None,
            Some(1),
            &locations,
        );
        let without_detail = ToolPresentation::new(
            "Run tests",
            "Failed",
            ToolCallKind::Execute,
            Some("cargo test"),
            true,
            None,
            Some(1),
            &locations,
        );
        let exited = ToolPresentation::new(
            "Run tests",
            "Exited (2)",
            ToolCallKind::Execute,
            Some("cargo test"),
            true,
            None,
            Some(2),
            &locations,
        );

        assert_eq!(with_detail.visible_exit_code(), None);
        assert_eq!(without_detail.visible_exit_code(), Some(1));
        assert_eq!(exited.visible_exit_code(), None);
    }

    #[test]
    fn only_short_single_commands_fit_compact_headers() {
        let locations = [];
        let short = ToolPresentation::new(
            "Run tests",
            "Completed",
            ToolCallKind::Execute,
            Some("cargo test"),
            true,
            None,
            Some(0),
            &locations,
        );
        let long = ToolPresentation::new(
            "Resolve cargo",
            "Completed",
            ToolCallKind::Execute,
            Some("wta.exe resolve-command cargo --shell pwsh --cwd C:\\project --json"),
            true,
            None,
            Some(0),
            &locations,
        );

        assert_eq!(short.inline_compact_command(), Some("cargo test"));
        assert_eq!(long.inline_compact_command(), None);
    }

    #[test]
    fn every_tool_kind_has_a_persistent_label() {
        let _locale = crate::test_support::lock_locale();
        rust_i18n::set_locale("en-US");
        let locations = [];
        let expected = [
            (ToolCallKind::Execute, "Run"),
            (ToolCallKind::Read, "Read"),
            (ToolCallKind::Search, "Search"),
            (ToolCallKind::Edit, "Edit"),
            (ToolCallKind::Delete, "Delete"),
            (ToolCallKind::Move, "Move"),
            (ToolCallKind::Fetch, "Fetch"),
            (ToolCallKind::Think, "Think"),
            (ToolCallKind::SwitchMode, "Mode"),
            (ToolCallKind::Other, "Tool"),
        ];

        for (kind, label) in expected {
            let presentation = ToolPresentation::new(
                "Provider title",
                "Completed",
                kind,
                None,
                false,
                None,
                None,
                &locations,
            );
            assert_eq!(presentation.kind_label(), label);
        }
    }
}
