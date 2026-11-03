// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

//! Temporary extractions owned by one native Antigravity ACP process job.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};

use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE};
use windows_sys::Win32::Storage::FileSystem::{
    FileDispositionInfo, GetFileInformationByHandle, GetShortPathNameW, SetFileInformationByHandle,
    BY_HANDLE_FILE_INFORMATION, DELETE, FILE_ATTRIBUTE_REPARSE_POINT, FILE_DISPOSITION_INFO,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
    FILE_SHARE_READ, FILE_SHARE_WRITE,
};

use crate::protocol::acp::process::{
    request_resource_scope, valid_resource_scope, ProcessJob, ProcessResources, ResourceRecovery,
    RESOURCE_SCOPE_ENV, RESOURCE_SCOPE_RECEIPT_PREFIX,
};

const SCHEMA: u32 = 1;
const INSTANCE_LENGTH: usize = 12;
const MAX_RECORD_BYTES: u64 = 4096;
const RETIRED: &[u8] = b"{\"retired\":true}\n";
// ACP 1.4.0 has a 122-unit archive member. Leave room for _MEI, separators
// and the terminator without depending on the bootloader's long-path support.
const MAX_TEMP_PATH_UNITS: usize = 119;

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct DirectoryIdentity {
    volume: u32,
    index: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Ownership {
    schema: u32,
    provider: String,
    source: String,
    scope: String,
    instance: String,
    directory: DirectoryIdentity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent_scope: Option<String>,
}

impl Ownership {
    fn job_name(&self) -> String {
        // Global keeps recovery correct across Windows logon sessions.
        format!("Global\\WtaAcpRuntime-{}-{}", self.scope, self.instance)
    }
}

struct Root {
    path: PathBuf,
    scope: String,
    _pins: Vec<File>,
}

pub(super) struct Runtime {
    root: Root,
    owner: Ownership,
    record: File,
    directory: Option<File>,
    temp: PathBuf,
    job_name: String,
    cleaned: bool,
    retirement_offset: u64,
    retired: bool,
}

impl Runtime {
    pub(super) fn prepare() -> Result<Self> {
        let parent_scope = match std::env::var(RESOURCE_SCOPE_ENV) {
            Ok(scope) => {
                request_resource_scope(&scope)?;
                Some(scope)
            }
            Err(std::env::VarError::NotPresent) => None,
            Err(error) => return Err(error).context("read process resource scope"),
        };
        let root = crate::runtime_paths::acp_runtime_root()?;
        recover_in(&root)?;
        Self::create_with_scope(&root, parent_scope)
    }

    #[cfg(test)]
    fn create(path: &Path) -> Result<Self> {
        Self::create_with_scope(path, None)
    }

    fn create_with_scope(path: &Path, parent_scope: Option<String>) -> Result<Self> {
        let root = open_root(path, true)?.context("ACP runtime root is unavailable")?;
        for _ in 0..8 {
            let instance = uuid::Uuid::new_v4().simple().to_string()[..INSTANCE_LENGTH].to_string();
            let record_path = root.path.join(format!("{instance}.jsonl"));
            let mut record = match record_options().create_new(true).open(&record_path) {
                Ok(record) => record,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error).context("create ACP runtime ownership lease"),
            };
            let directory_path = root.path.join(&instance);
            if let Err(error) = fs::create_dir(&directory_path) {
                delete_record_on_close(&record)?;
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    continue;
                }
                return Err(error).context("create Antigravity runtime directory");
            }
            let directory = open_directory(&directory_path)?;
            let owner = Ownership {
                schema: SCHEMA,
                provider: crate::agent_registry::ANTIGRAVITY_AGENT_ID.to_string(),
                source: "host".to_string(),
                scope: root.scope.clone(),
                instance,
                directory: directory_identity(&directory)?,
                parent_scope: parent_scope.clone(),
            };
            serde_json::to_writer(&mut record, &owner).context("write ACP runtime ownership")?;
            record.write_all(b"\n")?;
            let retirement_offset = record.stream_position()?;
            record.flush().context("flush ACP runtime ownership")?;
            record.sync_all().context("commit ACP runtime ownership")?;
            let mut runtime = Self {
                job_name: owner.job_name(),
                root,
                owner,
                record,
                directory: Some(directory),
                temp: directory_path.clone(),
                cleaned: false,
                retirement_offset,
                retired: false,
            };
            match extraction_path(&directory_path) {
                Ok(temp) => runtime.temp = temp,
                Err(error) => {
                    if let Err(cleanup) = runtime.cleanup() {
                        tracing::warn!(target: "antigravity_runtime", error = %format!("{cleanup:#}"),
                            "failed to reclaim unused runtime directory");
                    }
                    return Err(error);
                }
            }
            tracing::info!(target: "antigravity_runtime", instance = %runtime.owner.instance,
                directory = %runtime.temp.display(), "runtime_created");
            return Ok(runtime);
        }
        bail!("could not allocate a unique Antigravity runtime directory")
    }

    fn record_retirement(&mut self) -> Result<()> {
        if !self.retired {
            write_retirement(&mut self.record, self.retirement_offset)?;
            self.retired = true;
        }
        Ok(())
    }
}

