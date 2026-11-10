use super::{InstallerKind, NativeProvider};

pub(super) static ADAPTER: NativeProvider = NativeProvider {
    id: crate::agent_registry::COPILOT_AGENT_ID,
    installer: InstallerKind::CopilotCli,
};
