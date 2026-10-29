//! Shared startup admission, independent of provider-specific launch details.

pub(crate) mod providers;
pub(crate) mod verification;

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

pub(crate) const MAX_STARTUP_ATTEMPTS: usize = 3;
pub(crate) const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(60);
pub(crate) const ADMISSION_TIMEOUT: Duration = Duration::from_secs(300);
// Allow the install locks, download and two WSL file transfers before ACP
// initialization. The helper's outer deadline also bounds stalled preparation.
const PREPARATION_WAIT_BUDGET: Duration = Duration::from_secs(1200);
const STARTUP_CLEANUP_BUDGET: Duration = Duration::from_secs(60);
// Explicit retry admission and subsequent startup admission are separate waits.
pub(crate) const HELPER_STARTUP_TIMEOUT: Duration = Duration::from_secs(
    2 * ADMISSION_TIMEOUT.as_secs()
        + PREPARATION_WAIT_BUDGET.as_secs()
        + INITIALIZE_TIMEOUT.as_secs()
        + STARTUP_CLEANUP_BUDGET.as_secs(),
);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StartupPhase {
    Preparation,
    Spawn,
    Initialize,
    Session,
    Cleanup,
}

impl std::fmt::Display for StartupPhase {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for StartupPhase {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StartupReason {
    Failed,
    TimedOut,
    AuthenticationRequired,
    RetryLimitReached,
}

impl std::fmt::Display for StartupReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for StartupReason {}

#[derive(Debug)]
pub(crate) struct DependencyPrepared;

impl std::fmt::Display for DependencyPrepared {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ACP dependency is prepared")
    }
}

impl std::error::Error for DependencyPrepared {}

pub(crate) const MAX_STARTUP_DETAILS_BYTES: usize = 1024;

#[derive(Debug)]
pub(crate) struct StartupDetails(String);

impl StartupDetails {
    fn from_text(text: &str) -> Option<Self> {
        let mut lines = std::collections::VecDeque::new();
        for line in text.lines() {
            let line = line.trim_end_matches('\r');
            if line.trim().is_empty() {
                continue;
            }
            let redacted = line.to_ascii_lowercase().find("[redacted");
            let input = &line[..redacted.unwrap_or(line.len())];
            let safe = crate::protocol::acp::authentication::safe_error_reason(input);
            let stop = redacted.is_some() || safe.as_deref() != Some(input.trim());
            if let Some(mut safe) = safe {
                if redacted.is_some() && !safe.ends_with("[redacted]") {
                    safe.push_str(" [redacted]");
                }
                if lines.len() == 4 {
                    lines.pop_front();
                }
                lines.push_back(safe);
            }
            // A credential value may continue on the following line.
            if stop {
                break;
            }
        }
        let mut text = lines.into_iter().collect::<Vec<_>>().join("\n");
        if text.is_empty() {
            return None;
        }
        if text.len() > MAX_STARTUP_DETAILS_BYTES {
            let mut end = MAX_STARTUP_DETAILS_BYTES - 3;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
            text.push_str("...");
        }
        Some(Self(text))
    }