impl ProcessResources for Runtime {
    fn job_name(&self) -> &str {
        &self.job_name
    }

    fn configure(&self, command: &mut tokio::process::Command) {
        command.env("TEMP", &self.temp).env("TMP", &self.temp);
        if let Some(scope) = &self.owner.parent_scope {
            command.env(RESOURCE_SCOPE_ENV, scope);
        }
    }

    fn cleanup(&mut self) -> Result<()> {
        if !self.cleaned {
            self.record_retirement()?;
            ensure!(
                reclaim(
                    &self.root,
                    &self.record,
                    &self.owner,
                    self.directory.take(),
                    true
                )?,
                "Antigravity runtime process exit could not be confirmed"
            );
            self.cleaned = true;
        }
        Ok(())
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        if !self.cleaned {
            tracing::warn!(target: "antigravity_runtime", instance = %self.owner.instance,
                "runtime retained for subsequent recovery");
        }
    }
}

pub(super) fn recover() -> Result<ResourceRecovery> {
    recover_in(&crate::runtime_paths::acp_runtime_root()?)
}

fn recover_in(path: &Path) -> Result<ResourceRecovery> {
    let mut report = ResourceRecovery::default();
    let Some(root) = open_root(path, false)? else {
        return Ok(report);
    };
    for entry in fs::read_dir(&root.path).context("enumerate ACP runtime ownership")? {
        let entry = entry.context("read ACP runtime ownership entry")?;
        let name = entry.file_name();
        let Some(instance) = name.to_str().and_then(|name| name.strip_suffix(".jsonl")) else {
            continue;
        };
        if !valid_instance(instance) {
            continue;
        }
        let result = (|| -> Result<Option<bool>> {
            let mut record = match record_options().open(entry.path()) {
                Ok(record) => record,
                Err(error) if matches!(error.raw_os_error(), Some(32 | 33)) => {
                    return Ok(Some(false))
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error).context("acquire ACP runtime recovery lease"),
            };
            let (owner, retired, retirement_offset) = read_ownership(&record, &root, instance)?;
            let confirmed = retired
                || has_parent_retirement(&root, owner.parent_scope.as_deref())?
                || ProcessJob::named_exit_confirmed(&owner.job_name())?;
            if !confirmed {
                tracing::warn!(target: "antigravity_runtime", instance,
                    "runtime recovery deferred without complete process-exit evidence");
                return Ok(Some(false));
            }
            if !retired {
                write_retirement(&mut record, retirement_offset)?;
            }
            reclaim(&root, &record, &owner, None, true).map(Some)
        })();
        match result {
            Ok(Some(true)) => report.reclaimed += 1,
            Ok(Some(false)) => report.deferred += 1,
            Ok(None) => {}
            Err(error) => {
                report.failed += 1;
                tracing::warn!(target: "antigravity_runtime", instance, error = %format!("{error:#}"),
                    "runtime recovery incomplete; ownership retained");
            }
        }
    }
    prune_unused_receipts(&root)?;
    Ok(report)
}

