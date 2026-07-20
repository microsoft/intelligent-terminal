//! Canonical ACP proxy-chain construction for one pooled agent instance.

use agent_client_protocol as acp;
use agent_client_protocol_conductor::{AgentOnly, ConductorImpl};

/// Build the behavior-preserving first stage of the proxy migration.
///
/// The chain has no configured transform proxies, so every ACP message remains
/// unchanged while initialization and routing run through the canonical
/// conductor implementation.
pub(super) fn transparent(
    final_agent: impl acp::ConnectTo<acp::Client> + 'static,
) -> ConductorImpl<acp::Agent> {
    ConductorImpl::new_agent("wta-master", AgentOnly(final_agent))
}
