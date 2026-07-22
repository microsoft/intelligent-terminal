//! Execution source for an ACP agent pane.
//!
//! The canonical agent id answers "which agent" (`copilot`, `claude`, ...).
//! [`AgentSource`] answers "where that agent runs". Keeping the two dimensions
//! separate lets `/agent` offer both Windows and the current WSL distro without
//! changing policy identifiers, telemetry bucketing, or session CLI labels.

use std::fmt;

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
            Self::Wsl { distro } => format!("{distro} (WSL)"),
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
}
