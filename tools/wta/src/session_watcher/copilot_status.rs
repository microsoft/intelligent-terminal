//! Response-only in-use evidence for native Copilot sessions without a live IT registration.

use crate::agent_sessions::{AgentStatus, CliSource, SessionLocation, SessionOrigin};
use crate::session_registry::SessionInfo;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

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

fn native_process(pid: u32) -> Option<Process> {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, GetProcessTimes, OpenProcess, QueryFullProcessImageNameW,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    // The handle is query-only, and is closed on every success/failure path.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let result = (|| {
            let mut exit = 0;
            if GetExitCodeProcess(handle, &mut exit) == 0 || exit != 259 {
                return None;
            }
            let zero = FILETIME {
                dwLowDateTime: 0,
                dwHighDateTime: 0,
            };
            let (mut created, mut ended, mut kernel, mut user) = (zero, zero, zero, zero);
            if GetProcessTimes(handle, &mut created, &mut ended, &mut kernel, &mut user) == 0 {
                return None;
            }
            let ticks = ((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64;
            let unix = ticks.checked_sub(116_444_736_000_000_000)?;
            let created =
                SystemTime::UNIX_EPOCH.checked_add(Duration::from_nanos(unix.checked_mul(100)?))?;
            let mut image = vec![0u16; 32768];
            let mut length = image.len() as u32;
            if QueryFullProcessImageNameW(handle, 0, image.as_mut_ptr(), &mut length) == 0 {
                return None;
            }
            Some(Process {
                created,
                image: PathBuf::from(String::from_utf16(&image[..length as usize]).ok()?),
            })
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

fn eligible(row: &SessionInfo) -> bool {
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

fn held_lease(path: &Path, created: SystemTime) -> Option<SystemTime> {
    use std::os::windows::fs::OpenOptionsExt;
    let metadata = std::fs::metadata(path).ok()?;
    let modified = metadata.modified().ok()?;
    if metadata.created().ok()? < created || modified < created {
        return None;
    }
    // SDK 1.0.80 holds this zero-length file open while the session is in use.
    // A leftover filename is insufficient: only sharing violation proves a lease.
    match std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(path)
    {
        Err(error) if error.raw_os_error() == Some(32) => Some(modified),
        _ => None,
    }
}

fn status(
    row: &SessionInfo,
    root: &Path,
    lookup: &impl Fn(u32) -> Option<Process>,
    deadline: Instant,
) -> Option<AgentStatus> {
    if !eligible(row) || Instant::now() >= deadline {
        return None;
    }
    let root = root.canonicalize().ok()?;
    let directory = root.join(row.session_id.0.as_ref()).canonicalize().ok()?;
    if directory.parent() != Some(root.as_path()) {
        return None;
    }
    let mut owner = None;
    for entry in std::fs::read_dir(&directory).ok()?.take(64) {
        if Instant::now() >= deadline {
            return None;
        }
        let entry = entry.ok()?;
        let name = entry.file_name();
        let Some(name) = name
            .to_str()
            .filter(|name| name.starts_with("inuse.") && name.ends_with(".lock"))
        else {
            continue;
        };
        let mut bytes = Vec::new();
        let mut marker = std::fs::File::open(entry.path()).ok()?;
        let metadata = marker.metadata().ok()?;
        if metadata.len() > 64 {
            return None;
        }
        let modified = metadata.modified().ok()?;
        (&mut marker).take(64).read_to_end(&mut bytes).ok()?;
        if let Some((pid, process)) = marker_process(name, &bytes, modified, lookup) {
            if held_lease(
                &directory.join(format!("inuse.{pid}.hold")),
                process.created,
            )
            .is_none()
            {
                continue;
            }
            if owner.replace(pid).is_some() {
                return None;
            }
        }
    }
    owner.map(|_| AgentStatus::InUse)
}

pub(crate) async fn enrich_snapshot(rows: &mut Vec<SessionInfo>) {
    let Some(root) = session_root() else { return };
    let snapshot = rows.clone();
    let task = tokio::task::spawn_blocking(move || {
        let mut rows = snapshot;
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut order: Vec<_> = (0..rows.len())
            .filter(|index| eligible(&rows[*index]))
            .collect();
        order.sort_by_key(|index| std::cmp::Reverse(rows[*index].last_activity_at_ms));
        for index in order {
            let row = &mut rows[index];
            if let Some(in_use) = status(row, &root, &native_process, deadline) {
                row.status = Some(in_use);
            }
        }
        rows
    });
    match tokio::time::timeout(Duration::from_secs(2), task).await {
        Ok(Ok(snapshot)) => *rows = snapshot,
        Ok(Err(error)) => {
            tracing::warn!(target: "copilot_status", %error, "in-use probe failed; retaining original statuses");
        }
        Err(_) => {
            tracing::debug!(target: "copilot_status", "in-use probe timed out; retaining original statuses");
        }
    }
}

#[cfg(test)]
#[path = "copilot_status_tests.rs"]
mod tests;
