use super::{
    available_or_missing, config_action, config_channel, find_config_control,
    finish_supported_action, mode_action, mode_channel, observe_mode, ConfigSpec, DiscoveryInput,
    ModeSpec, NativeConfigControl, NativeYoloAction, NativeYoloProvider, ProviderSessionState,
};

const CONFIG: ConfigSpec = ConfigSpec {
    id: "mode",
    category: "mode",
    enable_value: "bypassPermissions",
    default_restore_value: "default",
    require_default_restore_value: false,
};

const MODE: ModeSpec = ModeSpec {
    enable_mode_id: "bypassPermissions",
    default_restore_mode_id: "default",
};

pub(super) struct ClaudeYoloProvider;

pub(super) static ADAPTER: ClaudeYoloProvider = ClaudeYoloProvider;

impl NativeYoloProvider for ClaudeYoloProvider {
    fn family_id(&self) -> &'static str {
        crate::agent_registry::CLAUDE_AGENT_ID
    }

    fn discover(&self, input: DiscoveryInput<'_>) -> ProviderSessionState {
        let channel = config_channel(input.config_options, CONFIG, input.previous)
            .or_else(|| mode_channel(input.modes, MODE, input.previous));
        available_or_missing(channel, input.loaded)
    }

    fn enable(&self, state: &ProviderSessionState) -> Result<NativeYoloAction, String> {
        let action = config_action(state, true).or_else(|| mode_action(state, true));
        finish_supported_action(self.family_id(), state, action, true)
    }

    fn disable(&self, state: &ProviderSessionState) -> Result<NativeYoloAction, String> {
        let action = config_action(state, false).or_else(|| mode_action(state, false));
        finish_supported_action(self.family_id(), state, action, false)
    }

    fn refresh_config(
        &self,
        config_options: &[agent_client_protocol::schema::v1::SessionConfigOption],
        previous: &ProviderSessionState,
    ) -> super::ChannelDiscovery {
        config_channel(Some(config_options), CONFIG, Some(previous))
    }

    fn config_control(
        &self,
        config_options: Option<&[agent_client_protocol::schema::v1::SessionConfigOption]>,
    ) -> Option<NativeConfigControl> {
        find_config_control(config_options, CONFIG)
    }

    fn observe_current_mode(&self, state: &mut ProviderSessionState, current_mode_id: &str) {
        observe_mode(state, current_mode_id, MODE);
    }
}
