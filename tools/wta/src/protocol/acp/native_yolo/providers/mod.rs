mod claude;
mod codex;
mod copilot;
mod gemini;
mod opencode;

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
pub(super) struct ProviderSpec {
    pub config: Option<ConfigSpec>,
    pub mode: Option<ModeSpec>,
}

pub(super) trait NativeYoloProvider: Sync {
    fn family_id(&self) -> &'static str;
    fn spec(&self) -> Option<ProviderSpec>;
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
        assert!(lookup(crate::agent_registry::OPENCODE_AGENT_ID)
            .unwrap()
            .spec()
            .is_none());
    }
}
