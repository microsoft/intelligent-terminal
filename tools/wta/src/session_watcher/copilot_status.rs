//! Response-only activity evidence for native Copilot sessions without a local pane.

use crate::agent_sessions::{AgentStatus, CliSource, SessionEvent, SessionLocation, SessionOrigin};
use crate::session_registry::SessionInfo;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

const MAX_TAIL: u64 = 4 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Process {
    created: SystemTime,
    image: PathBuf,
}

#[derive(Clone)]
struct Tail {
    pid: u32,
    process_created: SystemTime,
    file_created: SystemTime,
    modified: SystemTime,
    lease_modified: SystemTime,
    marker_modified: SystemTime,
    offset: u64,
    phase: Option<AgentStatus>,
}

#[derive(Default)]
struct Cache(HashMap<PathBuf, Tail>);

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
    cache: &mut Cache,
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
            let Some(lease) = held_lease(
                &directory.join(format!("inuse.{pid}.hold")),
                process.created,
            ) else {
                continue;
            };
            if owner.replace((pid, process, modified, lease)).is_some() {
                return None;
            }
        }
    }
    let Some((pid, process, marker_modified, lease_modified)) = owner else {
        cache.0.remove(&directory);
        return None;
    };
    let path = directory.join("events.jsonl");
    let mut file = std::fs::File::open(&path).ok()?;
    let metadata = file.metadata().ok()?;
    let (length, created, modified) = (
        metadata.len(),
        metadata.created().ok()?,
        metadata.modified().ok()?,
    );
    let prior = cache.0.remove(&directory);
    let prior = prior.filter(|tail| {
        tail.pid == pid
            && tail.process_created == process.created
            && tail.file_created == created
            && tail.marker_modified == marker_modified
            && tail.lease_modified == lease_modified
            && length >= tail.offset
            && length - tail.offset <= MAX_TAIL
            && (length != tail.offset || modified == tail.modified)
    });
    let bootstrap = prior.is_none();
    let mut tail = prior.unwrap_or(Tail {
        pid,
        process_created: process.created,
        file_created: created,
        modified,
        marker_modified,
        lease_modified,
        offset: length.saturating_sub(MAX_TAIL),
        phase: None,
    });
    let from = tail.offset;
    file.seek(SeekFrom::Start(from)).ok()?;
    let mut bytes = Vec::new();
    file.take(length - from).read_to_end(&mut bytes).ok()?;
    let start = if bootstrap && from != 0 {
        bytes.iter().position(|byte| *byte == b'\n')? + 1
    } else {
        0
    };
    let text = std::str::from_utf8(&bytes[start..]).ok()?;
    // An incomplete record cannot establish a current phase.
    if !text.is_empty() && !text.ends_with('\n') {
        return None;
    }
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        if Instant::now() >= deadline {
            return None;
        }
        let record: serde_json::Value = serde_json::from_str(line).ok()?;
        let kind = record
            .get("type")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        for event in super::classify_copilot::classify(&record, &row.session_id.to_string()) {
            match event {
                SessionEvent::ToolStarting { .. }
                    if tail.phase.is_some() || kind == "assistant.turn_start" =>
                {
                    tail.phase = Some(AgentStatus::Working)
                }
                SessionEvent::ToolCompleted { .. } => tail.phase = Some(AgentStatus::Idle),
                SessionEvent::Notification { .. } => tail.phase = Some(AgentStatus::Attention),
                _ => {}
            }
        }
    }
    tail.offset = length;
    tail.modified = modified;
    let phase = tail.phase.clone();
    if cache.0.len() >= 256 {
        cache.0.clear();
    }
    cache.0.insert(directory, tail);
    phase
}

pub(crate) async fn enrich_snapshot(rows: &mut Vec<SessionInfo>) {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    let Some(root) = session_root() else { return };
    let snapshot = rows.clone();
    let task = tokio::task::spawn_blocking(move || {
        let mut rows = snapshot;
        let Ok(mut cache) = CACHE
            .get_or_init(|| Mutex::new(Cache::default()))
            .try_lock()
        else {
            return rows;
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut order: Vec<_> = (0..rows.len())
            .filter(|index| eligible(&rows[*index]))
            .collect();
        order.sort_by_key(|index| std::cmp::Reverse(rows[*index].last_activity_at_ms));
        for index in order {
            let row = &mut rows[index];
            if let Some(phase) = status(row, &root, &mut cache, &native_process, deadline) {
                row.status = Some(phase);
            }
        }
        rows
    });
    if let Ok(Ok(snapshot)) = tokio::time::timeout(Duration::from_secs(2), task).await {
        *rows = snapshot;
    }
}

#[cfg(test)]
#[path = "copilot_status_tests.rs"]
mod tests;
