//! Response-only in-use evidence for native Copilot sessions without a live IT registration.

use crate::agent_sessions::{AgentStatus, CliSource, SessionLocation, SessionOrigin};
use crate::session_registry::SessionInfo;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant, SystemTime};

static PROBE_GATE: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(1)));

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProbeError {
    Busy,
    Timeout,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Process {
    created: SystemTime,
    image: PathBuf,
}

fn session_root() -> Option<PathBuf> {
    let home = std::env::var_os("COPILOT_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join(".copilot"))
        })?;
    home.is_absolute().then(|| home.join("session-state"))
}

fn native_process(pid: u32) -> Result<Option<Process>, ProbeError> {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, GetProcessTimes, OpenProcess, QueryFullProcessImageNameW,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    // The handle is query-only, and is closed on every success/failure path.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return if std::io::Error::last_os_error().raw_os_error() == Some(87) {
                Ok(None)
            } else {
                Err(ProbeError::Unavailable)
            };
        }
        let result = (|| {
            let mut exit = 0;
            if GetExitCodeProcess(handle, &mut exit) == 0 {
                return Err(ProbeError::Unavailable);
            }
            if exit != 259 {
                return Ok(None);
            }
            let zero = FILETIME {
                dwLowDateTime: 0,
                dwHighDateTime: 0,
            };
            let (mut created, mut ended, mut kernel, mut user) = (zero, zero, zero, zero);
            if GetProcessTimes(handle, &mut created, &mut ended, &mut kernel, &mut user) == 0 {
                return Err(ProbeError::Unavailable);
            }
            let ticks = ((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64;
            let unix = ticks
                .checked_sub(116_444_736_000_000_000)
                .ok_or(ProbeError::Unavailable)?;
            let created = SystemTime::UNIX_EPOCH
                .checked_add(Duration::from_nanos(
                    unix.checked_mul(100).ok_or(ProbeError::Unavailable)?,
                ))
                .ok_or(ProbeError::Unavailable)?;
            let mut image = vec![0u16; 32768];
            let mut length = image.len() as u32;
            if QueryFullProcessImageNameW(handle, 0, image.as_mut_ptr(), &mut length) == 0 {
                return Err(ProbeError::Unavailable);
            }
            Ok(Some(Process {
                created,
                image: PathBuf::from(
                    String::from_utf16(&image[..length as usize])
                        .map_err(|_| ProbeError::Unavailable)?,
                ),
            }))
        })();
        CloseHandle(handle);
        result
    }
}

fn marker_process(
    name: &str,
    bytes: &[u8],
    modified: SystemTime,
    lookup: &impl Fn(u32) -> Option<Process>,
) -> Option<(u32, Process)> {
    let pid: u32 = name
        .strip_prefix("inuse.")?
        .strip_suffix(".lock")?
        .parse()
        .ok()?;
    let content: u32 = std::str::from_utf8(bytes).ok()?.trim().parse().ok()?;
    if pid == 0 || pid != content {
        return None;
    }
    let process = lookup(pid)?;
    if process.created > modified
        || !process
            .image
            .file_name()?
            .to_str()?
            .eq_ignore_ascii_case("copilot.exe")
    {
        return None;
    }
    Some((pid, process))
}

pub(crate) fn eligible(row: &SessionInfo) -> bool {
    row.provider_id.as_deref() == Some("copilot")
        && row.cli_source == Some(CliSource::Copilot)
        && row.location == SessionLocation::Host
        && row.session_universe.is_none()
        && row.origin != Some(SessionOrigin::AgentPane)
        // Only history rows are probed. Live master registrations already
        // carry authoritative activity and must keep it unchanged.
        && matches!(
            row.status,
            Some(AgentStatus::Historical | AgentStatus::Ended)
        )
        && uuid::Uuid::parse_str(row.session_id.0.as_ref()).is_ok()
}

fn optional_io<T>(result: std::io::Result<T>) -> Result<Option<T>, ProbeError> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(ProbeError::Unavailable),
    }
}

fn held_lease(path: &Path, created: SystemTime) -> Result<Option<SystemTime>, ProbeError> {
    use std::os::windows::fs::OpenOptionsExt;
    let Some(metadata) = optional_io(std::fs::metadata(path))? else {
        return Ok(None);
    };
    let modified = metadata.modified().map_err(|_| ProbeError::Unavailable)?;
    if metadata.created().map_err(|_| ProbeError::Unavailable)? < created || modified < created {
        return Ok(None);
    }
    // SDK 1.0.80 holds this zero-length file open while the session is in use.
    // A leftover filename is insufficient: only sharing violation proves a lease.
    match std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(path)
    {
        Err(error) if error.raw_os_error() == Some(32) => Ok(Some(modified)),
        result => optional_io(result).map(|_| None),
    }
}

