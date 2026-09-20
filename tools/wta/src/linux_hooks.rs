//! Installation state, independent of the transport used to deliver live hooks.

use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

pub(crate) const METHOD: &str = "_intellterm.wta/linux_hooks";
pub(crate) const PROVIDERS: &[&str] = &["copilot", "claude", "codex", "gemini", "opencode"];

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Target {
    Ssh {
        target: crate::ssh_sessions::SshTarget,
    },
    Wsl {
        distro: String,
        user: String,
    },
}

impl Target {
    pub fn validate(&self) -> Result<()> {
        if let Self::Wsl { distro, user } = self {
            ensure!(
                !distro.is_empty()
                    && distro.len() <= 256
                    && !distro.starts_with('-')
                    && distro
                        .chars()
                        .all(|c| c.is_alphanumeric() || " ._-".contains(c)),
                "Invalid WSL distribution"
            );
            ensure!(
                !user.is_empty()
                    && user.len() <= 128
                    && !user.starts_with('-')
                    && user
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_.-$".contains(&b)),
                "Invalid WSL user"
            );
        }
        Ok(())
    }

    pub fn label(&self) -> String {
        match self {
            Self::Ssh { target } => match target.port() {
                Some(port) => format!("{}:{port}", target.destination()),
                None => target.destination().to_owned(),
            },
            Self::Wsl { distro, user } => format!("WSL {distro} ({user})"),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Binding {
    pub pane_id: String,
    pub tab_id: String,
    pub window_id: String,
    pub target: Target,
    #[serde(default)]
    pub native_tmux: bool,
}

impl Binding {
    pub fn validate(&mut self) -> Result<()> {
        let pane = uuid::Uuid::parse_str(&self.pane_id)?;
        ensure!(!pane.is_nil(), "Missing native pane identity");
        self.pane_id = pane.hyphenated().to_string();
        ensure!(
            !self.tab_id.is_empty() && self.tab_id.len() <= 128,
            "Invalid native tab identity"
        );
        ensure!(
            !self.window_id.is_empty()
                && self.window_id.len() <= 20
                && self.window_id.bytes().all(|byte| byte.is_ascii_digit()),
            "Invalid native window identity"
        );
        self.target.validate()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum InstallState {
    Checking,
    Installing,
    Installed,
    NotFound,
    Disabled,
    Unavailable { reason: String },
}

impl InstallState {
    pub fn failed(&self) -> bool {
        matches!(
            self,
            Self::Unavailable { .. } | Self::Checking | Self::Installing
        )
    }

    pub fn from_result(code: &str) -> Self {
        match code {
            "installing" => Self::Installing,
            "installed" => Self::Installed,
            "not-found" => Self::NotFound,
            "user-disabled" | "user-removed" => Self::Disabled,
            _ => Self::Unavailable {
                reason: code.to_owned(),
            },
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ProviderStatus {
    pub cli: String,
    pub status: InstallState,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct TargetStatus {
    pub target: Target,
    pub providers: Vec<ProviderStatus>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Request {
    Snapshot {
        tab_id: String,
        cli: String,
        #[serde(default)]
        pane_id: Option<String>,
    },
    Install {
        cli: Option<String>,
    },
    Prepare {
        target: Target,
        cli: String,
    },
    PrepareWsl {
        distro: String,
        cli: String,
    },
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct Response {
    pub enabled: bool,
    pub targets: Vec<TargetStatus>,
}

impl Response {
    pub fn succeeded(&self) -> bool {
        self.targets
            .iter()
            .all(|target| target.providers.iter().all(|p| !p.status.failed()))
    }
}

pub(crate) fn build_request(
    request: &Request,
) -> Result<agent_client_protocol::schema::v1::ExtRequest> {
    Ok(agent_client_protocol::schema::v1::ExtRequest::new(
        METHOD,
        serde_json::value::to_raw_value(request)?.into(),
    ))
}

pub(crate) fn parse_request(
    raw: &serde_json::value::RawValue,
) -> Result<Request, serde_json::Error> {
    serde_json::from_str(raw.get())
}

pub(crate) async fn prepare_launch(pipe: Option<String>, request: Request) {
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let request = build_request(&request)?;
        let result = crate::cli::sessions::request_from_master(pipe, request).await?;
        serde_json::from_str::<Response>(result.0.get()).map_err(anyhow::Error::from)
    })
    .await;
    if !matches!(result, Ok(Ok(ref report)) if report.succeeded()) {
        tracing::warn!(target: "linux_hooks", "Hook preparation is incomplete; agent launch continues");
    }
}

pub(crate) fn wsl_executable() -> Result<std::path::PathBuf> {
    let root = std::env::var_os("SystemRoot")
        .ok_or_else(|| anyhow::anyhow!("Windows system directory unavailable"))?;
    Ok(std::path::PathBuf::from(root)
        .join("System32")
        .join("wsl.exe"))
}

pub(crate) fn status_text(status: &InstallState, cli: &str) -> String {
    let command = format!("wta hooks install --cli {cli}");
    match status {
        InstallState::Checking => t!("agents.loading").into_owned(),
        InstallState::Installing => t!("hooks.linux_installing", cli = cli).into_owned(),
        InstallState::Installed => t!("hooks.installed").into_owned(),
        InstallState::NotFound => t!("hooks.cli_not_on_path").into_owned(),
        InstallState::Disabled => t!("hooks.installed_but_disabled").into_owned(),
        InstallState::Unavailable { reason }
            if matches!(
                reason.as_str(),
                "required-utility-unavailable:tmux" | "unsupported-tmux-version"
            ) =>
        {
            t!("hooks.linux_tmux_required", command = command).into_owned()
        }
        InstallState::Unavailable { reason } if reason.starts_with("unsupported-") => {
            t!("hooks.linux_unsupported", cli = cli, reason = reason).into_owned()
        }
        InstallState::Unavailable { reason } => {
            t!("hooks.linux_failed", reason = reason, command = command).into_owned()
        }
    }
}

/// Only nonce-correlated, bounded machine records are accepted; remote stderr
/// and shell startup output are never displayed as installation diagnostics.
pub(crate) fn parse_report<'a>(
    line: &'a str,
    nonce: &str,
    allowed: &[String],
) -> Result<Option<(&'a str, InstallState)>> {
    let Some(record) = line.strip_prefix("IT_HOOK_INSTALL/1 ") else {
        return Ok(None);
    };
    let fields: Vec<_> = record.split(' ').collect();
    if fields.first().copied() != Some(nonce) {
        return Ok(None);
    }
    ensure!(
        fields.len() == 3
            && (fields[1] == "all" || allowed.iter().any(|cli| cli == fields[1]))
            && !fields[2].is_empty()
            && fields[2].len() <= 80
            && fields[2]
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-:".contains(&b)),
        "Invalid Linux hook installation record"
    );
    Ok(Some((fields[1], InstallState::from_result(fields[2]))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wsl_installation_identity_preserves_the_linux_user() {
        let alice = Target::Wsl {
            distro: "Ubuntu".into(),
            user: "alice".into(),
        };
        let root = Target::Wsl {
            distro: "Ubuntu".into(),
            user: "root".into(),
        };
        assert_ne!(alice, root);
        assert!(alice.validate().is_ok());
        assert!(Target::Wsl {
            distro: "--shutdown".into(),
            user: "root".into()
        }
        .validate()
        .is_err());
        assert!(Target::Wsl {
            distro: "Ubuntu".into(),
            user: "root;id".into()
        }
        .validate()
        .is_err());
    }

    #[test]
    fn installation_records_require_nonce_provider_and_bounded_codes() {
        let allowed = vec!["copilot".to_owned()];
        assert!(parse_report("untrusted startup output", "nonce", &allowed)
            .unwrap()
            .is_none());
        assert!(parse_report(
            "IT_HOOK_INSTALL/1 other copilot installed",
            "nonce",
            &allowed
        )
        .unwrap()
        .is_none());
        assert!(parse_report(
            "IT_HOOK_INSTALL/1 nonce claude installed",
            "nonce",
            &allowed
        )
        .is_err());
        assert_eq!(
            parse_report(
                "IT_HOOK_INSTALL/1 nonce copilot installing",
                "nonce",
                &allowed
            )
            .unwrap()
            .unwrap()
            .1,
            InstallState::Installing
        );
        assert!(parse_report(
            "IT_HOOK_INSTALL/1 nonce all required-utility-unavailable:tmux",
            "nonce",
            &allowed
        )
        .unwrap()
        .unwrap()
        .1
        .failed());
        assert!(parse_report(
            "IT_HOOK_INSTALL/1 nonce copilot \u{1b}[31m",
            "nonce",
            &allowed
        )
        .is_err());
    }

    #[test]
    fn pending_work_is_not_a_successful_installation() {
        assert!(InstallState::Checking.failed());
        assert!(InstallState::Installing.failed());
        assert!(!InstallState::Installed.failed());
        assert_eq!(
            InstallState::from_result("user-removed"),
            InstallState::Disabled
        );
    }
}
