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
