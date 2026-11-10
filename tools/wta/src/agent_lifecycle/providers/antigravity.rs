use std::ffi::OsStr;
use std::path::Path;

use anyhow::Result;
use futures::future::BoxFuture;

use super::{
    DependencyKind, InstallerKind, PreparationOutcome, PreparedArtifact, ProviderLifecycleAdapter,
};
use crate::agent_source::AgentSource;

pub(super) struct AntigravityProvider;
pub(super) static ADAPTER: AntigravityProvider = AntigravityProvider;

impl ProviderLifecycleAdapter for AntigravityProvider {
    fn family_id(&self) -> &'static str {
        crate::agent_registry::ANTIGRAVITY_AGENT_ID
    }

    fn dependency_kind(&self) -> DependencyKind {
        DependencyKind::StandaloneAcp
    }

    fn installer_kind(&self) -> InstallerKind {
        InstallerKind::StandaloneAcp
    }

    fn find_host_acp(&self) -> Option<String> {
        let profile = crate::agent_registry::lookup_profile_by_id(self.family_id());
        let path = crate::agent_check::spawn_path()
            .map(std::ffi::OsString::from)
            .or_else(|| std::env::var_os("PATH"))?;
        let resolved = crate::agent_check::find_standalone_acp_executable_in_path(
            profile,
            &path,
            Path::is_file,
        );
        if resolved.is_some() || has_server_on_path(&path) {
            return resolved.map(|candidate| candidate.to_string_lossy().into_owned());
        }
        crate::antigravity_acp::managed_host_executable()
            .map(|candidate| candidate.to_string_lossy().into_owned())
    }

    fn host_bootstrap_available(&self, launch_ready: bool) -> bool {
        crate::antigravity_acp::host_platform().is_ok()
            && !launch_ready
            && crate::agent_check::find_exe(self.family_id()).is_some()
            && !crate::agent_check::spawn_path()
                .is_some_and(|path| has_server_on_path(OsStr::new(&path)))
    }

    fn prepare<'a>(
        &'a self,
        command: &'a str,
        source: &'a AgentSource,
    ) -> BoxFuture<'a, Result<PreparedArtifact>> {
        Box::pin(async move {
            match crate::antigravity_acp::prepare(command, Some(self.family_id()), source).await? {
                Some(executable) => Ok(PreparedArtifact {
                    outcome: PreparationOutcome::Ready,
                    executable: Some(executable),
                }),
                None => Ok(PreparationOutcome::NotRequired.into()),
            }
        })
    }
}

fn has_server_on_path(path: &OsStr) -> bool {
    std::env::split_paths(path).any(|directory| directory.join("agy_acp_server.exe").is_file())
}