    pub(crate) fn attach(error: anyhow::Error, lines: &[String]) -> anyhow::Error {
        match Self::from_text(&lines.join("\n")) {
            Some(details) => error.context(details),
            None => error,
        }
    }
}

impl std::fmt::Display for StartupDetails {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for StartupDetails {}

fn deserialize_startup_details<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<serde_json::Value>::deserialize(deserializer)? {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(text)) => {
            Ok(StartupDetails::from_text(&text).map(|details| details.0))
        }
        Some(_) => {
            tracing::warn!(target: "acp_startup", "ignored invalid startup diagnostic details");
            Ok(None)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct StartupFailure {
    #[serde(default)]
    pub retry_epoch: u64,
    pub phase: StartupPhase,
    pub reason: StartupReason,
    pub attempts: usize,
    pub max_attempts: usize,
    pub dependency_present: Option<bool>,
    pub startup_verified: bool,
    pub os_error: Option<i32>,
    pub rpc_code: Option<i32>,
    pub diagnostic_id: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_startup_details"
    )]
    pub details: Option<String>,
}

impl StartupFailure {
    pub(crate) fn from_error(
        error: &anyhow::Error,
        attempts: usize,
        dependency_present: Option<bool>,
        startup_verified: bool,
    ) -> Self {
        if let Some(failure) = error.downcast_ref::<Self>() {
            return failure.clone();
        }
        let rpc_code = error
            .downcast_ref::<agent_client_protocol::Error>()
            .map(|error| error.code.into());
        let reason = error
            .downcast_ref::<StartupReason>()
            .copied()
            .unwrap_or_else(|| {
                if error.is::<StartupLimitReached>() {
                    StartupReason::RetryLimitReached
                } else if error.is::<tokio::time::error::Elapsed>() {
                    StartupReason::TimedOut
                } else if rpc_code == Some(-32000) {
                    StartupReason::AuthenticationRequired
                } else {
                    StartupReason::Failed
                }
            });
        Self {
            retry_epoch: 0,
            phase: error
                .downcast_ref::<StartupPhase>()
                .copied()
                .unwrap_or(StartupPhase::Initialize),
            reason,
            attempts: error
                .downcast_ref::<StartupLimitReached>()
                .map_or(attempts, |limit| limit.attempts),
            max_attempts: MAX_STARTUP_ATTEMPTS,
            dependency_present: dependency_present
                .or_else(|| error.is::<DependencyPrepared>().then_some(true)),
            startup_verified,
            os_error: error
                .downcast_ref::<std::io::Error>()
                .and_then(std::io::Error::raw_os_error),
            rpc_code,
            diagnostic_id: uuid::Uuid::new_v4().to_string(),
            details: error
                .downcast_ref::<StartupDetails>()
                .map(|details| details.0.clone())
                .or_else(|| {
                    error
                        .downcast_ref::<agent_client_protocol::Error>()
                        .and_then(|error| StartupDetails::from_text(&error.message))
                        .map(|details| details.0)
                })
                .or_else(|| {
                    error
                        .downcast_ref::<std::io::Error>()
                        .and_then(|error| StartupDetails::from_text(&error.to_string()))
                        .map(|details| details.0)
                }),
        }
    }
}

impl std::fmt::Display for StartupFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "ACP {:?} {:?} [{}/{}; diagnostic {}]",
            self.phase, self.reason, self.attempts, self.max_attempts, self.diagnostic_id
        )?;
        if let Some(details) = &self.details {
            write!(formatter, "\n{details}")?;
        }
        Ok(())
    }
}

impl std::error::Error for StartupFailure {}

#[derive(Debug)]
pub(crate) struct InstallationStartupCheckFailed;

impl std::fmt::Display for InstallationStartupCheckFailed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ACP preparation succeeded but startup verification failed")
    }
}

impl std::error::Error for InstallationStartupCheckFailed {}

pub(crate) fn provider_binding_key(
    selection: &str,
    config: &crate::custom_model_provider::Config,
) -> String {
    let mut digest = Sha256::new();
    for field in [
        selection,
        config.base_url.as_str(),
        config.model.as_str(),
        config.credential_id.as_deref().unwrap_or(""),
        config.credential_resource,
    ] {
        digest.update((field.len() as u64).to_le_bytes());
        digest.update(field.as_bytes());
    }
    digest.update([
        u8::from(config.api_key_required),
        u8::from(config.credential_id.is_some()),
    ]);
    format!("binding:{:x}", digest.finalize())
}

pub(crate) fn agent_key(
    command: &str,
    agent_id: Option<&str>,
    source: &crate::agent_source::AgentSource,
    provider_binding: &str,
    model_scoped: bool,
) -> String {
    let agent_id = agent_id.or_else(|| {
        let inferred = crate::agent_registry::resolve_agent_id_from_cmd(command);
        crate::agent_registry::is_known_id(inferred).then_some(inferred)
    });
    let lifecycle = if model_scoped || command_is_model_scoped(command, agent_id) {
        "model:"
    } else {
        "warm:"
    };
    format!(
        "{lifecycle}{:?}",
        (source, agent_id, command, provider_binding)
    )
}

