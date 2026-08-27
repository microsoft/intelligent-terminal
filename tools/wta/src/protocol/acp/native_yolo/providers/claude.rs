use super::{ConfigSpec, ModeSpec, NativeYoloProvider, ProviderSpec};

pub(super) struct ClaudeYoloProvider;

pub(super) static ADAPTER: ClaudeYoloProvider = ClaudeYoloProvider;

impl NativeYoloProvider for ClaudeYoloProvider {
    fn family_id(&self) -> &'static str {
        crate::agent_registry::CLAUDE_AGENT_ID
    }

    fn spec(&self) -> Option<ProviderSpec> {
        Some(ProviderSpec {
            config: Some(ConfigSpec {
                id: "mode",
                category: "mode",
                enable_value: "bypassPermissions",
                default_restore_value: "default",
            }),
            mode: Some(ModeSpec {
                enable_mode_id: "bypassPermissions",
                default_restore_mode_id: "default",
            }),
        })
    }
}