fn probe_status(
    row: &SessionInfo,
    root: &Path,
    lookup: &impl Fn(u32) -> Result<Option<Process>, ProbeError>,
    deadline: Instant,
) -> Result<Option<AgentStatus>, ProbeError> {
    if !eligible(row) {
        return Ok(None);
    }
    if Instant::now() >= deadline {
        return Err(ProbeError::Timeout);
    }
    let Some(root) = optional_io(root.canonicalize())? else {
        return Ok(None);
    };
    let Some(directory) = optional_io(root.join(row.session_id.0.as_ref()).canonicalize())? else {
        return Ok(None);
    };
    if directory.parent() != Some(root.as_path()) {
        return Err(ProbeError::Unavailable);
    }
    let mut owner = None;
    for (index, entry) in std::fs::read_dir(&directory)
        .map_err(|_| ProbeError::Unavailable)?
        .enumerate()
    {
        if Instant::now() >= deadline {
            return Err(ProbeError::Timeout);
        }
        if index >= 64 {
            return Err(ProbeError::Unavailable);
        }
        let entry = entry.map_err(|_| ProbeError::Unavailable)?;
        let name = entry.file_name();
        let Some(name) = name
            .to_str()
            .filter(|name| name.starts_with("inuse.") && name.ends_with(".lock"))
        else {
            continue;
        };
        let mut bytes = Vec::new();
        let Some(mut marker) = optional_io(std::fs::File::open(entry.path()))? else {
            continue;
        };
        let metadata = marker.metadata().map_err(|_| ProbeError::Unavailable)?;
        if metadata.len() > 64 {
            return Err(ProbeError::Unavailable);
        }
        let modified = metadata.modified().map_err(|_| ProbeError::Unavailable)?;
        (&mut marker)
            .take(64)
            .read_to_end(&mut bytes)
            .map_err(|_| ProbeError::Unavailable)?;
        let lookup_error = std::cell::Cell::new(None);
        let process = marker_process(name, &bytes, modified, &|pid| match lookup(pid) {
            Ok(process) => process,
            Err(error) => {
                lookup_error.set(Some(error));
                None
            }
        });
        if let Some(error) = lookup_error.get() {
            return Err(error);
        }
        if let Some((pid, process)) = process {
            if held_lease(
                &directory.join(format!("inuse.{pid}.hold")),
                process.created,
            )?
            .is_none()
            {
                continue;
            }
            if owner.replace(pid).is_some() {
                return Err(ProbeError::Unavailable);
            }
        }
    }
    if Instant::now() >= deadline {
        return Err(ProbeError::Timeout);
    }
    Ok(owner.map(|_| AgentStatus::InUse))
}

async fn run_probe<T: Send + 'static>(
    gate: Arc<tokio::sync::Semaphore>,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, ProbeError> {
    let permit = gate.try_acquire_owned().map_err(|_| ProbeError::Busy)?;
    let task = tokio::task::spawn_blocking(move || {
        // Keep admission occupied even after the async waiter times out or is dropped.
        let _permit = permit;
        work()
    });
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .map_err(|_| ProbeError::Timeout)?
        .map_err(|_| ProbeError::Unavailable)
}

pub(crate) async fn activation_status(
    row: &SessionInfo,
) -> Result<Option<AgentStatus>, ProbeError> {
    if !eligible(row) {
        return Ok(None);
    }
    let root = session_root().ok_or(ProbeError::Unavailable)?;
    let row = row.clone();
    run_probe(PROBE_GATE.clone(), move || {
        probe_status(
            &row,
            &root,
            &native_process,
            Instant::now() + Duration::from_secs(2),
        )
    })
    .await?
}

pub(crate) async fn enrich_snapshot(rows: &mut Vec<SessionInfo>) {
    let Some(root) = session_root() else { return };
    let snapshot = rows.clone();
    let result = run_probe(PROBE_GATE.clone(), move || {
        let mut rows = snapshot;
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut order: Vec<_> = (0..rows.len())
            .filter(|index| eligible(&rows[*index]))
            .collect();
        order.sort_by_key(|index| std::cmp::Reverse(rows[*index].last_activity_at_ms));
        for index in order {
            let row = &mut rows[index];
            match probe_status(row, &root, &native_process, deadline) {
                Ok(Some(in_use)) => row.status = Some(in_use),
                Ok(None) => {}
                Err(error) => {
                    tracing::debug!(target: "copilot_status", ?error, "session in-use evidence unavailable; retaining original status");
                }
            }
        }
        rows
    })
    .await;
    match result {
        Ok(snapshot) => *rows = snapshot,
        Err(error) => {
            tracing::debug!(target: "copilot_status", ?error, "in-use probe unavailable; retaining original statuses");
        }
    }
}

#[cfg(test)]
fn status(
    row: &SessionInfo,
    root: &Path,
    lookup: &impl Fn(u32) -> Option<Process>,
    deadline: Instant,
) -> Option<AgentStatus> {
    probe_status(row, root, &|pid| Ok(lookup(pid)), deadline)
        .ok()
        .flatten()
}

#[cfg(test)]
#[path = "copilot_status_tests.rs"]
mod tests;