pub(crate) fn command_is_model_scoped(command: &str, agent_id: Option<&str>) -> bool {
    let id = agent_id.unwrap_or_else(|| crate::agent_registry::resolve_agent_id_from_cmd(command));
    if !crate::agent_registry::is_known_id(id)
        || crate::agent_registry::supports_live_model_switch(id)
    {
        return false;
    }
    let profile = crate::agent_registry::lookup_profile_by_id(id);
    let tokens = crate::coordinator::split_windows_commandline(command);
    let args: Vec<&str> = tokens.iter().skip(1).map(String::as_str).collect();
    crate::agent_registry::extract_model_from_args(&args, profile).is_some()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StartupLimitReached {
    pub attempts: usize,
}

impl std::fmt::Display for StartupLimitReached {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "ACP startup attempt limit reached ({}/{MAX_STARTUP_ATTEMPTS})",
            self.attempts
        )
    }
}

impl std::error::Error for StartupLimitReached {}

#[derive(Debug)]
pub(crate) struct StartupCleanupUnconfirmed;

impl std::fmt::Display for StartupCleanupUnconfirmed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .write_str("ACP startup is blocked because previous process cleanup was not confirmed")
    }
}

impl std::error::Error for StartupCleanupUnconfirmed {}

#[derive(Default, Serialize, Deserialize)]
struct StartupState {
    attempts: usize,
    #[serde(default)]
    retry_epoch: u64,
    #[serde(default)]
    last_failure: Option<StartupFailure>,
    #[serde(default)]
    cleanup_failed: bool,
}

#[derive(Default)]
pub(crate) struct StartupRegistry {
    states: Mutex<HashMap<String, Arc<AsyncMutex<StartupState>>>>,
    directory: Option<PathBuf>,
}

impl StartupRegistry {
    pub(crate) fn runtime() -> Result<Self> {
        let root = crate::runtime_paths::intelligent_terminal_local_root()
            .context("ACP startup state directory is unavailable")?;
        let executable = std::env::current_exe().context("resolve WTA startup identity")?;
        let metadata = executable
            .metadata()
            .context("inspect WTA startup identity")?;
        let identity = format!(
            "{}:{}:{}",
            executable.display(),
            metadata.file_size(),
            metadata.last_write_time()
        );
        let build = format!("{:x}", Sha256::digest(identity.as_bytes()));
        Ok(Self::persistent(root.join("acp-startup").join(build)))
    }

    fn persistent(directory: PathBuf) -> Self {
        Self {
            states: Mutex::new(HashMap::new()),
            directory: Some(directory),
        }
    }

    pub(crate) async fn lock(&self, key: &str) -> Result<StartupLease> {
        let state = {
            let mut states = self
                .states
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            Arc::clone(states.entry(key.to_string()).or_default())
        };
        let deadline = tokio::time::Instant::now() + ADMISSION_TIMEOUT;
        let mut state = tokio::time::timeout_at(deadline, state.lock_owned())
            .await
            .context("another ACP startup operation is still in progress")?;
        let file = if let Some(directory) = &self.directory {
            let path = directory.join(format!("{:x}.json", Sha256::digest(key.as_bytes())));
            loop {
                let path = path.clone();
                let opened = tokio::task::spawn_blocking(move || open_startup_state(path))
                    .await
                    .context("ACP startup state task failed")??;
                if let Some((file, saved)) = opened {
                    *state = saved;
                    break Some(file);
                }
                ensure!(
                    tokio::time::Instant::now() < deadline,
                    "another ACP startup operation is still in progress"
                );
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        } else {
            None
        };
        Ok(StartupLease {
            state,
            file,
            attempt_number: 0,
            dependency_present: false,
            startup_verified: false,
            executable: None,
        })
    }

    pub(crate) async fn retry(&self, key: &str) -> Result<()> {
        let mut lease = self.lock(key).await?;
        lease.ensure_cleanup()?;
        lease.state.retry_epoch = lease
            .state
            .retry_epoch
            .checked_add(1)
            .context("ACP startup retry epoch exhausted")?;
        lease.state.attempts = 0;
        lease.state.last_failure = None;
        lease.save()
    }

    pub(crate) async fn retry_epoch(&self, key: &str) -> Result<u64> {
        Ok(self.lock(key).await?.retry_epoch())
    }
}

fn open_startup_state(path: PathBuf) -> Result<Option<(File, StartupState)>> {
    let parent = path.parent().context("ACP startup state has no parent")?;
    std::fs::create_dir_all(parent).context("create ACP startup state directory")?;
    let options = |create_new| {
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(true)
            .share_mode(0)
            .create_new(create_new)
            .custom_flags(0x0020_0000);
        options
    };
    let (mut file, fresh) = match options(true).open(&path) {
        Ok(file) => (file, true),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            match options(false).open(&path) {
                Ok(file) => (file, false),
                Err(error) if matches!(error.raw_os_error(), Some(32 | 33)) => return Ok(None),
                Err(error) => return Err(error).context("open ACP startup state"),
            }
        }
        Err(error) if matches!(error.raw_os_error(), Some(32 | 33)) => return Ok(None),
        Err(error) => return Err(error).context("create ACP startup state"),
    };
    let metadata = file.metadata().context("inspect ACP startup state")?;
    ensure!(
        metadata.is_file() && metadata.file_attributes() & 0x400 == 0 && metadata.len() <= 4096,
        "ACP startup state is not a bounded regular file"
    );
    let state = if fresh {
        let state = StartupState::default();
        save_startup_state(&mut file, &state)?;
        state
    } else {
        let mut text = String::new();
        file.read_to_string(&mut text)
            .context("read ACP startup state")?;
        serde_json::from_str(&text).context("parse ACP startup state")?
    };
    Ok(Some((file, state)))
}

