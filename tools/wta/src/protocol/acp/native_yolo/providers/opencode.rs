use super::{
    unsupported_action, DiscoveryInput, NativeYoloAction, NativeYoloProvider, ProviderSessionState,
};

pub(super) struct OpenCodeYoloProvider;

pub(super) static ADAPTER: OpenCodeYoloProvider = OpenCodeYoloProvider;

impl NativeYoloProvider for OpenCodeYoloProvider {
    fn family_id(&self) -> &'static str {
        crate::agent_registry::OPENCODE_AGENT_ID
    }

    fn discover(&self, _input: DiscoveryInput<'_>) -> ProviderSessionState {
        ProviderSessionState::Unsupported
    }

    fn enable(&self, _state: &ProviderSessionState) -> Result<NativeYoloAction, String> {
        unsupported_action(self.family_id(), true)
    }

    fn disable(&self, _state: &ProviderSessionState) -> Result<NativeYoloAction, String> {
        unsupported_action(self.family_id(), false)
    }
}
