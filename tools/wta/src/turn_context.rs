#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TurnContext {
    /// Host-resolved pane whose terminal context was used for this turn.
    pub target_pane_id: Option<String>,
}

impl TurnContext {
    pub fn with_target_pane(target_pane_id: impl Into<String>) -> Self {
        let target_pane_id = target_pane_id.into();
        Self {
            target_pane_id: (!target_pane_id.trim().is_empty()).then_some(target_pane_id),
        }
    }

    pub fn target_pane_id(&self) -> Option<&str> {
        self.target_pane_id
            .as_deref()
            .map(str::trim)
            .filter(|pane_id| !pane_id.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_pane_treats_blank_values_as_unresolved() {
        assert_eq!(TurnContext::with_target_pane("").target_pane_id(), None);
        assert_eq!(
            TurnContext {
                target_pane_id: Some("  ".into()),
            }
            .target_pane_id(),
            None
        );
    }
}
