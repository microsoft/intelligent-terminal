use std::ffi::OsStr;
use std::path::Path;

use anyhow::Result;
use futures::future::BoxFuture;

use super::{
    DependencyKind, InstallerKind, PreparationOutcome, PreparedArtifact, ProviderLifecycleAdapter,
};
use crate::agent_source::AgentSource;

mod runtime;

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

    fn runtime_resources(
        &self,
        source: &AgentSource,
    ) -> Result<Option<Box<dyn crate::protocol::acp::process::ProcessResources>>> {
        match source {
            AgentSource::Host => Ok(Some(Box::new(runtime::Runtime::prepare()?))),
            AgentSource::Wsl { .. } => Ok(None),
        }
    }

    fn recover_runtime_resources(&self) -> Result<()> {
        runtime::recover()
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    use crate::protocol::acp::spawn::{
        spawn_agent_process_with_provider, AgentSpawn, ChildEnvironmentPolicy,
        SharedProviderSelection,
    };

    async fn extraction_fixture(agent_id: &str) -> (AgentSpawn, PathBuf) {
        let name = format!("_MEI-wta-test-{}", uuid::Uuid::new_v4().simple());
        let command = format!(
            "pwsh.exe -NoLogo -NoProfile -NonInteractive -Command \
             $p=Join-Path $env:TEMP '{name}';\
             $null=New-Item -ItemType Directory -Path $p;\
             [IO.File]::WriteAllText((Join-Path $p 'payload'),'owned-fixture');\
             [Console]::WriteLine($p);\
             $null=[Console]::ReadLine()"
        );
        let mut spawned = spawn_agent_process_with_provider(
            &command,
            None,
            Some(agent_id),
            ChildEnvironmentPolicy::CleanCloudDiscovery,
            SharedProviderSelection::Disabled,
        )
        .await
        .unwrap();
        let mut output = spawned.child.take_stdout().await.unwrap();
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(20), output.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        let extraction = PathBuf::from(line.trim());
        assert_eq!(extraction.file_name().unwrap(), name.as_str());
        assert!(extraction.join("payload").is_file());
        assert!(spawned.child.try_wait().unwrap().is_none());
        (spawned, extraction)
    }

    #[tokio::test]
    async fn antigravity_runtime_cleanup_removes_owned_extraction_after_process_exit() {
        let (mut spawned, extraction) = extraction_fixture("antigravity").await;
        spawned.child.terminate().await.unwrap();
        let removed = !extraction.try_exists().unwrap();
        if !removed {
            std::fs::remove_dir_all(&extraction).expect("remove only this test's extraction");
        }
        assert!(
            removed,
            "confirmed Antigravity process exit must also reclaim its owned extraction"
        );
    }

    #[tokio::test]
    async fn antigravity_runtime_cleanup_handles_normal_exit_and_dropped_owner() {
        for cancel in [false, true] {
            let parent_temp = std::env::var_os("TEMP");
            let (mut spawned, extraction) = extraction_fixture("antigravity").await;
            let root = crate::runtime_paths::acp_runtime_root()
                .unwrap()
                .canonicalize()
                .unwrap();
            assert!(extraction.canonicalize().unwrap().starts_with(root));
            assert_eq!(std::env::var_os("TEMP"), parent_temp);
            if !cancel {
                spawned
                    .child
                    .stdin
                    .as_mut()
                    .unwrap()
                    .write_all(b"\n")
                    .await
                    .unwrap();
                assert!(spawned.child.wait().await.unwrap().success());
            }
            drop(spawned);
            tokio::time::timeout(Duration::from_secs(10), async {
                while extraction.try_exists().unwrap() {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            })
            .await
            .expect("cancelled process cleanup must reclaim the extraction");
        }
    }

    #[tokio::test]
    async fn antigravity_runtime_cleanup_does_not_own_custom_commands_or_other_sources() {
        let (mut spawned, extraction) = extraction_fixture("custom:antigravity").await;
        spawned.child.terminate().await.unwrap();
        assert!(extraction.join("payload").is_file());
        std::fs::remove_dir_all(extraction).unwrap();
        for profile in crate::agent_registry::KNOWN_AGENTS {
            let provider = crate::agent_lifecycle::providers::lookup(profile.id).unwrap();
            if profile.id != "antigravity" {
                assert!(provider
                    .runtime_resources(&crate::agent_source::AgentSource::Host)
                    .unwrap()
                    .is_none());
            }
            assert!(provider
                .runtime_resources(&crate::agent_source::AgentSource::Wsl {
                    distro: "fixture".into()
                })
                .unwrap()
                .is_none());
        }
    }
}
