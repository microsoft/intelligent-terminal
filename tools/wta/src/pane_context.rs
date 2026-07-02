use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PaneContext {
    pub pane_id: Option<String>,
    pub tab_id: Option<String>,
    pub window_id: Option<String>,
    pub cwd: Option<String>,
    pub source_pane_id: Option<String>,
    /// Raw `@`-mention tokens from the user's message (e.g. `["@1", "@build"]`).
    /// Each token is resolved asynchronously by the ACP client task to a pane
    /// id (by index or title match), and that pane's recent output is injected
    /// into the prompt as additional context.  Empty for most prompts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub at_pane_refs: Vec<String>,
}

impl PaneContext {
    pub fn effective_source_pane_id(&self) -> Option<&str> {
        self.source_pane_id.as_deref().or(self.pane_id.as_deref())
    }
}

/// Parse all `@word` tokens from a user message string.
///
/// Returns the unique tokens in the order they first appear.  Tokens that
/// contain only digits (e.g. `@1`) are intended as 1-based pane index
/// references; all other tokens are matched against pane titles.
///
/// The `@` sigil must be immediately followed by at least one word
/// character (letter, digit, `-`, or `_`). A bare `@` or `@ ` is ignored.
pub fn extract_at_refs(text: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '@' {
            let start = i + 1;
            let mut end = start;
            // Consume word chars: letters, digits, hyphens, underscores.
            while end < chars.len()
                && (chars[end].is_alphanumeric() || chars[end] == '-' || chars[end] == '_')
            {
                end += 1;
            }
            if end > start {
                let token: String = chars[start..end].iter().collect();
                let at_token = format!("@{}", token);
                if seen.insert(at_token.clone()) {
                    result.push(at_token);
                }
            }
            i = end;
        } else {
            i += 1;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(source: Option<&str>, pane: Option<&str>) -> PaneContext {
        PaneContext {
            pane_id: pane.map(String::from),
            source_pane_id: source.map(String::from),
            ..Default::default()
        }
    }

    /// `effective_source_pane_id` prefers `source_pane_id` (the pane that
    /// actually produced the failing command) and only falls back to
    /// `pane_id` (the agent pane) when no source is recorded. Autofix routing
    /// depends on this precedence — a regression would land fixes in the wrong
    /// pane.
    #[test]
    fn effective_source_prefers_source_then_falls_back_to_pane() {
        // Both present → source wins.
        assert_eq!(ctx(Some("src"), Some("pane")).effective_source_pane_id(), Some("src"));
        // Only pane present → fall back to pane.
        assert_eq!(ctx(None, Some("pane")).effective_source_pane_id(), Some("pane"));
        // Only source present → source.
        assert_eq!(ctx(Some("src"), None).effective_source_pane_id(), Some("src"));
        // Neither → None (must not invent a target pane).
        assert_eq!(ctx(None, None).effective_source_pane_id(), None);
    }

    #[test]
    fn extract_at_refs_basic() {
        let refs = extract_at_refs("look at @pane2 and @build");
        assert_eq!(refs, vec!["@pane2", "@build"]);
    }

    #[test]
    fn extract_at_refs_index() {
        let refs = extract_at_refs("what is happening in @1?");
        assert_eq!(refs, vec!["@1"]);
    }

    #[test]
    fn extract_at_refs_deduplicates() {
        let refs = extract_at_refs("@pane1 and @pane1 again");
        assert_eq!(refs, vec!["@pane1"]);
    }

    #[test]
    fn extract_at_refs_bare_at_ignored() {
        let refs = extract_at_refs("send @ me");
        assert!(refs.is_empty());
    }

    #[test]
    fn extract_at_refs_allows_hyphens_and_underscores() {
        let refs = extract_at_refs("check @my-pane and @some_pane");
        assert_eq!(refs, vec!["@my-pane", "@some_pane"]);
    }

    #[test]
    fn extract_at_refs_empty_input() {
        assert!(extract_at_refs("").is_empty());
        assert!(extract_at_refs("no mentions here").is_empty());
    }
}
