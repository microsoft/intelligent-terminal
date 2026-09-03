use super::{
    available_or_missing, config_action, config_channel, find_config_control,
    finish_supported_action, ConfigSpec, DiscoveryInput, NativeConfigControl, NativeYoloAction,
    NativeYoloProvider, ProviderSessionState,
};

const CONFIG: ConfigSpec = ConfigSpec {
    id: "allow_all",
    category: "permissions",
    enable_value: "on",
    default_restore_value: "off",
    require_default_restore_value: true,
};

pub(super) struct CopilotYoloProvider;

pub(super) static ADAPTER: CopilotYoloProvider = CopilotYoloProvider;

// github/copilot-cli#4537 starts at 1.0.81-1 and remains open. Keep the
// upper bound open until a later release passes the denied-permission probe.
fn has_acp_permission_regression(version: Option<&str>) -> bool {
    let Some(version) = version else {
        return true;
    };
    let version = version.trim();
    let version = version.strip_prefix('v').unwrap_or(version);
    let (core, prerelease) = version
        .split_once('-')
        .map_or((version, None), |(core, prerelease)| {
            (core, Some(prerelease))
        });
    let mut parts = core.split('.');
    let parsed = (
        parts.next().and_then(|part| part.parse::<u64>().ok()),
        parts.next().and_then(|part| part.parse::<u64>().ok()),
        parts.next().and_then(|part| part.parse::<u64>().ok()),
        parts.next(),
    );
    let (Some(major), Some(minor), Some(patch), None) = parsed else {
        return true;
    };
    match (major, minor, patch).cmp(&(1, 0, 81)) {
        std::cmp::Ordering::Less => false,
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Equal => match prerelease {
            Some(prerelease) => prerelease
                .split('.')
                .next()
                .and_then(|part| part.parse::<u64>().ok())
                .is_none_or(|build| build >= 1),
            None => true,
        },
    }
}

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

    fn disabled_prompt_block_reason(&self, agent_version: Option<&str>) -> Option<String> {
        has_acp_permission_regression(agent_version).then(|| {
            let version = agent_version.unwrap_or("unknown");
            format!(
                "GitHub Copilot CLI {version} cannot enforce ACP permission requests while Yolo is off because of github/copilot-cli#4537. Use Copilot CLI 1.0.81-0 or earlier, or explicitly enable Yolo if organization policy permits."
            )
        })
    }

    fn config_control(
        &self,
        config_options: Option<&[agent_client_protocol::schema::v1::SessionConfigOption]>,
    ) -> Option<NativeConfigControl> {
        find_config_control(config_options, CONFIG)
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
