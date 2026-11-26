//! Execution source for an ACP agent pane.
//!
//! The canonical agent id answers "which agent" (`copilot`, `claude`, ...).
//! [`AgentSource`] answers "where that agent runs". Keeping the two dimensions
//! separate lets `/agent` offer both Windows and the current WSL distro without
//! changing policy identifiers, telemetry bucketing, or session CLI labels.

use anyhow::{Context, Result};
use std::fmt;
use std::time::Duration;

/// Listing distributions does not start them. Never infer running state from
/// registered distributions when the separate running-state query fails.
pub(crate) async fn list_wsl_distros(running_only: bool) -> Result<Vec<String>> {
    let mut command = tokio::process::Command::new("wsl.exe");
    command.arg("--list");
    if running_only {
        command.arg("--running");
    }
    command
        .arg("--quiet")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    let output = tokio::time::timeout(Duration::from_secs(10), command.output())
        .await
        .context("WSL distribution enumeration timed out")?
        .context("enumerate WSL distributions")?;
    anyhow::ensure!(
        output.status.success(),
        "WSL distribution enumeration failed ({})",
        output.status
    );
    parse_wsl_distros(&output.stdout)
}

pub(crate) fn parse_wsl_distros(bytes: &[u8]) -> Result<Vec<String>> {
    anyhow::ensure!(
        bytes.len() <= 1024 * 1024,
        "WSL distribution list is too large"
    );
    let utf16 =
        bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff]) || bytes.contains(&0);
    let text = if utf16 {
        let big_endian = bytes.starts_with(&[0xfe, 0xff]);
        let bytes = if bytes.starts_with(&[0xff, 0xfe]) || big_endian {
            &bytes[2..]
        } else {
            bytes
        };
        anyhow::ensure!(
            bytes.len() % 2 == 0,
            "Malformed UTF-16 WSL distribution list"
        );
        let units: Vec<_> = bytes
            .chunks_exact(2)
            .map(|pair| {
                if big_endian {
                    u16::from_be_bytes([pair[0], pair[1]])
                } else {
                    u16::from_le_bytes([pair[0], pair[1]])
                }
            })
            .collect();
        String::from_utf16(&units).context("decode UTF-16 WSL distribution list")?
    } else {
        std::str::from_utf8(bytes)
            .context("decode UTF-8 WSL distribution list")?
            .to_string()
    };
    let mut names: Vec<_> = text
        .trim_start_matches('\u{feff}')
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .filter(|name| is_safe_wsl_distro_name(name))
        .filter(|name| {
            let lower = name.to_ascii_lowercase();
            !lower.starts_with("docker-desktop") && !lower.starts_with("rancher-desktop")
        })
        .map(str::to_string)
        .collect();
    names.sort_by_key(|name| (name.to_ascii_lowercase(), name.clone()));
    names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    Ok(names)
}

pub(crate) async fn ensure_wsl_running(distro: &str) -> Result<()> {
    anyhow::ensure!(
        is_safe_wsl_distro_name(distro),
        "Invalid WSL distribution name"
    );
    let running = list_wsl_distros(true).await?;
    anyhow::ensure!(
        running.iter().any(|name| name.eq_ignore_ascii_case(distro)),
        "WSL distribution is not running: {distro}"
    );
    Ok(())
}

