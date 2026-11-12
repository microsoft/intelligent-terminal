//! ACP cwd namespaces and bounded, preflight-only session/new retries.

use std::future::Future;
use std::path::{Path, PathBuf};

use agent_client_protocol as acp;
use tokio::time::Instant;

use crate::agent_source::AgentSource;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CwdNamespace {
    Windows,
    Posix,
}

pub(crate) fn detect_namespace<'a>(
    paths: impl IntoIterator<Item = &'a Path>,
) -> Option<CwdNamespace> {
    let mut detected = None;
    for path in paths {
        let value = path.to_string_lossy();
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        let format = if crate::agent_source::absolute_windows_drive_path(value).is_some() {
            CwdNamespace::Windows
        } else if value.starts_with("//") || value.starts_with(r"\\") {
            return None;
        } else if value.starts_with('/') {
            CwdNamespace::Posix
        } else {
            return None;
        };
        if detected.is_some_and(|previous| previous != format) {
            return None;
        }
        detected = Some(format);
    }
    detected
}

pub(crate) async fn build_cwd_attempts(
    value: &Path,
    source: &AgentSource,
    native_host: bool,
    detected: Option<CwdNamespace>,
) -> Vec<PathBuf> {
    if matches!(source, AgentSource::Wsl { .. }) {
        let raw = value.to_string_lossy();
        let resolved = crate::agent_source::resolve_source_cwd(source, Some(&raw))
            .await
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"));
        let mut attempts = vec![resolved];
        push_distinct(&mut attempts, PathBuf::from("/tmp"));
        return attempts;
    }

    let posix_input = value.to_string_lossy().starts_with('/');
    let windows = if value.is_absolute() && (native_host || !posix_input) {
        value.to_path_buf()
    } else {
        windows_home()
    };
    if native_host {
        if windows != value {
            tracing::warn!(target: "master", "cwd is not in the native agent's Windows namespace; using Windows home");
        }
        return vec![windows];
    }
    // Namespace evidence does not identify a distro or its mounts. An opaque
    // wrapper cannot faithfully translate a Windows project path; never guess.
    let posix = if posix_input {
        value.to_path_buf()
    } else {
        PathBuf::from("/tmp")
    };
    let first_posix = detected == Some(CwdNamespace::Posix) || (detected.is_none() && posix_input);
    let (first, second) = if first_posix {
        (posix, windows)
    } else {
        (windows, posix)
    };
    if first != value {
        tracing::warn!(target: "master", "opaque agent namespace has no known distro mapping; using namespace-local fallback cwd");
    }
    let mut attempts = vec![first];
    push_distinct(&mut attempts, second);
    attempts
}

fn push_distinct(attempts: &mut Vec<PathBuf>, path: PathBuf) {
    if !attempts.contains(&path) {
        attempts.push(path);
    }
}

fn windows_home() -> PathBuf {
    for variable in ["USERPROFILE", "HOME"] {
        if let Some(value) = std::env::var_os(variable).map(PathBuf::from) {
            if value.is_absolute() && !value.to_string_lossy().starts_with('/') {
                return value;
            }
        }
    }
    let drive = std::env::var("SystemDrive").unwrap_or_default();
    let bytes = drive.as_bytes();
    if bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        PathBuf::from(format!("{drive}\\"))
    } else {
        PathBuf::from(r"C:\")
    }
}

#[derive(Debug)]
pub(crate) enum CwdAttemptFailure {
    Agent(acp::Error),
    Timeout,
}

pub(crate) async fn run_cwd_attempts<T, F, Fut>(
    attempts: &[PathBuf],
    deadline: Instant,
    mut operation: F,
) -> Result<(T, PathBuf), CwdAttemptFailure>
where
    F: FnMut(PathBuf) -> Fut,
    Fut: Future<Output = acp::Result<T>>,
{
    for (index, cwd) in attempts.iter().enumerate().take(2) {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or(CwdAttemptFailure::Timeout)?;
        match tokio::time::timeout(remaining, operation(cwd.clone())).await {
            Ok(Ok(response)) => return Ok((response, cwd.clone())),
            Ok(Err(error))
                if index == 0 && attempts.len() > 1 && is_retryable_cwd_rejection(&error, cwd) =>
            {
                tracing::warn!(
                    target: "master",
                    rejected_cwd = %cwd.display(),
                    fallback_cwd = %attempts[1].display(),
                    "agent rejected cwd before session creation; retrying once with fallback cwd"
                );
            }
            Ok(Err(error)) => return Err(CwdAttemptFailure::Agent(error)),
            Err(_) => return Err(CwdAttemptFailure::Timeout),
        }
    }
    Err(CwdAttemptFailure::Agent(acp::Error::internal_error()))
}