fn write_retirement(record: &mut File, offset: u64) -> Result<()> {
    ensure!(offset > 0, "invalid ACP retirement record boundary");
    // Preserve the ownership line even if the append is interrupted.
    record.seek(SeekFrom::Start(offset - 1))?;
    record.write_all(b"\n")?;
    record.write_all(RETIRED)?;
    record.set_len(offset + RETIRED.len() as u64)?;
    record.sync_all().context("commit ACP runtime retirement")
}

fn has_parent_retirement(root: &Root, scope: Option<&str>) -> Result<bool> {
    let Some(scope) = scope else { return Ok(false) };
    ensure!(
        valid_resource_scope(scope),
        "invalid ACP runtime parent scope"
    );
    let path = root
        .path
        .join(format!("{RESOURCE_SCOPE_RECEIPT_PREFIX}{scope}"));
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).context("read parent runtime retirement"),
    };
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.len() == 0
            && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0,
        "invalid parent runtime retirement receipt"
    );
    Ok(true)
}

fn prune_unused_receipts(root: &Root) -> Result<()> {
    let mut receipts = Vec::new();
    for entry in fs::read_dir(&root.path)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(scope) = name
            .to_str()
            .and_then(|name| name.strip_prefix(RESOURCE_SCOPE_RECEIPT_PREFIX))
        else {
            return Ok(());
        };
        let metadata = fs::symlink_metadata(entry.path())?;
        if !valid_resource_scope(scope)
            || !metadata.is_file()
            || metadata.len() != 0
            || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        {
            return Ok(());
        }
        receipts.push(entry.path());
    }
    // No runtime directory or record remains. An ended scope cannot create
    // new references; unfamiliar entries keep all receipts conservatively.
    for path in receipts {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("remove unused runtime retirement receipt"),
        }
    }
    Ok(())
}

fn valid_instance(instance: &str) -> bool {
    instance.len() == INSTANCE_LENGTH
        && instance
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn record_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .access_mode(GENERIC_READ | GENERIC_WRITE | DELETE)
        .share_mode(0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    options
}

fn read_ownership(record: &File, root: &Root, instance: &str) -> Result<(Ownership, bool, u64)> {
    let metadata = record.metadata().context("inspect ACP runtime ownership")?;
    ensure!(
        metadata.is_file()
            && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0
            && metadata.len() <= MAX_RECORD_BYTES,
        "ACP runtime ownership is not a bounded regular file"
    );
    let mut bytes = Vec::new();
    record.take(MAX_RECORD_BYTES + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_RECORD_BYTES,
        "ACP runtime ownership is oversized"
    );
    let (json, retired, retirement_offset) = match bytes.iter().position(|byte| *byte == b'\n') {
        Some(end) => (&bytes[..end], &bytes[end + 1..] == RETIRED, end + 1),
        None => (bytes.as_slice(), false, bytes.len() + 1),
    };
    let owner: Ownership = serde_json::from_slice(json).context("parse ACP runtime ownership")?;
    ensure!(
        owner.schema == SCHEMA
            && owner.provider == crate::agent_registry::ANTIGRAVITY_AGENT_ID
            && owner.source == "host"
            && owner.scope == root.scope
            && owner.instance == instance
            && owner
                .parent_scope
                .as_deref()
                .is_none_or(valid_resource_scope),
        "ACP runtime ownership does not match this provider, source and cache"
    );
    Ok((owner, retired, retirement_offset as u64))
}

fn open_root(path: &Path, create: bool) -> Result<Option<Root>> {
    ensure!(path.is_absolute(), "ACP runtime root must be absolute");
    let mut pins = Vec::new();
    // Pin every ancestor without delete sharing so path traversal cannot be
    // redirected between validation, enumeration and removal.
    for component in path.ancestors().collect::<Vec<_>>().into_iter().rev() {
        let directory = match open_directory(component) {
            Ok(directory) => directory,
            Err(error) if is_not_found(&error) => {
                if !create {
                    return Ok(None);
                }
                match fs::create_dir(component) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error).context("create ACP runtime root"),
                }
                open_directory(component)?
            }
            Err(error) => return Err(error),
        };
        pins.push(directory);
    }
    let identity = directory_identity(pins.last().context("ACP runtime root has no directory")?)?;
    Ok(Some(Root {
        path: path.to_path_buf(),
        scope: format!("{:08x}{:016x}", identity.volume, identity.index),
        _pins: pins,
    }))
}

