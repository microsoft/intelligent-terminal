//! Persistence abstraction for canonical Remote Agent Host state.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::Mutex;

use anyhow::{Context, Result};

use super::state::PersistedHostState;

/// Store canonical host state. Wire-level AHP requests and envelopes never
/// enter this interface, making transport or SDK changes non-breaking.
pub(crate) trait HostStateStore: Send + Sync {
    fn load(&self) -> Result<Option<PersistedHostState>>;
    fn save(&self, state: &PersistedHostState) -> Result<()>;
}

/// JSON file store for the local host process.
pub(crate) struct FileHostStateStore {
    path: PathBuf,
    _lock: File,
}

impl FileHostStateStore {
    pub(crate) fn new(path: PathBuf) -> Result<Self> {
        let parent = path
            .parent()
            .context("Remote Agent Host state path has no parent")?;
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "create Remote Agent Host state directory {}",
                parent.display()
            )
        })?;
        let lock_path = path.with_extension("lock");
        let lock = open_exclusive_lock(&lock_path).with_context(|| {
            format!(
                "lock Remote Agent Host state {}; another host may already be running",
                path.display()
            )
        })?;
        Ok(Self { path, _lock: lock })
    }
}

impl HostStateStore for FileHostStateStore {
    fn load(&self) -> Result<Option<PersistedHostState>> {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("parse Remote Agent Host state {}", self.path.display()))
                .map(Some),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error)
                .with_context(|| format!("read Remote Agent Host state {}", self.path.display())),
        }
    }

    fn save(&self, state: &PersistedHostState) -> Result<()> {
        let bytes =
            serde_json::to_vec_pretty(state).context("serialize Remote Agent Host state")?;
        let temporary_path = self.path.with_extension(format!(
            "tmp-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let save_result = (|| -> Result<()> {
            let mut temporary = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary_path)
                .with_context(|| {
                    format!(
                        "create temporary Remote Agent Host state {}",
                        temporary_path.display()
                    )
                })?;
            temporary
                .write_all(&bytes)
                .context("write temporary Remote Agent Host state")?;
            temporary
                .sync_all()
                .context("flush temporary Remote Agent Host state")?;
            drop(temporary);
            replace_file(&temporary_path, &self.path)
                .with_context(|| format!("replace Remote Agent Host state {}", self.path.display()))
        })();
        if save_result.is_err() {
            let _ = std::fs::remove_file(&temporary_path);
        }
        save_result
    }
}

#[cfg(windows)]
fn open_exclusive_lock(path: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;

    OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .share_mode(0)
        .open(path)
}

#[cfg(not(windows))]
fn open_exclusive_lock(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(path)
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::rename(source, destination)
}

/// In-memory state store used by reducer tests.
#[cfg(test)]
pub(crate) struct MemoryHostStateStore {
    state: Mutex<Option<PersistedHostState>>,
}

#[cfg(test)]
impl MemoryHostStateStore {
    pub(crate) fn empty() -> Self {
        Self {
            state: Mutex::new(None),
        }
    }
}

#[cfg(test)]
impl HostStateStore for MemoryHostStateStore {
    fn load(&self) -> Result<Option<PersistedHostState>> {
        Ok(self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone())
    }

    fn save(&self, state: &PersistedHostState) -> Result<()> {
        *self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(state.clone());
        Ok(())
    }
}
