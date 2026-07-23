//! Execution source for an ACP agent pane.
//!
//! The canonical agent id answers "which agent" (`copilot`, `claude`, ...).
//! [`AgentSource`] answers "where that agent runs". Keeping the two dimensions
//! separate lets `/agent` offer both Windows and the current WSL distro without
//! changing policy identifiers, telemetry bucketing, or session CLI labels.

use std::fmt;
use std::time::Duration;

/// Environment that hosts an ACP agent process.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum AgentSource {
    #[default]
    Host,
    Wsl {
        distro: String,
    },
}

impl AgentSource {
    pub const HOST_KIND: &'static str = "host";
    pub const WSL_KIND: &'static str = "wsl";

    /// Parse the source fields carried on the helper command line or `_meta.wta`.
    ///
    /// Invalid/incomplete values fail closed to `Host`; callers log malformed
    /// WSL requests before using this compatibility fallback.
    pub fn from_wire(kind: Option<&str>, distro: Option<&str>) -> Self {
        match kind.map(str::trim) {
            Some(kind) if kind.eq_ignore_ascii_case(Self::WSL_KIND) => distro
                .map(str::trim)
                .filter(|distro| !distro.is_empty())
                .map(|distro| Self::Wsl {
                    distro: distro.to_string(),
                })
                .unwrap_or(Self::Host),
            _ => Self::Host,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Host => Self::HOST_KIND,
            Self::Wsl { .. } => Self::WSL_KIND,
        }
    }

    pub fn distro(&self) -> Option<&str> {
        match self {
            Self::Host => None,
            Self::Wsl { distro } => Some(distro),
        }
    }

    pub fn display_suffix(&self) -> String {
        match self {
            Self::Host => "Windows".to_string(),
            Self::Wsl { distro } => distro.clone(),
        }
    }

    pub fn session_location(&self) -> crate::agent_sessions::SessionLocation {
        match self {
            Self::Host => crate::agent_sessions::SessionLocation::Host,
            Self::Wsl { distro } => crate::agent_sessions::SessionLocation::Wsl {
                distro: distro.clone(),
            },
        }
    }
}

impl fmt::Display for AgentSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Host => f.write_str(Self::HOST_KIND),
            Self::Wsl { distro } => write!(f, "{}:{distro}", Self::WSL_KIND),
        }
    }
}

/// Extract `wsl:<distro>` from the Terminal Protocol active-pane payload.
pub fn active_pane_wsl_distro(active: Option<&serde_json::Value>) -> Option<&str> {
    active
        .and_then(|pane| pane.get("shell"))
        .and_then(serde_json::Value::as_str)
        .and_then(|shell| shell.strip_prefix("wsl:"))
        .map(str::trim)
        .filter(|distro| !distro.is_empty())
}

/// Resolve the cwd sent to an ACP agent running in `source`.
///
/// Host agents preserve the pre-WSL-backend behavior and use WTA's own cwd.
/// WSL agents require an absolute POSIX path, so translate the common WT
/// forms and resolve `~`/relative paths against the distro's real `$HOME`.
pub async fn resolve_source_cwd(source: &AgentSource, reported: Option<&str>) -> Option<String> {
    let AgentSource::Wsl { distro } = source else {
        return None;
    };

    let reported = reported.map(str::trim).filter(|cwd| !cwd.is_empty());
    if let Some(cwd) = reported.and_then(|cwd| normalize_wsl_cwd(distro, cwd)) {
        return Some(cwd);
    }

    let home = resolve_wsl_home(distro)
        .await
        .unwrap_or_else(|| "/".to_string());
    match reported {
        Some("~") | None => Some(home),
        Some(relative) if relative.starts_with("~/") => {
            Some(format!("{}/{}", home.trim_end_matches('/'), &relative[2..]))
        }
        Some(relative) if !relative.contains(':') && !relative.starts_with('\\') => {
            Some(format!("{}/{}", home.trim_end_matches('/'), relative))
        }
        _ => Some(home),
    }
}

fn normalize_wsl_cwd(distro: &str, cwd: &str) -> Option<String> {
    if cwd == "~" || cwd.starts_with("~/") {
        return None;
    }

    let normalized = cwd.replace('\\', "/");
    for root in [
        format!("//wsl.localhost/{distro}"),
        format!("//wsl$/{distro}"),
        format!("//?/UNC/wsl.localhost/{distro}"),
        format!("//?/UNC/wsl$/{distro}"),
    ] {
        if normalized.eq_ignore_ascii_case(&root) {
            return Some("/".to_string());
        }
        let prefix = format!("{root}/");
        if normalized.len() > prefix.len()
            && normalized[..prefix.len()].eq_ignore_ascii_case(&prefix)
        {
            return Some(format!("/{}", &normalized[prefix.len()..]));
        }
    }
    if cwd.starts_with('/') {
        return Some(cwd.to_string());
    }

    let bytes = normalized.as_bytes();
    if bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'/' {
        let drive = (bytes[0] as char).to_ascii_lowercase();
        return Some(format!("/mnt/{drive}/{}", &normalized[3..]));
    }
    None
}

async fn resolve_wsl_home(distro: &str) -> Option<String> {
    let mut command = tokio::process::Command::new("wsl.exe");
    command
        .arg("-d")
        .arg(distro)
        .arg("--")
        .arg("sh")
        .arg("-lc")
        .arg("printf '%s' \"$HOME\"")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(10), command.output())
        .await
        .ok()?
        .ok()?;
    let home = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (output.status.success() && home.starts_with('/')).then_some(home)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_source_requires_a_nonempty_wsl_distro() {
        assert_eq!(
            AgentSource::from_wire(Some("wsl"), Some("Ubuntu")),
            AgentSource::Wsl {
                distro: "Ubuntu".to_string()
            }
        );
        assert_eq!(
            AgentSource::from_wire(Some("wsl"), Some(" ")),
            AgentSource::Host
        );
        assert_eq!(
            AgentSource::from_wire(Some("unknown"), Some("Ubuntu")),
            AgentSource::Host
        );
    }

    #[test]
    fn active_pane_distro_comes_from_shell_metadata() {
        let pane = serde_json::json!({ "profile": "Renamed profile", "shell": "wsl:Ubuntu" });
        assert_eq!(active_pane_wsl_distro(Some(&pane)), Some("Ubuntu"));
        assert_eq!(
            active_pane_wsl_distro(Some(&serde_json::json!({ "shell": "pwsh.exe" }))),
            None
        );
    }

    #[test]
    fn wsl_cwd_normalizes_terminal_path_forms() {
        assert_eq!(
            normalize_wsl_cwd("Ubuntu", "/home/me/project").as_deref(),
            Some("/home/me/project")
        );
        assert_eq!(
            normalize_wsl_cwd("Ubuntu", r"\\wsl.localhost\Ubuntu\home\me").as_deref(),
            Some("/home/me")
        );
        assert_eq!(
            normalize_wsl_cwd("Ubuntu", r"\\wsl$\Ubuntu\home\me").as_deref(),
            Some("/home/me")
        );
        assert_eq!(
            normalize_wsl_cwd("Ubuntu", "//wsl.localhost/Ubuntu/home/me").as_deref(),
            Some("/home/me")
        );
        assert_eq!(
            normalize_wsl_cwd("Ubuntu", r"C:\src\project").as_deref(),
            Some("/mnt/c/src/project")
        );
        assert_eq!(normalize_wsl_cwd("Ubuntu", "~"), None);
        assert_eq!(normalize_wsl_cwd("Ubuntu", r"\\wsl$\Debian\home\me"), None);
    }
}