fn save_startup_state(file: &mut File, state: &StartupState) -> Result<()> {
    let data = serde_json::to_vec(state).context("serialize ACP startup state")?;
    file.seek(SeekFrom::Start(0))
        .context("seek ACP startup state")?;
    file.write_all(&data).context("write ACP startup state")?;
    file.set_len(data.len() as u64)
        .context("truncate ACP startup state")?;
    file.sync_data().context("persist ACP startup state")
}

pub(crate) struct StartupLease {
    state: OwnedMutexGuard<StartupState>,
    file: Option<File>,
    attempt_number: usize,
    dependency_present: bool,
    startup_verified: bool,
    executable: Option<String>,
}

impl StartupLease {
    pub(crate) fn retry_epoch(&self) -> u64 {
        self.state.retry_epoch
    }

    fn ensure_cleanup(&self) -> Result<()> {
        if self.state.cleanup_failed {
            return Err(match &self.state.last_failure {
                Some(failure) => {
                    anyhow::Error::new(failure.clone()).context(StartupCleanupUnconfirmed)
                }
                None => {
                    anyhow::Error::new(StartupPhase::Cleanup).context(StartupCleanupUnconfirmed)
                }
            });
        }
        Ok(())
    }

    fn save(&mut self) -> Result<()> {
        match &mut self.file {
            Some(file) => save_startup_state(file, &self.state),
            None => Ok(()),
        }
    }

    pub(crate) fn begin(&mut self) -> Result<()> {
        self.ensure_cleanup()?;
        if self.state.attempts >= MAX_STARTUP_ATTEMPTS {
            let limit = StartupLimitReached {
                attempts: self.state.attempts,
            };
            return Err(match &self.state.last_failure {
                Some(failure) => anyhow::Error::new(failure.clone()).context(limit),
                None => limit.into(),
            });
        }
        self.state.attempts += 1;
        self.attempt_number = self.state.attempts;
        self.save()?;
        Ok(())
    }

    pub(crate) fn succeeded(&mut self) -> Result<()> {
        self.startup_verified = true;
        self.state.attempts = 0;
        self.state.last_failure = None;
        self.save()
    }

    pub(crate) fn prepared(&mut self, artifact: providers::PreparedArtifact) {
        self.dependency_present = artifact.outcome == providers::PreparationOutcome::Ready;
        self.executable = artifact.executable;
    }

    pub(crate) fn dependency_present(&self) -> bool {
        self.dependency_present
    }

    pub(crate) fn executable(&self) -> Option<&str> {
        self.executable.as_deref()
    }

    pub(crate) fn verified(&self) -> bool {
        self.startup_verified
    }