fn open_directory(path: &Path) -> Result<File> {
    let directory = OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .context("open pinned ACP runtime directory")?;
    let metadata = directory.metadata()?;
    ensure!(
        metadata.is_dir() && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0,
        "ACP runtime path contains a reparse point or non-directory"
    );
    Ok(directory)
}

fn directory_identity(directory: &File) -> Result<DirectoryIdentity> {
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: the live directory handle and output buffer have the required types.
    if unsafe { GetFileInformationByHandle(directory.as_raw_handle(), &mut info) } == 0 {
        return Err(std::io::Error::last_os_error()).context("read ACP runtime directory identity");
    }
    Ok(DirectoryIdentity {
        volume: info.dwVolumeSerialNumber,
        index: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
    })
}

fn is_not_found(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
}

fn reclaim(
    root: &Root,
    record: &File,
    owner: &Ownership,
    pinned: Option<File>,
    retired: bool,
) -> Result<bool> {
    if !retired && !ProcessJob::named_exit_confirmed(&owner.job_name())? {
        return Ok(false);
    }
    let path = root.path.join(&owner.instance);
    let directory = match pinned {
        Some(directory) => Some(directory),
        None => match open_directory(&path) {
            Ok(directory) => Some(directory),
            Err(error) if is_not_found(&error) => None,
            Err(error) => return Err(error),
        },
    };
    if let Some(directory) = directory {
        ensure!(
            directory_identity(&directory)? == owner.directory,
            "ACP runtime directory identity changed; refusing cleanup"
        );
        remove_contents(&path)?;
        drop(directory);
        fs::remove_dir(&path).context("remove empty Antigravity runtime directory")?;
    }
    // Delete by the locked handle, after the payload is gone. There is no
    // unlock/reopen window in which another recovery can acquire this record.
    delete_record_on_close(record)?;
    tracing::info!(target: "antigravity_runtime", instance = %owner.instance, "runtime_reclaimed");
    Ok(true)
}

fn remove_contents(path: &Path) -> Result<()> {
    let entries = fs::read_dir(path).context("enumerate owned Antigravity runtime")?;
    let mut stack = vec![(path.to_path_buf(), None, entries)];
    while let Some((_, _, entries)) = stack.last_mut() {
        let Some(entry) = entries
            .next()
            .transpose()
            .context("read owned runtime entry")?
        else {
            let (path, pin, _) = stack
                .pop()
                .context("owned runtime traversal lost its directory")?;
            if let Some(pin) = pin {
                drop(pin);
                fs::remove_dir(&path).context("remove owned runtime subdirectory")?;
            }
            continue;
        };
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).context("inspect owned runtime entry")?;
        ensure!(
            metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0,
            "owned Antigravity runtime contains a reparse point; refusing cleanup"
        );
        if metadata.is_dir() {
            let pin = open_directory(&path)?;
            let entries = fs::read_dir(&path).context("enumerate owned runtime subdirectory")?;
            stack.push((path, Some(pin), entries));
        } else {
            ensure!(
                metadata.is_file(),
                "owned runtime contains an unsupported file type"
            );
            fs::remove_file(&path).context("remove owned runtime file")?;
        }
    }
    Ok(())
}

