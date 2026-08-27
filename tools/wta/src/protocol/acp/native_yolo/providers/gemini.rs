use super::{ModeSpec, NativeYoloProvider, ProviderSpec};

pub(super) struct GeminiYoloProvider;

pub(super) static ADAPTER: GeminiYoloProvider = GeminiYoloProvider;

impl NativeYoloProvider for GeminiYoloProvider {
    fn family_id(&self) -> &'static str {
        crate::agent_registry::GEMINI_AGENT_ID
    }

    fn spec(&self) -> Option<ProviderSpec> {
        Some(ProviderSpec {
            config: None,
            mode: Some(ModeSpec {
                enable_mode_id: "yolo",
                default_restore_mode_id: "default",
            }),
        })
    }
}
