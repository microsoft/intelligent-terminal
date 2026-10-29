mod antigravity;
mod claude;
mod codex;
mod copilot;
mod gemini;
mod opencode;

use anyhow::Result;
use futures::future::BoxFuture;

use crate::agent_source::AgentSource;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DependencyKind {
    NativeCli,
    NpmAdapter,
    StandaloneAcp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreparationOutcome {
    NotRequired,
    DeferredToLaunch,
    Ready,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedArtifact {
    pub outcome: PreparationOutcome,
    pub executable: Option<String>,
}

impl From<PreparationOutcome> for PreparedArtifact {
    fn from(outcome: PreparationOutcome) -> Self {
        Self {
            outcome,
            executable: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InstallerKind {
    Manual,
    CopilotCli,
    StandaloneAcp,
}

pub(crate) trait ProviderLifecycleAdapter: Sync {
    fn family_id(&self) -> &'static str;
    fn dependency_kind(&self) -> DependencyKind;
    fn installer_kind(&self) -> InstallerKind {
        InstallerKind::Manual
    }
    fn find_host_acp(&self) -> Option<String> {
        crate::agent_check::find_exe(self.family_id())
    }
    fn host_bootstrap_available(&self, _launch_ready: bool) -> bool {
        false
    }
    fn prepare<'a>(
        &'a self,
        _command: &'a str,
        _source: &'a AgentSource,
    ) -> BoxFuture<'a, Result<PreparedArtifact>> {
        Box::pin(async move {
            Ok(match self.dependency_kind() {
                DependencyKind::NativeCli => PreparationOutcome::NotRequired,
                DependencyKind::NpmAdapter => PreparationOutcome::DeferredToLaunch,
                DependencyKind::StandaloneAcp => {
                    anyhow::bail!("standalone ACP provider has no preparation implementation")
                }
            }
            .into())
        })
    }
}

pub(super) struct NativeProvider {
    pub id: &'static str,
    pub installer: InstallerKind,
}

impl ProviderLifecycleAdapter for NativeProvider {
    fn family_id(&self) -> &'static str {
        self.id
    }

    fn dependency_kind(&self) -> DependencyKind {
        if crate::agent_registry::lookup_profile_by_id(self.id)
            .acp_launch_command
            .starts_with("npx ")
        {
            DependencyKind::NpmAdapter
        } else {
            DependencyKind::NativeCli
        }
    }

    fn installer_kind(&self) -> InstallerKind {
        self.installer
    }
}

static PROVIDERS: [&dyn ProviderLifecycleAdapter; 6] = [
    &copilot::ADAPTER,
    &claude::ADAPTER,
    &codex::ADAPTER,
    &gemini::ADAPTER,
    &opencode::ADAPTER,
    &antigravity::ADAPTER,
];

pub(crate) fn lookup(agent_id: &str) -> Option<&'static dyn ProviderLifecycleAdapter> {
    PROVIDERS
        .iter()
        .copied()
        .find(|provider| provider.family_id().eq_ignore_ascii_case(agent_id))
}

pub(crate) async fn prepare(
    command: &str,
    agent_id: Option<&str>,
    source: &AgentSource,
) -> Result<PreparedArtifact> {
    let id = agent_id.unwrap_or_else(|| crate::agent_registry::resolve_agent_id_from_cmd(command));
    match lookup(id) {
        Some(provider) => provider.prepare(command, source).await,
        None => Ok(PreparationOutcome::NotRequired.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_adapters_cover_the_registry_without_owning_custom_commands() {
        for profile in crate::agent_registry::KNOWN_AGENTS {
            let provider = lookup(profile.id).expect("every built-in has a lifecycle adapter");
            assert_eq!(provider.family_id(), profile.id);
            assert_eq!(
                provider.dependency_kind() == DependencyKind::NpmAdapter,
                profile.acp_launch_command.starts_with("npx ")
            );
        }
        assert_eq!(PROVIDERS.len(), crate::agent_registry::KNOWN_AGENTS.len());
        assert!(lookup("custom:antigravity").is_none());
        assert!(lookup("unknown").is_none());
    }

    #[tokio::test]
    async fn native_and_adapter_preparation_preserve_the_existing_launch_contracts() {
        for profile in crate::agent_registry::KNOWN_AGENTS {
            if profile.id == crate::agent_registry::ANTIGRAVITY_AGENT_ID {
                continue;
            }
            let expected = if profile.acp_launch_command.starts_with("npx ") {
                PreparationOutcome::DeferredToLaunch
            } else {
                PreparationOutcome::NotRequired
            };
            assert_eq!(
                prepare(
                    &crate::agent_registry::build_acp_command(profile.id, None),
                    Some(profile.id),
                    &AgentSource::Host,
                )
                .await
                .unwrap()
                .outcome,
                expected
            );
        }
        assert_eq!(
            prepare(
                "agy_acp_server.exe",
                Some("custom:antigravity"),
                &AgentSource::Host,
            )
            .await
            .unwrap()
            .outcome,
            PreparationOutcome::NotRequired
        );
    }
}
