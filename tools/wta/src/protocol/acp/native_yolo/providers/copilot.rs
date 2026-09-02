use super::{
    available_or_missing, config_action, config_channel, finish_supported_action, ConfigSpec,
    DiscoveryInput, NativeYoloAction, NativeYoloProvider, ProviderSessionState,
};

const CONFIG: ConfigSpec = ConfigSpec {
    id: "allow_all",
    category: "permissions",
    enable_value: "on",
    default_restore_value: "off",
};

pub(super) struct CopilotYoloProvider;

pub(super) static ADAPTER: CopilotYoloProvider = CopilotYoloProvider;

impl NativeYoloProvider for CopilotYoloProvider {
    fn family_id(&self) -> &'static str {
        crate::agent_registry::COPILOT_AGENT_ID
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

    fn is_privileged_agent_command(&self, command_name: &str) -> bool {
        command_name.eq_ignore_ascii_case("allow_all")
    }

    fn refresh_config(
        &self,
        config_options: &[agent_client_protocol::schema::v1::SessionConfigOption],
        previous: &ProviderSessionState,
    ) -> super::ChannelDiscovery {
        config_channel(Some(config_options), CONFIG, Some(previous))
    }
}