pub(crate) fn is_safe_wsl_distro_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 256
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'+'))
}

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

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
    /// Absent or legacy source kinds retain the host default. Explicit WSL
    /// requests must name a valid distro and never fall back to a host agent.
    pub fn from_wire(kind: Option<&str>, distro: Option<&str>) -> anyhow::Result<Self> {
        match kind.map(str::trim) {
            Some(kind) if kind.eq_ignore_ascii_case(Self::WSL_KIND) => {
                let distro = distro
                    .map(str::trim)
                    .filter(|name| is_safe_wsl_distro_name(name))
                    .ok_or_else(|| anyhow::anyhow!("Invalid or missing WSL distribution name"))?;
                Ok(Self::Wsl {
                    distro: distro.to_string(),
                })
            }
            _ => Ok(Self::Host),
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
/// Host agents use the working directory reported by Terminal. WSL agents
/// require an absolute POSIX path, so translate the common WT forms and
/// resolve `~`/relative paths against the distro's real `$HOME`.
pub async fn resolve_source_cwd(source: &AgentSource, reported: Option<&str>) -> Option<String> {
    let reported = reported.map(str::trim).filter(|cwd| !cwd.is_empty());
    let AgentSource::Wsl { distro } = source else {
        return reported.map(str::to_string);
    };
    resolve_wsl_cwd_with(
        distro,
        reported,
        |path| async move { translate_windows_cwd(distro, &path).await },
        || resolve_wsl_home(distro),
    )
    .await
}

async fn resolve_wsl_cwd_with<T, TF, H, HF>(
    distro: &str,
    reported: Option<&str>,
    translate: T,
    home: H,
) -> Option<String>
where
    T: FnOnce(String) -> TF,
    TF: std::future::Future<Output = Option<String>>,
    H: FnOnce() -> HF,
    HF: std::future::Future<Output = Option<String>>,
{
    if let Some(cwd) = reported.and_then(|cwd| normalize_wsl_cwd(distro, cwd)) {
        return Some(cwd);
    }
    if let Some(path) = reported.and_then(absolute_windows_drive_path) {
        if let Some(mapped) = translate(path.to_string()).await {
            return Some(mapped);
        }
        tracing::warn!(
            target: "agent_source",
            %distro,
            "Windows working directory is not accessible in the selected distro; using its home"
        );
    }
    let home = home().await.unwrap_or_else(|| {
        tracing::warn!(target: "agent_source", %distro, "cannot resolve WSL home; using filesystem root");
        "/".to_string()
    });
    match reported {
        Some("~") | None => Some(home),
        Some(relative) if relative.starts_with("~/") => {
            Some(format!("{}/{}", home.trim_end_matches('/'), &relative[2..]))
        }
        Some(relative) if !relative.contains(':') && !relative.starts_with(['\\', '/']) => {
            Some(format!(
                "{}/{}",
                home.trim_end_matches('/'),
                normalize_wsl_relative_cwd(relative)
            ))
        }
        _ => Some(home),
    }
}

fn normalize_wsl_relative_cwd(cwd: &str) -> String {
    cwd.replace('\\', "/").trim_start_matches("./").to_string()
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
        if normalized
            .get(..prefix.len())
            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(&prefix))
        {
            return Some(format!("/{}", &normalized[prefix.len()..]));
        }
    }
    if normalized.starts_with("//") {
        return None;
    }
    if cwd.starts_with('/') {
        return Some(cwd.to_string());
    }

    None
}