fn is_retryable_cwd_rejection(error: &acp::Error, attempted: &Path) -> bool {
    // Echoed error.data can contain the request's cwd even after side effects.
    // Only the primary, deterministic preflight error can authorize a retry.
    let evidence = error.message.to_ascii_lowercase();
    let attempted = attempted
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    let normalized = evidence.replace('\\', "/");
    let references_attempt = !attempted.is_empty()
        && normalized
            .match_indices(&attempted)
            .any(|(start, matched)| {
                let end = start + matched.len();
                let boundary = |ch: char| {
                    !ch.is_alphanumeric() && !matches!(ch, '/' | '\\' | '.' | '_' | '-' | '~' | ':')
                };
                normalized[..start].chars().next_back().is_none_or(boundary)
                    && normalized[end..].chars().next().is_none_or(boundary)
            });
    let declares_cwd = [
        "cwd",
        "working directory",
        "current directory",
        "directory path",
    ]
    .iter()
    .any(|field| evidence.contains(field));
    let preflight = [
        "must be absolute",
        "not absolute",
        "not a directory",
        "no such file or directory",
        "does not exist",
        "cannot be accessed",
        "invalid working directory",
        "failed to set current directory",
    ]
    .iter()
    .any(|reason| evidence.contains(reason));
    preflight && (references_attempt || declares_cwd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::time::Duration;

    #[test]
    fn history_namespace_requires_unambiguous_consensus() {
        for (paths, expected) in [
            (vec!["/home/me", "/work"], Some(CwdNamespace::Posix)),
            (vec![r"C:\repo", r"D:\work"], Some(CwdNamespace::Windows)),
            (vec![r"\\?\C:\repo"], Some(CwdNamespace::Windows)),
            (vec!["/home/me", r"C:\repo"], None),
            (vec![r"\\wsl$\Ubuntu\home\me"], None),
            (vec!["//server/share"], None),
            (vec!["relative", "/home/me"], None),
            (vec![""], None),
            (vec![], None),
        ] {
            assert_eq!(detect_namespace(paths.iter().map(Path::new)), expected);
        }
    }

    #[tokio::test]
    async fn explicit_sources_preserve_cwd_and_do_not_guess_mounts() {
        let value = Path::new(r"C:\Windows\System32");
        assert_eq!(
            build_cwd_attempts(value, &AgentSource::Host, true, Some(CwdNamespace::Posix)).await,
            vec![value.to_path_buf()]
        );
        assert_eq!(
            build_cwd_attempts(value, &AgentSource::Host, false, None).await,
            vec![value.to_path_buf(), PathBuf::from("/tmp")]
        );
        assert_eq!(
            build_cwd_attempts(value, &AgentSource::Host, false, Some(CwdNamespace::Posix)).await,
            vec![PathBuf::from("/tmp"), value.to_path_buf()]
        );
        let source = AgentSource::Wsl {
            distro: "Ubuntu".into(),
        };
        assert_eq!(
            build_cwd_attempts(Path::new("/windows/c/project"), &source, false, None).await,
            vec![PathBuf::from("/windows/c/project"), PathBuf::from("/tmp")]
        );
        assert_eq!(
            build_cwd_attempts(Path::new("/tmp"), &source, false, None).await,
            vec![PathBuf::from("/tmp")]
        );
    }

    #[tokio::test]
    async fn native_host_preserves_windows_unc_but_not_posix_cwd() {
        for cwd in [
            "//server/share/project",
            r"\\server\share\project",
            "//wsl$/Ubuntu/home/me/project",
        ] {
            let value = Path::new(cwd);
            assert_eq!(
                build_cwd_attempts(value, &AgentSource::Host, true, Some(CwdNamespace::Posix))
                    .await,
                vec![value.to_path_buf()],
                "{cwd}"
            );
        }
        let attempts = build_cwd_attempts(
            Path::new("/home/me/project"),
            &AgentSource::Host,
            true,
            None,
        )
        .await;
        assert_eq!(attempts, vec![windows_home()]);
        assert!(attempts[0].is_absolute());
    }

    #[tokio::test]
    async fn only_proven_cwd_rejection_retries_and_returns_successful_cwd() {
        for (message, retry) in [
            ("Directory path must be absolute", true),
            ("cwd does not exist", true),
            ("'/project' is not a directory", true),
            ("'/project/missing-tool' does not exist", false),
            ("tool does not exist", false),
            ("authentication required", false),
            ("session already created", false),
        ] {
            let calls = Cell::new(0);
            let attempts = [PathBuf::from("/project"), PathBuf::from("/tmp")];
            let result =
                run_cwd_attempts(&attempts, Instant::now() + Duration::from_secs(1), |_| {
                    calls.set(calls.get() + 1);
                    let result = if calls.get() == 1 {
                        Err(acp::Error::new(-32603, message)
                            .data(serde_json::json!({"cwd": "/project"})))
                    } else {
                        Ok("created")
                    };
                    std::future::ready(result)
                })
                .await;
            assert_eq!(calls.get(), if retry { 2 } else { 1 }, "{message}");
            if retry {
                assert_eq!(result.unwrap(), ("created", PathBuf::from("/tmp")));
            } else {
                assert!(matches!(result, Err(CwdAttemptFailure::Agent(_))));
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn retry_uses_one_absolute_deadline() {
        let calls = Cell::new(0);
        let attempts = [PathBuf::from("/project"), PathBuf::from("/tmp")];
        let result = run_cwd_attempts(&attempts, Instant::now() + Duration::from_secs(10), |_| {
            calls.set(calls.get() + 1);
            async {
                tokio::time::sleep(Duration::from_secs(6)).await;
                Err::<(), _>(acp::Error::new(-32603, "cwd does not exist"))
            }
        })
        .await;
        assert!(matches!(result, Err(CwdAttemptFailure::Timeout)));
        assert_eq!(calls.get(), 2);
    }
}