fn delete_record_on_close(record: &File) -> Result<()> {
    let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: this exclusive ownership handle was opened with DELETE access.
    if unsafe {
        SetFileInformationByHandle(
            record.as_raw_handle(),
            FileDispositionInfo,
            std::ptr::addr_of!(disposition).cast(),
            std::mem::size_of_val(&disposition) as u32,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error()).context("retire ACP runtime ownership");
    }
    Ok(())
}

fn extraction_path(path: &Path) -> Result<PathBuf> {
    if path.as_os_str().encode_wide().count() <= MAX_TEMP_PATH_UNITS {
        return Ok(path.to_path_buf());
    }
    let input: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: both calls use a terminated input; the second owns the queried capacity.
    let capacity = unsafe { GetShortPathNameW(input.as_ptr(), std::ptr::null_mut(), 0) };
    ensure!(
        capacity > 0,
        "cannot shorten Antigravity TEMP path: {}",
        std::io::Error::last_os_error()
    );
    let mut output = vec![0u16; capacity as usize];
    let length = unsafe { GetShortPathNameW(input.as_ptr(), output.as_mut_ptr(), capacity) };
    ensure!(
        length > 0 && length < capacity,
        "cannot read shortened Antigravity TEMP path"
    );
    ensure!(
        length as usize <= MAX_TEMP_PATH_UNITS,
        "Antigravity TEMP path is too long for its extraction payload, even after shortening"
    );
    Ok(PathBuf::from(std::ffi::OsString::from_wide(
        &output[..length as usize],
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;
    use std::time::Duration;
    use tokio::io::AsyncBufReadExt;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "it-acp-{}",
                &uuid::Uuid::new_v4().simple().to_string()[..12]
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).expect("remove this test's owned fixture root");
        }
    }

    fn payload(runtime: &Runtime) -> PathBuf {
        let directory = runtime
            .root
            .path
            .join(&runtime.owner.instance)
            .join("_MEI-fixture");
        fs::create_dir(&directory).unwrap();
        let path = directory.join("payload.dll");
        fs::write(&path, "fixture").unwrap();
        path
    }

    fn record_path(runtime: &Runtime) -> PathBuf {
        runtime
            .root
            .path
            .join(format!("{}.jsonl", runtime.owner.instance))
    }

    fn junction(path: &Path, target: &Path) {
        let output = std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(path)
            .arg(target)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }

    #[test]
    fn antigravity_runtime_cleanup_parent_receipt_reclaims_only_its_retired_scope() {
        let scratch = Scratch::new();
        let old_scope = uuid::Uuid::new_v4().simple().to_string();
        let old = Runtime::create_with_scope(&scratch.0, Some(old_scope.clone())).unwrap();
        let old_file = payload(&old);
        drop(old);
        let mut active =
            Runtime::create_with_scope(&scratch.0, Some(uuid::Uuid::new_v4().simple().to_string()))
                .unwrap();
        let active_file = payload(&active);
        let receipt = scratch
            .0
            .join(format!("{RESOURCE_SCOPE_RECEIPT_PREFIX}{old_scope}"));
        fs::write(&receipt, []).unwrap();
        let report = recover_in(&scratch.0).unwrap();
        assert_eq!(report.reclaimed, 1);
        assert!(!old_file.exists());
        assert!(active_file.exists());
        assert!(
            receipt.exists(),
            "active runtime records prevent premature receipt pruning"
        );
        active.cleanup().unwrap();
        drop(active);
        recover_in(&scratch.0).unwrap();
        assert!(!receipt.exists());
    }

    #[test]
    fn antigravity_runtime_cleanup_rejects_malformed_parent_retirement() {
        let scratch = Scratch::new();
        let scope = uuid::Uuid::new_v4().simple().to_string();
        let runtime = Runtime::create_with_scope(&scratch.0, Some(scope.clone())).unwrap();
        let file = payload(&runtime);
        drop(runtime);
        let receipt = scratch
            .0
            .join(format!("{RESOURCE_SCOPE_RECEIPT_PREFIX}{scope}"));
        fs::write(&receipt, "not a retirement receipt").unwrap();
        assert_eq!(recover_in(&scratch.0).unwrap().failed, 1);
        assert!(file.exists());
        fs::write(&receipt, []).unwrap();
        assert_eq!(recover_in(&scratch.0).unwrap().reclaimed, 1);
        assert!(!file.exists());
    }

    #[test]
    fn antigravity_runtime_cleanup_retains_orphans_without_confirmed_exit() {
        let scratch = Scratch::new();
        let runtime = Runtime::create(&scratch.0).unwrap();
        let file = payload(&runtime);
        let record = record_path(&runtime);
        drop(runtime);
        recover_in(&scratch.0).unwrap();
        assert!(
            file.exists(),
            "a missing job name is not complete process-exit evidence"
        );
        assert!(
            record.exists(),
            "unconfirmed orphan ownership must remain recoverable"
        );
    }

    #[test]
    fn antigravity_runtime_cleanup_retains_locks_and_recovers_failed_deletion() {
        let scratch = Scratch::new();
        let mut runtime = Runtime::create(&scratch.0).unwrap();
        let file = payload(&runtime);
        let record = record_path(&runtime);
        let foreign = scratch.0.join("_MEI-unowned");
        fs::create_dir(&foreign).unwrap();
        fs::write(foreign.join("keep"), "user-data").unwrap();
        recover_in(&scratch.0).unwrap();
        assert!(
            file.is_file(),
            "a live ownership lease must prevent recovery"
        );

        let lock = OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&file)
            .unwrap();
        assert!(
            runtime.cleanup().is_err(),
            "a locked payload must not report successful cleanup"
        );
        drop(runtime);
        recover_in(&scratch.0).unwrap();
        assert!(
            record.is_file(),
            "failed cleanup must retain its durable ownership"
        );
        assert!(file.is_file());
        drop(lock);
        recover_in(&scratch.0).unwrap();
        assert!(!record.exists());
        assert!(!file.exists());
        assert_eq!(
            fs::read_to_string(foreign.join("keep")).unwrap(),
            "user-data"
        );
    }

    #[tokio::test]
    async fn antigravity_runtime_cleanup_pending_files_do_not_quarantine_a_dead_process() {
        let scratch = Scratch::new();
        let runtime = Runtime::create(&scratch.0).unwrap();
        let file = payload(&runtime);
        let lock = OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&file)
            .unwrap();
        let registry = crate::agent_lifecycle::StartupRegistry::default();
        let mut startup = registry.lock("host:runtime-cleanup").await.unwrap();
        startup.begin().unwrap();
        startup.succeeded();
        let mut command = tokio::process::Command::new("pwsh.exe");
        command
            .args(["-NoProfile", "-Command", "$null=[Console]::ReadLine()"])
            .stdin(Stdio::piped())
            .kill_on_drop(true);
        let mut child = crate::protocol::acp::process::AgentProcess::spawn_with_resources(
            &mut command,
            None,
            Some(Box::new(runtime)),
        )
        .unwrap();
        let error = child.terminate().await.unwrap_err();
        assert!(
            child.try_wait().unwrap().is_some(),
            "process exit must already be confirmed"
        );
        let _ = startup.failed(error);
        drop(lock);
        child.terminate().await.unwrap();
        assert!(!file.exists());
        registry
            .retry("host:runtime-cleanup")
            .await
            .expect("reclaimed files must not leave permanent process-cleanup quarantine");
        registry
            .lock("host:runtime-cleanup")
            .await
            .unwrap()
            .begin()
            .unwrap();
    }

    #[tokio::test]
    async fn antigravity_runtime_cleanup_waits_for_descendants_not_just_launcher_exit() {
        let scratch = Scratch::new();
        let runtime = Runtime::create(&scratch.0).unwrap();
        let file = payload(&runtime);
        let record = record_path(&runtime);
        let mut command = tokio::process::Command::new("pwsh.exe");
        command.args([
            "-NoLogo", "-NoProfile", "-NonInteractive", "-Command",
            "$p=Start-Process pwsh.exe -ArgumentList '-NoProfile','-NonInteractive','-Command','Start-Sleep -Seconds 120' -WindowStyle Hidden -PassThru;[Console]::WriteLine($p.Id)",
        ]).stdout(Stdio::piped()).kill_on_drop(true);
        let (mut child, job) =
            ProcessJob::spawn_named(&mut command, Some(runtime.job_name())).unwrap();
        let mut output = tokio::io::BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(20), output.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        assert!(line.trim().parse::<u32>().unwrap() > 0);
        assert!(tokio::time::timeout(Duration::from_secs(20), child.wait())
            .await
            .unwrap()
            .unwrap()
            .success());
        assert!(!ProcessJob::named_exit_confirmed(runtime.job_name()).unwrap());

        drop(runtime);
        recover_in(&scratch.0).unwrap();
        assert!(
            file.is_file(),
            "a missing launcher/lease cannot authorize deleting a live job's runtime"
        );
        job.terminate().await.unwrap();
        recover_in(&scratch.0).unwrap();
        assert!(!file.exists());
        assert!(!record.exists());
    }

    #[test]
    fn antigravity_runtime_cleanup_refuses_reparse_points_and_replaced_directories() {
        let scratch = Scratch::new();
        let foreign = scratch.0.join("foreign");
        fs::create_dir(&foreign).unwrap();
        fs::write(foreign.join("keep"), "user-data").unwrap();
        let mut runtime = Runtime::create(&scratch.0).unwrap();
        let record = record_path(&runtime);
        let directory = runtime.root.path.join(&runtime.owner.instance);
        let link = directory.join("link");
        junction(&link, &foreign);
        assert!(runtime.cleanup().is_err());
        assert!(record.is_file());
        assert!(foreign.join("keep").is_file());
        fs::remove_dir(&link).unwrap();
        runtime.cleanup().unwrap();
        drop(runtime);
        assert!(!record.exists());

        let mut runtime = Runtime::create(&scratch.0).unwrap();
        let directory = runtime.root.path.join(&runtime.owner.instance);
        let record = record_path(&runtime);
        runtime.record_retirement().unwrap();
        drop(runtime);
        fs::rename(&directory, scratch.0.join("moved-original")).unwrap();
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join("keep"), "replacement-data").unwrap();
        recover_in(&scratch.0).unwrap();
        assert!(record.is_file());
        assert_eq!(
            fs::read_to_string(directory.join("keep")).unwrap(),
            "replacement-data"
        );

        let root_link = scratch.0.join("root-link");
        junction(&root_link, &foreign);
        assert!(Runtime::create(&root_link.join("acp")).is_err());
        assert!(!foreign.join("acp").exists());
        fs::remove_dir(root_link).unwrap();
    }

    #[test]
    fn antigravity_runtime_cleanup_rejects_foreign_and_malformed_ownership() {
        let scratch = Scratch::new();
        let other = Scratch::new();
        let mut runtime = Runtime::create(&scratch.0).unwrap();
        let instance = runtime.owner.instance.clone();
        fs::write(
            other.0.join(format!("{instance}.jsonl")),
            serde_json::to_vec(&runtime.owner).unwrap(),
        )
        .unwrap();
        let foreign = other.0.join(&instance);
        fs::create_dir(&foreign).unwrap();
        fs::write(foreign.join("keep"), "user-data").unwrap();
        let malformed = other.0.join("000000000000.jsonl");
        fs::write(&malformed, "{").unwrap();
        runtime.record_retirement().unwrap();
        drop(runtime);
        recover_in(&other.0).unwrap();
        assert!(foreign.join("keep").is_file());
        assert!(other.0.join(format!("{instance}.jsonl")).is_file());
        assert!(malformed.is_file());
        recover_in(&scratch.0).unwrap();
        assert!(!scratch.0.join(&instance).exists());
    }
}
