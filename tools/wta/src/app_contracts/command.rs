#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionBehavior {
    ExecuteImmediately,
    OpenPicker,
    RequireFreeText,
    OptionalFreeText,
}

impl CompletionBehavior {
    pub fn prepares_free_text(self) -> bool {
        matches!(self, Self::RequireFreeText | Self::OptionalFreeText)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpSessionCommand {
    pub name: String,
    pub description: String,
    pub input_hint: Option<String>,
    pub completion_behavior: CompletionBehavior,
}
