use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PaneContext {
    pub pane_id: Option<String>,
    pub tab_id: Option<String>,
    pub window_id: Option<String>,
    pub cwd: Option<String>,
    pub source_pane_id: Option<String>,
}

impl PaneContext {
    pub fn effective_source_pane_id(&self) -> Option<&str> {
        self.source_pane_id.as_deref().or(self.pane_id.as_deref())
    }
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
}
