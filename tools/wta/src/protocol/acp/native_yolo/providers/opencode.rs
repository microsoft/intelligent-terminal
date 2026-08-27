use super::{NativeYoloProvider, ProviderSpec};

pub(super) struct OpenCodeYoloProvider;

pub(super) static ADAPTER: OpenCodeYoloProvider = OpenCodeYoloProvider;

impl NativeYoloProvider for OpenCodeYoloProvider {
    fn family_id(&self) -> &'static str {
        crate::agent_registry::OPENCODE_AGENT_ID
    }

    fn spec(&self) -> Option<ProviderSpec> {
        None
    }
}
