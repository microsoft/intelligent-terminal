use super::{InstallerKind, NativeProvider};

pub(super) static ADAPTER: NativeProvider = NativeProvider {
    id: crate::agent_registry::GEMINI_AGENT_ID,
    installer: InstallerKind::Manual,
};
