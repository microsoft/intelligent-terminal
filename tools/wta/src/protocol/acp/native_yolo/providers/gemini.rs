use super::{
    available_or_missing, finish_supported_action, mode_action, mode_channel, observe_mode,
    DiscoveryInput, ModeSpec, NativeYoloAction, NativeYoloProvider, ProviderSessionState,
};

const MODE: ModeSpec = ModeSpec {
    enable_mode_id: "yolo",
    default_restore_mode_id: "default",
};

pub(super) struct GeminiYoloProvider;

pub(super) static ADAPTER: GeminiYoloProvider = GeminiYoloProvider;

impl NativeYoloProvider for GeminiYoloProvider {
    fn family_id(&self) -> &'static str {
        crate::agent_registry::GEMINI_AGENT_ID
    }

    fn discover(&self, input: DiscoveryInput<'_>) -> ProviderSessionState {
        available_or_missing(
            mode_channel(input.modes, MODE, input.previous),
            input.loaded,
        )
    }

    fn enable(&self, state: &ProviderSessionState) -> Result<NativeYoloAction, String> {
        finish_supported_action(self.family_id(), state, mode_action(state, true), true)
    }

    fn disable(&self, state: &ProviderSessionState) -> Result<NativeYoloAction, String> {
        finish_supported_action(self.family_id(), state, mode_action(state, false), false)
    }

    fn observe_current_mode(&self, state: &mut ProviderSessionState, current_mode_id: &str) {
        observe_mode(state, current_mode_id, MODE);
    }
}
