use super::{ConfigSpec, ModeSpec, NativeYoloProvider, ProviderSpec};

pub(super) struct CodexYoloProvider;

pub(super) static ADAPTER: CodexYoloProvider = CodexYoloProvider;

impl NativeYoloProvider for CodexYoloProvider {
    fn family_id(&self) -> &'static str {
        crate::agent_registry::CODEX_AGENT_ID
    }

    fn spec(&self) -> Option<ProviderSpec> {
        Some(ProviderSpec {
            config: Some(ConfigSpec {
                id: "mode",
                category: "mode",
                enable_value: "agent-full-access",
                default_restore_value: "agent",
            }),
            mode: Some(ModeSpec {
                enable_mode_id: "agent-full-access",
                default_restore_mode_id: "agent",
            }),
        })
    }
}
