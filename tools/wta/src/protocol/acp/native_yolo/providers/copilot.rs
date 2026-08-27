use super::{ConfigSpec, NativeYoloProvider, ProviderSpec};

pub(super) struct CopilotYoloProvider;

pub(super) static ADAPTER: CopilotYoloProvider = CopilotYoloProvider;

impl NativeYoloProvider for CopilotYoloProvider {
    fn family_id(&self) -> &'static str {
        crate::agent_registry::COPILOT_AGENT_ID
    }

    fn spec(&self) -> Option<ProviderSpec> {
        Some(ProviderSpec {
            config: Some(ConfigSpec {
                id: "allow_all",
                category: "permissions",
                enable_value: "on",
                default_restore_value: "off",
            }),
            mode: None,
        })
    }
}
