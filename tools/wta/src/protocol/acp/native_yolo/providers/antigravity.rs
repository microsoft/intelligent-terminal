use super::{
    available_or_missing, config_action, config_channel, find_config_control,
    finish_supported_action, ConfigSpec, DiscoveryInput, NativeConfigControl, NativeYoloAction,
    NativeYoloProvider, ProviderSessionState,
};

const CONFIG: ConfigSpec = ConfigSpec {
    id: "mode",
    category: "mode",
    enable_value: "yolo",
    default_restore_value: "default",
    require_default_restore_value: true,
};

pub(super) struct AntigravityYoloProvider;

pub(super) static ADAPTER: AntigravityYoloProvider = AntigravityYoloProvider;

impl NativeYoloProvider for AntigravityYoloProvider {
    fn family_id(&self) -> &'static str {
        crate::agent_registry::ANTIGRAVITY_AGENT_ID
    }

    fn discover(&self, input: DiscoveryInput<'_>) -> ProviderSessionState {
        available_or_missing(
            config_channel(input.config_options, CONFIG, input.previous),
            input.loaded,
        )
    }

    fn enable(&self, state: &ProviderSessionState) -> Result<NativeYoloAction, String> {
        finish_supported_action(self.family_id(), state, config_action(state, true), true)
    }

    fn disable(&self, state: &ProviderSessionState) -> Result<NativeYoloAction, String> {
        finish_supported_action(self.family_id(), state, config_action(state, false), false)
    }

    fn config_control(
        &self,
        options: Option<&[agent_client_protocol::schema::v1::SessionConfigOption]>,
    ) -> Option<NativeConfigControl> {
        find_config_control(options, CONFIG)
    }

    fn refresh_config(
        &self,
        options: &[agent_client_protocol::schema::v1::SessionConfigOption],
        previous: &ProviderSessionState,
    ) -> super::ChannelDiscovery {
        config_channel(Some(options), CONFIG, Some(previous))
    }
}