    pub(crate) fn failed(mut self, error: anyhow::Error) -> anyhow::Error {
        let mut failure = StartupFailure::from_error(
            &error,
            self.attempt_number,
            self.dependency_present.then_some(true),
            self.startup_verified,
        );
        failure.retry_epoch = self.retry_epoch();
        let mut error = error.context(failure.clone());
        if failure.phase == StartupPhase::Cleanup {
            self.state.cleanup_failed = true;
        }
        if !self.startup_verified || self.state.cleanup_failed {
            self.state.last_failure = Some(failure);
            if let Err(persist_error) = self.save() {
                error = error.context(persist_error);
            }
        }
        if !self.startup_verified && self.state.attempts >= MAX_STARTUP_ATTEMPTS {
            error.context(StartupLimitReached {
                attempts: self.state.attempts,
            })
        } else {
            error
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn omitted_builtin_identity_shares_diagnostic_admission_and_retry() {
        let registry = StartupRegistry::default();
        let source = crate::agent_source::AgentSource::Host;
        let command = "copilot --acp --stdio";
        let legacy = agent_key(command, None, &source, "same-provider", false);
        let diagnostic = agent_key(command, Some("copilot"), &source, "same-provider", false);
        for _ in 0..MAX_STARTUP_ATTEMPTS {
            let mut lease = registry.lock(&legacy).await.unwrap();
            lease.begin().unwrap();
            let _ = lease.failed(anyhow::anyhow!("controlled initialization failure"));
        }
        let stopped = registry
            .lock(&diagnostic)
            .await
            .unwrap()
            .begin()
            .unwrap_err();
        assert!(stopped.is::<StartupLimitReached>());
        registry.retry(&diagnostic).await.unwrap();
        registry.lock(&legacy).await.unwrap().begin().unwrap();
    }

    #[tokio::test]
    async fn omitted_builtin_identity_preserves_cleanup_quarantine() {
        let registry = StartupRegistry::default();
        let source = crate::agent_source::AgentSource::Host;
        let command = "copilot --acp --stdio";
        let legacy = agent_key(command, None, &source, "same-provider", false);
        let diagnostic = agent_key(command, Some("copilot"), &source, "same-provider", false);
        let lease = registry.lock(&legacy).await.unwrap();
        let _ = lease.failed(anyhow::Error::new(StartupPhase::Cleanup));
        let stopped = registry
            .lock(&diagnostic)
            .await
            .unwrap()
            .begin()
            .unwrap_err();
        assert!(stopped.is::<StartupCleanupUnconfirmed>());
        assert!(registry.retry(&diagnostic).await.is_err());
    }

    #[test]
    fn omitted_builtin_identity_does_not_override_authoritative_custom_ids() {
        let source = crate::agent_source::AgentSource::Host;
        let command = "copilot --acp --stdio";
        assert_eq!(
            agent_key(command, None, &source, "native", false),
            agent_key(command, Some("copilot"), &source, "native", false),
        );
        assert_ne!(
            agent_key(command, None, &source, "native", false),
            agent_key(command, Some("custom:my-agent"), &source, "native", false),
        );
    }

    #[test]
    fn startup_details_preserve_native_text_and_structured_codes() {
        let lines = vec![
            "Failed to load Python DLL 'python310.dll'.".to_string(),
            "LoadLibrary: An Application Control policy has blocked this file.".to_string(),
        ];
        let error = StartupDetails::attach(
            agent_client_protocol::Error::new(-32603, "Internal error")
                .data(serde_json::json!({"password": "not-public"}))
                .into(),
            &lines,
        );
        let failure = StartupFailure::from_error(&error, 2, Some(true), false);
        assert_eq!(failure.reason, StartupReason::Failed);
        assert_eq!(failure.rpc_code, Some(-32603));
        assert_eq!(failure.details.as_deref(), Some(lines.join("\n").as_str()));
        let serialized = serde_json::to_string(&failure).unwrap();
        assert!(!serialized.contains("not-public"));
        assert_eq!(
            serde_json::from_str::<StartupFailure>(&serialized).unwrap(),
            failure
        );
    }

    #[test]
    fn startup_details_normalize_windows_line_endings_without_false_redaction() {
        let details = StartupDetails::from_text(
            "Failed to load Python DLL.\r\r\nLoadLibrary: blocked by the operating system.\r",
        )
        .unwrap();
        assert_eq!(
            details.0,
            "Failed to load Python DLL.\nLoadLibrary: blocked by the operating system."
        );
        let interior =
            StartupDetails::from_text(concat!("LoadLibrary failed", "\r", "private-marker"))
                .unwrap();
        assert!(!interior.0.contains("private-marker"));
    }

    #[test]
    fn startup_details_remove_secret_tails_and_unsafe_controls() {
        for text in [
            "LoadLibrary failed; password=private-marker\nprivate-continuation",
            "Provider rejected request; api_key=private-marker",
            "Provider rejected request; --api-key private-marker",
            "Provider rejected request; private_key: private-marker",
            "LoadLibrary failed\n[redacted agent authentication progress]\nprivate-marker",
            "LoadLibrary failed; https://example.test/?token=private-marker",
            "LoadLibrary failed\u{1b}[31mprivate-marker",
            "LoadLibrary failed\u{202e}private-marker",
        ] {
            let details = StartupDetails::from_text(text).unwrap().0;
            assert!(
                details.contains("LoadLibrary failed")
                    || details.contains("Provider rejected request")
            );
            assert!(!details.contains("private-marker"));
            assert!(!details.contains("private-continuation"));
            assert!(!details.contains("example.test"));
            assert!(!details.chars().any(|ch| ch.is_control() && ch != '\n'));
            assert!(!details.contains('\u{202e}'));
        }
        assert!(
            StartupDetails::from_text("password=private-marker").is_none(),
            "a credential-labelled diagnostic must be omitted"
        );
    }

    #[test]
    fn startup_details_preserve_literal_asterisks_as_harmless_text() {
        assert_eq!(StartupDetails::from_text("******").unwrap().0, "******");
    }

    #[test]
    fn startup_details_are_utf8_bounded_and_fit_the_persistent_ledger() {
        for text in [
            vec!["\\".repeat(512); 4].join("\n"),
            "\u{4e2d}".repeat(2048),
            (0..10)
                .map(|index| format!("native line {index}"))
                .collect::<Vec<_>>()
                .join("\n"),
        ] {
            let details = StartupDetails::from_text(&text).unwrap();
            assert!(details.0.len() <= MAX_STARTUP_DETAILS_BYTES);
            assert!(details.0.lines().count() <= 4);
            let failure =
                StartupFailure::from_error(&anyhow::Error::new(details), 1, Some(true), false);
            let state = StartupState {
                last_failure: Some(failure),
                ..Default::default()
            };
            assert!(serde_json::to_vec(&state).unwrap().len() <= 4096);
        }
    }

    #[test]
    fn startup_details_accept_legacy_payloads_and_discard_invalid_optional_data() {
        let failure =
            StartupFailure::from_error(&anyhow::anyhow!("startup failed"), 1, None, false);
        let mut payload = serde_json::to_value(&failure).unwrap();
        assert!(payload.get("details").is_none());
        assert_eq!(
            serde_json::from_value::<StartupFailure>(payload.clone())
                .unwrap()
                .details,
            None
        );
        payload["details"] = serde_json::json!({"password": "private-marker"});
        let decoded = serde_json::from_value::<StartupFailure>(payload).unwrap();
        assert_eq!(decoded.reason, StartupReason::Failed);
        assert_eq!(decoded.details, None);
        assert!(!decoded.to_string().contains("private-marker"));
    }

    #[test]
    fn startup_reasons_do_not_include_message_inferred_categories() {
        for (reason, wire) in [
            (StartupReason::Failed, "failed"),
            (StartupReason::TimedOut, "timed_out"),
            (
                StartupReason::AuthenticationRequired,
                "authentication_required",
            ),
            (StartupReason::RetryLimitReached, "retry_limit_reached"),
        ] {
            assert_eq!(serde_json::to_value(reason).unwrap(), wire);
            assert_eq!(
                serde_json::from_value::<StartupReason>(serde_json::json!(wire)).unwrap(),
                reason
            );
        }
        assert!(
            serde_json::from_str::<StartupReason>(r#""operating_system_policy""#).is_err(),
            "native stderr wording must not define a startup reason"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn helper_wait_allows_queued_initialization_but_remains_bounded() {
        let result = tokio::time::timeout(HELPER_STARTUP_TIMEOUT, async {
            tokio::time::sleep(ADMISSION_TIMEOUT).await;
            tokio::time::sleep(INITIALIZE_TIMEOUT).await;
        })
        .await;
        assert!(result.is_ok());
        assert!(
            tokio::time::timeout(HELPER_STARTUP_TIMEOUT, std::future::pending::<()>(),)
                .await
                .is_err()
        );
        assert!(tokio::time::timeout(
            INITIALIZE_TIMEOUT,
            tokio::time::sleep(INITIALIZE_TIMEOUT + Duration::from_secs(1)),
        )
        .await
        .is_err());
    }

    #[tokio::test]
    async fn retry_opens_one_new_bounded_cycle_without_changing_another_target() {
        let registry = StartupRegistry::default();
        for _ in 0..MAX_STARTUP_ATTEMPTS {
            registry.lock("host:one").await.unwrap().begin().unwrap();
        }
        assert!(registry.lock("host:one").await.unwrap().begin().is_err());
        registry.lock("wsl:one").await.unwrap().begin().unwrap();
        registry.retry("host:one").await.unwrap();
        assert_eq!(registry.retry_epoch("host:one").await.unwrap(), 1);
        assert_eq!(registry.retry_epoch("wsl:one").await.unwrap(), 0);
        for _ in 0..MAX_STARTUP_ATTEMPTS {
            registry.lock("host:one").await.unwrap().begin().unwrap();
        }
        assert!(registry.lock("host:one").await.unwrap().begin().is_err());
        assert_eq!(registry.lock("wsl:one").await.unwrap().state.attempts, 1);
    }

    #[tokio::test]
    async fn completed_startup_resets_only_its_own_failure_budget() {
        let registry = StartupRegistry::default();
        registry.lock("failed").await.unwrap().begin().unwrap();
        let mut ready = registry.lock("ready").await.unwrap();
        ready.begin().unwrap();
        ready.succeeded().unwrap();
        assert_eq!(ready.retry_epoch(), 0);
        drop(ready);
        assert_eq!(registry.lock("ready").await.unwrap().state.attempts, 0);
        assert_eq!(registry.lock("failed").await.unwrap().state.attempts, 1);
    }

    #[tokio::test]
    async fn replacement_waits_for_the_previous_attempt_lease() {
        use futures::FutureExt;
        let registry = StartupRegistry::default();
        let first = registry.lock("same").await.unwrap();
        assert!(registry.lock("same").now_or_never().is_none());
        assert!(registry.retry("same").now_or_never().is_none());
        drop(first);
        registry.lock("same").await.unwrap().begin().unwrap();
    }

    #[tokio::test]
    async fn independent_callers_share_persisted_failure_limits() {
        let directory = std::env::temp_dir().join(format!("wta-startup-{}", uuid::Uuid::new_v4()));
        let first = StartupRegistry::persistent(directory.clone());
        let second = StartupRegistry::persistent(directory.clone());
        for registry in [&first, &second, &first] {
            registry.lock("same-target").await.unwrap().begin().unwrap();
        }
        assert!(second.lock("same-target").await.unwrap().begin().is_err());
        assert_eq!(first.retry_epoch("same-target").await.unwrap(), 0);
        second.retry("same-target").await.unwrap();
        assert_eq!(first.retry_epoch("same-target").await.unwrap(), 1);
        first.lock("same-target").await.unwrap().begin().unwrap();
        drop((first, second));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn failure_carries_the_explicit_retry_epoch() {
        let registry = StartupRegistry::default();
        registry.retry("one").await.unwrap();
        let mut startup = registry.lock("one").await.unwrap();
        startup.begin().unwrap();
        let error = startup.failed(anyhow::anyhow!("controlled failure"));
        assert_eq!(
            error.downcast_ref::<StartupFailure>().unwrap().retry_epoch,
            1
        );
        assert_eq!(registry.retry_epoch("one").await.unwrap(), 1);
        let old: StartupState = serde_json::from_str(r#"{"attempts":3}"#).unwrap();
        assert_eq!(old.retry_epoch, 0);
    }

    #[tokio::test]
    async fn unconfirmed_cleanup_blocks_automatic_and_explicit_replacement_only_for_its_target() {
        let registry = StartupRegistry::default();
        let mut startup = registry.lock("blocked").await.unwrap();
        startup.begin().unwrap();
        let error = startup
            .failed(anyhow::anyhow!("controlled cleanup failure").context(StartupPhase::Cleanup));
        assert_eq!(
            error.downcast_ref::<StartupFailure>().unwrap().phase,
            StartupPhase::Cleanup
        );
        assert!(registry.lock("blocked").await.unwrap().begin().is_err());
        assert!(registry.retry("blocked").await.is_err());
        assert_eq!(registry.retry_epoch("blocked").await.unwrap(), 0);
        registry.lock("healthy").await.unwrap().begin().unwrap();
    }
}