pub(crate) fn absolute_windows_drive_path(path: &str) -> Option<&str> {
    let path = path
        .strip_prefix(r"\\?\")
        .or_else(|| path.strip_prefix(r"\\.\"))
        .unwrap_or(path);
    let bytes = path.as_bytes();
    (bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/'))
    .then_some(path)
}

async fn translate_windows_cwd(distro: &str, path: &str) -> Option<String> {
    if !is_safe_wsl_distro_name(distro) {
        return None;
    }
    let mut command = tokio::process::Command::new("wsl.exe");
    command
        .args([
            "-d",
            distro,
            "--exec",
            "sh",
            "-c",
            // wslpath alone can name an unmounted drive. Verify accessibility
            // in the same distro; the Windows path is data, never shell code.
            "mapped=$(wslpath -a -u \"$1\") && [ -n \"$mapped\" ] && [ -d \"$mapped\" ] && printf '%s' \"$mapped\"",
            "wta-cwd",
            path,
        ])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    let output = match tokio::time::timeout(Duration::from_secs(10), command.output()).await {
        Ok(Ok(output)) if output.status.success() => output,
        Ok(Ok(output)) => {
            tracing::warn!(target: "agent_source", %distro, status = %output.status, "WSL cwd translation or directory check failed");
            return None;
        }
        Ok(Err(error)) => {
            tracing::warn!(target: "agent_source", %distro, %error, "cannot run WSL cwd translation");
            return None;
        }
        Err(_) => {
            tracing::warn!(target: "agent_source", %distro, "WSL cwd translation timed out");
            return None;
        }
    };
    match String::from_utf8(output.stdout) {
        Ok(path) if path.starts_with('/') && !path.contains(['\0', '\r', '\n']) => Some(path),
        _ => {
            tracing::warn!(target: "agent_source", %distro, "WSL cwd translation returned an invalid absolute path");
            None
        }
    }
}

pub(crate) async fn resolve_wsl_home(distro: &str) -> Option<String> {
    if !is_safe_wsl_distro_name(distro) {
        return None;
    }
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
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);

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
    fn wsl_enumeration_handles_encodings_filters_and_deduplicates() {
        let text = "\u{feff}Ubuntu\r\nDebian\r\nubuntu\r\ndocker-desktop-data\r\nrancher-desktop\r\nbad name\r\n";
        let expected = vec!["Debian", "Ubuntu"];
        assert_eq!(parse_wsl_distros(text.as_bytes()).unwrap(), expected);
        for bom in [false, true] {
            let mut bytes = if bom { vec![0xff, 0xfe] } else { Vec::new() };
            bytes.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
            assert_eq!(parse_wsl_distros(&bytes).unwrap(), expected);
        }
        let mut bytes = vec![0xfe, 0xff];
        bytes.extend(text.encode_utf16().flat_map(u16::to_be_bytes));
        assert_eq!(parse_wsl_distros(&bytes).unwrap(), expected);
        assert!(parse_wsl_distros(&[]).unwrap().is_empty());
        assert!(parse_wsl_distros(&[0xff, 0xfe, 1]).is_err());
        assert!(parse_wsl_distros(&[0xff, 0xfe, 0x00, 0xd8]).is_err());
        assert!(parse_wsl_distros(&[0xff]).is_err());
    }

    #[test]
    fn wire_source_does_not_accept_unsafe_wsl_metadata() {
        for distro in [
            "Ubuntu&echo-marker",
            "Ubuntu;echo-marker",
            "Ubuntu extra",
            "../Ubuntu",
            "Ubuntu\"",
            "Ubuntu\nextra",
        ] {
            assert!(
                AgentSource::from_wire(Some("wsl"), Some(distro)).is_err(),
                "An unsafe wire distro must be rejected, not routed to WSL or a host agent."
            );
        }
        let too_long = "x".repeat(257);
        assert!(AgentSource::from_wire(Some("wsl"), Some(&too_long)).is_err());
        assert!(AgentSource::from_wire(Some("wsl"), None).is_err());
    }

    #[test]
    fn wire_source_requires_a_nonempty_wsl_distro() {
        assert_eq!(
            AgentSource::from_wire(Some("wsl"), Some("Ubuntu")).unwrap(),
            AgentSource::Wsl {
                distro: "Ubuntu".to_string()
            }
        );
        assert!(AgentSource::from_wire(Some("wsl"), Some(" ")).is_err());
        assert_eq!(
            AgentSource::from_wire(Some("unknown"), Some("Ubuntu")).unwrap(),
            AgentSource::Host
        );
        assert_eq!(
            AgentSource::from_wire(None, None).unwrap(),
            AgentSource::Host
        );
        assert_eq!(
            AgentSource::from_wire(Some("host"), None).unwrap(),
            AgentSource::Host
        );
        assert_eq!(
            AgentSource::from_wire(Some(" WSL "), Some(" Ubuntu-24.04+dev ")).unwrap(),
            AgentSource::Wsl {
                distro: "Ubuntu-24.04+dev".into(),
            }
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

    #[tokio::test]
    async fn host_source_preserves_reported_cwd() {
        assert_eq!(
            resolve_source_cwd(&AgentSource::Host, Some(r"C:\work\project")).await,
            Some(r"C:\work\project".to_string())
        );
        assert_eq!(
            resolve_source_cwd(&AgentSource::Host, Some("  ")).await,
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
        assert_eq!(normalize_wsl_cwd("Ubuntu", r"C:\src\project"), None);
        assert_eq!(normalize_wsl_cwd("Ubuntu", "~"), None);
        assert_eq!(normalize_wsl_cwd("Ubuntu", r"\\wsl$\Debian\home\me"), None);
        assert_eq!(normalize_wsl_cwd("Ubuntu", "//wsl$/Debian/home/me"), None);
        assert_eq!(
            normalize_wsl_cwd("Ubuntu", "//wsl.localhost/Debian/home/me"),
            None
        );
        assert_eq!(
            normalize_wsl_cwd("Ubuntu", "/home/路径/项目").as_deref(),
            Some("/home/路径/项目")
        );
    }

    #[test]
    fn wsl_cwd_normalizes_unc_roots_with_trailing_separator() {
        for cwd in [
            r"\\wsl$\Ubuntu\",
            r"\\wsl.localhost\Ubuntu\",
            "//wsl$/Ubuntu/",
            "//wsl.localhost/Ubuntu/",
        ] {
            assert_eq!(normalize_wsl_cwd("Ubuntu", cwd).as_deref(), Some("/"));
        }
    }

    #[test]
    fn wsl_relative_cwd_uses_posix_separators() {
        assert_eq!(normalize_wsl_relative_cwd(r"foo\bar"), "foo/bar");
        assert_eq!(normalize_wsl_relative_cwd(r".\foo"), "foo");
        assert_eq!(normalize_wsl_relative_cwd("./foo"), "foo");
        assert_eq!(normalize_wsl_relative_cwd("../foo"), "../foo");
    }

    #[tokio::test]
    async fn wsl_cwd_uses_the_distro_mapping_not_a_mount_root_guess() {
        for path in [r"C:\src\project", r"\\?\C:\src\project"] {
            let cwd = resolve_wsl_cwd_with(
                "Ubuntu",
                Some(path),
                |path| async move {
                    assert_eq!(path, r"C:\src\project");
                    Some("/windows/c/src/project".to_string())
                },
                || async { panic!("a mapped directory does not require home discovery") },
            )
            .await;
            assert_eq!(cwd.as_deref(), Some("/windows/c/src/project"));
        }
    }

    #[tokio::test]
    async fn wsl_cwd_unmounted_drive_uses_real_home() {
        let cwd = resolve_wsl_cwd_with(
            "Ubuntu",
            Some(r"Q:\unmounted\project"),
            |_| async { None },
            || async { Some("/home/developer".to_string()) },
        )
        .await;
        assert_eq!(cwd.as_deref(), Some("/home/developer"));
    }

    #[tokio::test]
    async fn wsl_cwd_preserves_native_paths_and_rejects_foreign_namespaces() {
        for (path, expected) in [
            ("/home/开发/project", "/home/开发/project"),
            (r"\\wsl.localhost\Ubuntu\home\me", "/home/me"),
            (r"\\?\UNC\WSL$\UBUNTU\home\me", "/home/me"),
            (r"\\wsl$\Debian\home\other", "/home/me"),
            (r"\\server\share", "/home/me"),
            ("C:relative", "/home/me"),
            ("~/project", "/home/me/project"),
            (r"relative\project", "/home/me/relative/project"),
        ] {
            let cwd = resolve_wsl_cwd_with(
                "Ubuntu",
                Some(path),
                |_| async { panic!("no rooted Windows drive was supplied") },
                || async { Some("/home/me".to_string()) },
            )
            .await;
            assert_eq!(cwd.as_deref(), Some(expected), "{path}");
        }
    }
}
