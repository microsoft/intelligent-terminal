mod claude;
mod codex;
mod copilot;
mod gemini;
mod opencode;

use agent_client_protocol as acp;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum NativeYoloAction {
    SetConfigOption { config_id: String, value: String },
    SetMode { mode_id: String },
    Noop,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum NativeYoloChannel {
    ConfigOption {
        config_id: String,
        enable_value: String,
        restore_value: String,
        current_value: String,
    },
    Mode {
        enable_mode_id: String,
        restore_mode_id: String,
        current_mode_id: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ProviderSessionState {
    Available(NativeYoloChannel),
    UnrestorablePrivileged,
    MissingCapability { loaded: bool },
    Unsupported,
}

pub(super) enum ChannelDiscovery {
    Available(NativeYoloChannel),
    UnrestorablePrivileged,
    Missing,
}

impl ChannelDiscovery {
    pub(super) fn or_else(self, discover: impl FnOnce() -> Self) -> Self {
        match self {
            available @ Self::Available(_) => available,
            unrestorable @ Self::UnrestorablePrivileged => match discover() {
                available @ Self::Available(_) => available,
                _ => unrestorable,
            },
            Self::Missing => discover(),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct ConfigSpec {
    pub id: &'static str,
    pub category: &'static str,
    pub enable_value: &'static str,
    pub default_restore_value: &'static str,
}

#[derive(Clone, Copy)]
pub(super) struct ModeSpec {
    pub enable_mode_id: &'static str,
    pub default_restore_mode_id: &'static str,
}

#[derive(Clone, Copy)]
pub(super) struct DiscoveryInput<'a> {
    pub config_options: Option<&'a [acp::schema::v1::SessionConfigOption]>,
    pub modes: Option<&'a acp::schema::v1::SessionModeState>,
    pub previous: Option<&'a ProviderSessionState>,
    pub loaded: bool,
}

pub(super) trait NativeYoloProvider: Sync {
    fn family_id(&self) -> &'static str;
    fn discover(&self, input: DiscoveryInput<'_>) -> ProviderSessionState;
    fn enable(&self, state: &ProviderSessionState) -> Result<NativeYoloAction, String>;
    fn disable(&self, state: &ProviderSessionState) -> Result<NativeYoloAction, String>;

    fn disabled_prompt_block_reason(&self, _agent_version: Option<&str>) -> Option<String> {
        None
    }

    fn is_privileged_agent_command(&self, _command_name: &str) -> bool {
        false
    }

    fn refresh_config(
        &self,
        _config_options: &[acp::schema::v1::SessionConfigOption],
        _previous: &ProviderSessionState,
    ) -> ChannelDiscovery {
        ChannelDiscovery::Missing
    }

    fn observe_current_mode(&self, _state: &mut ProviderSessionState, _current_mode_id: &str) {}
}

static PROVIDERS: [&dyn NativeYoloProvider; 5] = [
    &copilot::ADAPTER,
    &claude::ADAPTER,
    &codex::ADAPTER,
    &gemini::ADAPTER,
    &opencode::ADAPTER,
];

pub(super) fn lookup(family_id: &str) -> Option<&'static dyn NativeYoloProvider> {
    PROVIDERS
        .iter()
        .copied()
        .find(|provider| provider.family_id() == family_id)
}

pub(super) fn available_or_missing(
    discovery: ChannelDiscovery,
    loaded: bool,
) -> ProviderSessionState {
    match discovery {
        ChannelDiscovery::Available(channel) => ProviderSessionState::Available(channel),
        ChannelDiscovery::UnrestorablePrivileged => ProviderSessionState::UnrestorablePrivileged,
        ChannelDiscovery::Missing => ProviderSessionState::MissingCapability { loaded },
    }
}

pub(super) fn config_channel(
    options: Option<&[acp::schema::v1::SessionConfigOption]>,
    spec: ConfigSpec,
    previous: Option<&ProviderSessionState>,
) -> ChannelDiscovery {
    let Some(option) = options.and_then(|options| {
        options.iter().find(|option| {
            option.id.0.as_ref() == spec.id
                && category_name(option.category.as_ref()).as_deref() == Some(spec.category)
        })
    }) else {
        return ChannelDiscovery::Missing;
    };
    let acp::schema::v1::SessionConfigKind::Select(select) = &option.kind else {
        return ChannelDiscovery::Missing;
    };
    let current_value = select.current_value.0.as_ref();
    let Some(values) = select_values(select) else {
        return if current_value == spec.enable_value {
            ChannelDiscovery::UnrestorablePrivileged
        } else {
            ChannelDiscovery::Missing
        };
    };
    if !values.contains(&spec.enable_value) || !values.contains(&current_value) {
        return if current_value == spec.enable_value {
            ChannelDiscovery::UnrestorablePrivileged
        } else {
            ChannelDiscovery::Missing
        };
    }
    let restore_value = if current_value == spec.enable_value {
        previous_restore_config(previous, spec.id, spec.enable_value)
            .filter(|value| values.contains(&value.as_str()))
            .or_else(|| {
                values
                    .contains(&spec.default_restore_value)
                    .then(|| spec.default_restore_value.to_string())
            })
    } else {
        Some(current_value.to_string())
    };
    match restore_value {
        Some(restore_value) => ChannelDiscovery::Available(NativeYoloChannel::ConfigOption {
            config_id: spec.id.to_string(),
            enable_value: spec.enable_value.to_string(),
            restore_value,
            current_value: current_value.to_string(),
        }),
        None => ChannelDiscovery::UnrestorablePrivileged,
    }
}

pub(super) fn mode_channel(
    state: Option<&acp::schema::v1::SessionModeState>,
    spec: ModeSpec,
    previous: Option<&ProviderSessionState>,
) -> ChannelDiscovery {
    let Some(state) = state else {
        return ChannelDiscovery::Missing;
    };
    let available = state
        .available_modes
        .iter()
        .map(|mode| mode.id.0.as_ref())
        .collect::<Vec<_>>();
    let current_mode_id = state.current_mode_id.0.as_ref();
    if !available.contains(&spec.enable_mode_id) || !available.contains(&current_mode_id) {
        return if current_mode_id == spec.enable_mode_id {
            ChannelDiscovery::UnrestorablePrivileged
        } else {
            ChannelDiscovery::Missing
        };
    }
    let restore_mode_id = if current_mode_id == spec.enable_mode_id {
        previous_restore_mode(previous, spec.enable_mode_id)
            .filter(|value| available.contains(&value.as_str()))
            .or_else(|| {
                available
                    .contains(&spec.default_restore_mode_id)
                    .then(|| spec.default_restore_mode_id.to_string())
            })
    } else {
        Some(current_mode_id.to_string())
    };
    match restore_mode_id {
        Some(restore_mode_id) => ChannelDiscovery::Available(NativeYoloChannel::Mode {
            enable_mode_id: spec.enable_mode_id.to_string(),
            restore_mode_id,
            current_mode_id: current_mode_id.to_string(),
        }),
        None => ChannelDiscovery::UnrestorablePrivileged,
    }
}

pub(super) fn config_action(
    state: &ProviderSessionState,
    enabled: bool,
) -> Option<NativeYoloAction> {
    let ProviderSessionState::Available(NativeYoloChannel::ConfigOption {
        config_id,
        enable_value,
        restore_value,
        ..
    }) = state
    else {
        return None;
    };
    Some(NativeYoloAction::SetConfigOption {
        config_id: config_id.clone(),
        value: if enabled { enable_value } else { restore_value }.clone(),
    })
}

pub(super) fn mode_action(state: &ProviderSessionState, enabled: bool) -> Option<NativeYoloAction> {
    let ProviderSessionState::Available(NativeYoloChannel::Mode {
        enable_mode_id,
        restore_mode_id,
        ..
    }) = state
    else {
        return None;
    };
    Some(NativeYoloAction::SetMode {
        mode_id: if enabled {
            enable_mode_id
        } else {
            restore_mode_id
        }
        .clone(),
    })
}

pub(super) fn finish_supported_action(
    provider: &str,
    state: &ProviderSessionState,
    action: Option<NativeYoloAction>,
    enabled: bool,
) -> Result<NativeYoloAction, String> {
    if let Some(action) = action {
        return Ok(action);
    }
    if matches!(state, ProviderSessionState::UnrestorablePrivileged) {
        return Err(format!(
            "{provider} is already in its ACP session Yolo mode without an advertised restore value"
        ));
    }
    if !enabled
        && matches!(
            state,
            ProviderSessionState::MissingCapability { loaded: false }
        )
    {
        return Ok(NativeYoloAction::Noop);
    }
    Err(format!(
        "{provider} did not advertise its expected ACP session Yolo capability"
    ))
}

pub(super) fn unsupported_action(
    provider: &str,
    enabled: bool,
) -> Result<NativeYoloAction, String> {
    if enabled {
        Err(format!("{provider} does not support ACP session Yolo mode"))
    } else {
        Ok(NativeYoloAction::Noop)
    }
}

pub(super) fn observe_mode(
    state: &mut ProviderSessionState,
    current_mode_id: &str,
    spec: ModeSpec,
) {
    match state {
        ProviderSessionState::Available(NativeYoloChannel::Mode {
            enable_mode_id,
            restore_mode_id,
            current_mode_id: known_current,
        }) => {
            if current_mode_id != enable_mode_id {
                *restore_mode_id = current_mode_id.to_string();
            }
            *known_current = current_mode_id.to_string();
        }
        ProviderSessionState::Available(NativeYoloChannel::ConfigOption {
            enable_value,
            restore_value,
            current_value,
            ..
        }) => {
            if current_mode_id != enable_value {
                *restore_value = current_mode_id.to_string();
            }
            *current_value = current_mode_id.to_string();
        }
        ProviderSessionState::UnrestorablePrivileged if current_mode_id != spec.enable_mode_id => {
            *state = ProviderSessionState::Available(NativeYoloChannel::Mode {
                enable_mode_id: spec.enable_mode_id.to_string(),
                restore_mode_id: current_mode_id.to_string(),
                current_mode_id: current_mode_id.to_string(),
            });
        }
        ProviderSessionState::UnrestorablePrivileged
        | ProviderSessionState::MissingCapability { .. }
        | ProviderSessionState::Unsupported => {}
    }
}

fn previous_restore_config(
    previous: Option<&ProviderSessionState>,
    config_id: &str,
    enable_value: &str,
) -> Option<String> {
    match previous {
        Some(ProviderSessionState::Available(NativeYoloChannel::ConfigOption {
            config_id: previous_id,
            restore_value,
            ..
        })) if previous_id == config_id && restore_value != enable_value => {
            Some(restore_value.clone())
        }
        Some(ProviderSessionState::Available(NativeYoloChannel::Mode {
            enable_mode_id,
            restore_mode_id,
            ..
        })) if enable_mode_id == enable_value && restore_mode_id != enable_value => {
            Some(restore_mode_id.clone())
        }
        _ => None,
    }
}

fn previous_restore_mode(
    previous: Option<&ProviderSessionState>,
    enable_mode_id: &str,
) -> Option<String> {
    match previous {
        Some(ProviderSessionState::Available(NativeYoloChannel::Mode {
            enable_mode_id: previous_id,
            restore_mode_id,
            ..
        })) if previous_id == enable_mode_id && restore_mode_id != enable_mode_id => {
            Some(restore_mode_id.clone())
        }
        Some(ProviderSessionState::Available(NativeYoloChannel::ConfigOption {
            enable_value,
            restore_value,
            ..
        })) if enable_value == enable_mode_id && restore_value != enable_mode_id => {
            Some(restore_value.clone())
        }
        _ => None,
    }
}

fn select_values(select: &acp::schema::v1::SessionConfigSelect) -> Option<Vec<&str>> {
    match &select.options {
        acp::schema::v1::SessionConfigSelectOptions::Ungrouped(options) => Some(
            options
                .iter()
                .map(|option| option.value.0.as_ref())
                .collect(),
        ),
        acp::schema::v1::SessionConfigSelectOptions::Grouped(groups) => Some(
            groups
                .iter()
                .flat_map(|group| group.options.iter())
                .map(|option| option.value.0.as_ref())
                .collect(),
        ),
        _ => None,
    }
}

fn category_name(
    category: Option<&acp::schema::v1::SessionConfigOptionCategory>,
) -> Option<String> {
    serde_json::to_value(category?)
        .ok()?
        .as_str()
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_covers_every_known_agent_family() {
        let mut registered = PROVIDERS
            .iter()
            .map(|provider| provider.family_id())
            .collect::<Vec<_>>();
        registered.sort_unstable();

        let mut known = crate::agent_registry::KNOWN_AGENTS
            .iter()
            .map(|profile| profile.id)
            .collect::<Vec<_>>();
        known.sort_unstable();

        assert_eq!(registered, known);
    }

    #[test]
    fn opencode_explicitly_declares_yolo_unsupported() {
        let provider = lookup(crate::agent_registry::OPENCODE_AGENT_ID).unwrap();
        let state = provider.discover(DiscoveryInput {
            config_options: None,
            modes: None,
            previous: None,
            loaded: false,
        });

        assert_eq!(state, ProviderSessionState::Unsupported);
        assert_eq!(provider.disable(&state), Ok(NativeYoloAction::Noop));
        assert_eq!(
            provider.enable(&state),
            Err("opencode does not support ACP session Yolo mode".to_string())
        );
    }

    #[test]
    fn each_supported_provider_owns_its_enable_and_disable_transition() {
        let cases = [
            (
                crate::agent_registry::COPILOT_AGENT_ID,
                ProviderSessionState::Available(NativeYoloChannel::ConfigOption {
                    config_id: "allow_all".to_string(),
                    enable_value: "on".to_string(),
                    restore_value: "off".to_string(),
                    current_value: "off".to_string(),
                }),
                NativeYoloAction::SetConfigOption {
                    config_id: "allow_all".to_string(),
                    value: "on".to_string(),
                },
                NativeYoloAction::SetConfigOption {
                    config_id: "allow_all".to_string(),
                    value: "off".to_string(),
                },
            ),
            (
                crate::agent_registry::CLAUDE_AGENT_ID,
                ProviderSessionState::Available(NativeYoloChannel::ConfigOption {
                    config_id: "mode".to_string(),
                    enable_value: "bypassPermissions".to_string(),
                    restore_value: "default".to_string(),
                    current_value: "default".to_string(),
                }),
                NativeYoloAction::SetConfigOption {
                    config_id: "mode".to_string(),
                    value: "bypassPermissions".to_string(),
                },
                NativeYoloAction::SetConfigOption {
                    config_id: "mode".to_string(),
                    value: "default".to_string(),
                },
            ),
            (
                crate::agent_registry::CODEX_AGENT_ID,
                ProviderSessionState::Available(NativeYoloChannel::ConfigOption {
                    config_id: "mode".to_string(),
                    enable_value: "agent-full-access".to_string(),
                    restore_value: "agent".to_string(),
                    current_value: "agent".to_string(),
                }),
                NativeYoloAction::SetConfigOption {
                    config_id: "mode".to_string(),
                    value: "agent-full-access".to_string(),
                },
                NativeYoloAction::SetConfigOption {
                    config_id: "mode".to_string(),
                    value: "agent".to_string(),
                },
            ),
            (
                crate::agent_registry::GEMINI_AGENT_ID,
                ProviderSessionState::Available(NativeYoloChannel::Mode {
                    enable_mode_id: "yolo".to_string(),
                    restore_mode_id: "default".to_string(),
                    current_mode_id: "default".to_string(),
                }),
                NativeYoloAction::SetMode {
                    mode_id: "yolo".to_string(),
                },
                NativeYoloAction::SetMode {
                    mode_id: "default".to_string(),
                },
            ),
        ];

        for (family_id, state, enable, disable) in cases {
            let provider = lookup(family_id).unwrap();
            assert_eq!(provider.enable(&state), Ok(enable), "{family_id} enable");
            assert_eq!(provider.disable(&state), Ok(disable), "{family_id} disable");
        }
    }
}
