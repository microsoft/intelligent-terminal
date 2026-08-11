use crate::agent_source::AgentSource;
use agent_client_protocol as acp;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Mutex,
};

const STORE_FILE_NAME: &str = "session-path-grants.json";
const STORE_VERSION: u32 = 1;
const MAX_STORE_BYTES: u64 = 1024 * 1024;
const MAX_RECORDS: usize = 2048;
const MAX_DIRECTORIES_PER_RECORD: usize = 128;
const MAX_PATH_CHARS: usize = 32_768;
const STORE_MUTEX_NAME: &str = "Local\\IntelligentTerminal.SessionPathGrants.v1";

static NEXT_TMP_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GrantFile {
    version: u32,
    #[serde(default)]
    records: Vec<GrantRecord>,
}

impl Default for GrantFile {
    fn default() -> Self {
        Self {
            version: STORE_VERSION,
            records: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GrantRecord {
    agent_id: String,
    environment: EnvironmentKey,
    session_id: String,
    directories: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct EnvironmentKey {
    kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    distro: Option<String>,
}

impl From<&AgentSource> for EnvironmentKey {
    fn from(source: &AgentSource) -> Self {
        Self {
            kind: source.kind().to_string(),
            distro: source.distro().map(str::to_string),
        }
    }
}

#[derive(Debug)]
struct SessionGrantStore {
    path: Option<PathBuf>,
}

impl SessionGrantStore {
    fn runtime() -> Self {
        Self {
            path: crate::runtime_paths::intelligent_terminal_root()
                .map(|root| root.join(STORE_FILE_NAME)),
        }
    }

    #[cfg(test)]
    fn at(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    fn directories(&self, agent_id: &str, source: &AgentSource, session_id: &str) -> Vec<PathBuf> {
        let Some(path) = self.path.as_deref() else {
            return Vec::new();
        };
        let file = match read_bounded(path) {
            Ok(file) => file,
            Err(error) => {
                tracing::warn!(
                    target: "path_grants",
                    path = %path.display(),
                    error = %error,
                    "ignoring malformed session path grant store"
                );
                return Vec::new();
            }
        };
        let environment = EnvironmentKey::from(source);
        file.records
            .into_iter()
            .find(|record| {
                record.agent_id == agent_id
                    && record.environment == environment
                    && record.session_id == session_id
            })
            .map(|record| record.directories.into_iter().map(PathBuf::from).collect())
            .unwrap_or_default()
    }

    fn add(
        &self,
        agent_id: &str,
        source: &AgentSource,
        session_id: &str,
        directory: &Path,
    ) -> Result<bool, String> {
        self.mutate(agent_id, source, session_id, |directories| {
            if directories
                .iter()
                .any(|existing| paths_equal(existing, directory, source))
            {
                return Ok(false);
            }
            if directories.len() >= MAX_DIRECTORIES_PER_RECORD {
                return Err(format!(
                    "a session may contain at most {MAX_DIRECTORIES_PER_RECORD} directory grants"
                ));
            }
            directories.push(directory.to_path_buf());
            Ok(true)
        })
    }

    fn remove(
        &self,
        agent_id: &str,
        source: &AgentSource,
        session_id: &str,
        directory: &Path,
    ) -> Result<bool, String> {
        self.mutate(agent_id, source, session_id, |directories| {
            let old_len = directories.len();
            directories.retain(|existing| !paths_equal(existing, directory, source));
            Ok(directories.len() != old_len)
        })
    }

    fn mutate(
        &self,
        agent_id: &str,
        source: &AgentSource,
        session_id: &str,
        mutation: impl FnOnce(&mut Vec<PathBuf>) -> Result<bool, String>,
    ) -> Result<bool, String> {
        let path = self
            .path
            .as_deref()
            .ok_or_else(|| "the Intelligent Terminal state directory is unavailable".to_string())?;
        let _guard = NamedMutexGuard::acquire()?;
        let mut file = if path.exists() {
            read_bounded(path)?
        } else {
            GrantFile::default()
        };
        let environment = EnvironmentKey::from(source);
        let record_index = file.records.iter().position(|record| {
            record.agent_id == agent_id
                && record.environment == environment
                && record.session_id == session_id
        });
        let mut directories = record_index
            .map(|index| {
                file.records[index]
                    .directories
                    .iter()
                    .map(PathBuf::from)
                    .collect()
            })
            .unwrap_or_default();
        let changed = mutation(&mut directories)?;
        if !changed {
            return Ok(false);
        }

        let serialized_directories = directories
            .iter()
            .map(|directory| validate_path(directory))
            .collect::<Result<Vec<_>, _>>()?;
        if let Some(index) = record_index {
            if serialized_directories.is_empty() {
                file.records.remove(index);
            } else {
                file.records[index].directories = serialized_directories;
            }
        } else if !serialized_directories.is_empty() {
            if file.records.len() >= MAX_RECORDS {
                return Err(format!(
                    "the session path grant store may contain at most {MAX_RECORDS} records"
                ));
            }
            file.records.push(GrantRecord {
                agent_id: agent_id.to_string(),
                environment,
                session_id: session_id.to_string(),
                directories: serialized_directories,
            });
        }
        write_atomic(path, &file)?;
        Ok(true)
    }
}

fn read_bounded(path: &Path) -> Result<GrantFile, String> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(GrantFile::default())
        }
        Err(error) => return Err(format!("failed to inspect store: {error}")),
    };
    if metadata.len() > MAX_STORE_BYTES {
        return Err(format!("store exceeds {MAX_STORE_BYTES} bytes"));
    }
    let bytes = fs::read(path).map_err(|error| format!("failed to read store: {error}"))?;
    let file: GrantFile =
        serde_json::from_slice(&bytes).map_err(|error| format!("invalid JSON: {error}"))?;
    validate_file(&file)?;
    Ok(file)
}

fn validate_file(file: &GrantFile) -> Result<(), String> {
    if file.version != STORE_VERSION {
        return Err(format!("unsupported store version {}", file.version));
    }
    if file.records.len() > MAX_RECORDS {
        return Err(format!("store contains more than {MAX_RECORDS} records"));
    }
    let mut record_keys = HashSet::new();
    for record in &file.records {
        if record.agent_id.trim().is_empty() || record.session_id.trim().is_empty() {
            return Err("store contains an empty record key".to_string());
        }
        if record.environment.kind != AgentSource::HOST_KIND
            && record.environment.kind != AgentSource::WSL_KIND
        {
            return Err("store contains an unknown execution environment".to_string());
        }
        if record.environment.kind == AgentSource::WSL_KIND
            && record
                .environment
                .distro
                .as_deref()
                .is_none_or(|distro| distro.trim().is_empty())
        {
            return Err("WSL grant record is missing its distro".to_string());
        }
        if !record_keys.insert((
            record.agent_id.as_str(),
            record.environment.kind.as_str(),
            record.environment.distro.as_deref(),
            record.session_id.as_str(),
        )) {
            return Err("store contains duplicate grant records".to_string());
        }
        if record.directories.len() > MAX_DIRECTORIES_PER_RECORD {
            return Err(format!(
                "record contains more than {MAX_DIRECTORIES_PER_RECORD} directories"
            ));
        }
        let source = if record.environment.kind == AgentSource::HOST_KIND {
            AgentSource::Host
        } else {
            AgentSource::Wsl {
                distro: record.environment.distro.clone().unwrap_or_default(),
            }
        };
        let mut directories: Vec<PathBuf> = Vec::new();
        for directory in &record.directories {
            if directory.chars().count() > MAX_PATH_CHARS {
                return Err(format!("path exceeds {MAX_PATH_CHARS} characters"));
            }
            let path = PathBuf::from(directory);
            if !is_absolute_for_source(&path, &source) {
                return Err("store contains a non-absolute directory".to_string());
            }
            if directories
                .iter()
                .any(|existing| paths_equal(existing, &path, &source))
            {
                return Err("store contains duplicate directories".to_string());
            }
            directories.push(path);
        }
    }
    Ok(())
}

fn validate_path(path: &Path) -> Result<String, String> {
    let value = path.to_string_lossy().into_owned();
    if value.chars().count() > MAX_PATH_CHARS {
        return Err(format!("path exceeds {MAX_PATH_CHARS} characters"));
    }
    Ok(value)
}

fn write_atomic(path: &Path, file: &GrantFile) -> Result<(), String> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)
        .map_err(|error| format!("failed to create state directory: {error}"))?;
    let bytes = serde_json::to_vec_pretty(file)
        .map_err(|error| format!("failed to encode store: {error}"))?;
    if bytes.len() as u64 > MAX_STORE_BYTES {
        return Err(format!("updated store exceeds {MAX_STORE_BYTES} bytes"));
    }
    let unique = NEXT_TMP_ID.fetch_add(1, Ordering::Relaxed);
    let temp = parent.join(format!(
        ".{STORE_FILE_NAME}.{}.{}.tmp",
        std::process::id(),
        unique
    ));
    fs::write(&temp, bytes).map_err(|error| format!("failed to stage store: {error}"))?;
    match fs::rename(&temp, path) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = fs::remove_file(&temp);
            Err(format!("failed to replace store: {error}"))
        }
    }
}

struct NamedMutexGuard(windows_sys::Win32::Foundation::HANDLE);

impl NamedMutexGuard {
    fn acquire() -> Result<Self, String> {
        use windows_sys::Win32::Foundation::{WAIT_ABANDONED, WAIT_OBJECT_0};
        use windows_sys::Win32::System::Threading::{CreateMutexW, WaitForSingleObject, INFINITE};

        let name: Vec<u16> = STORE_MUTEX_NAME.encode_utf16().chain(Some(0)).collect();
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        if handle.is_null() {
            return Err(format!(
                "failed to create store mutex: {}",
                std::io::Error::last_os_error()
            ));
        }
        let wait = unsafe { WaitForSingleObject(handle, INFINITE) };
        if wait != WAIT_OBJECT_0 && wait != WAIT_ABANDONED {
            unsafe {
                windows_sys::Win32::Foundation::CloseHandle(handle);
            }
            return Err(format!(
                "failed to acquire store mutex (wait result {wait}, last error {})",
                std::io::Error::last_os_error()
            ));
        }
        Ok(Self(handle))
    }
}

impl Drop for NamedMutexGuard {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::System::Threading::ReleaseMutex(self.0);
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

pub(crate) struct SessionRoots {
    agent_id: String,
    source: AgentSource,
    configured: Mutex<Vec<PathBuf>>,
    store: SessionGrantStore,
    additional_directories_supported: AtomicBool,
    primary_by_session: Mutex<HashMap<String, PathBuf>>,
    effective_by_session: Mutex<HashMap<String, Vec<PathBuf>>>,
}

impl SessionRoots {
    pub(crate) fn new(agent_id: String, source: AgentSource, configured: Vec<PathBuf>) -> Self {
        Self::with_store(agent_id, source, configured, SessionGrantStore::runtime())
    }

    fn with_store(
        agent_id: String,
        source: AgentSource,
        configured: Vec<PathBuf>,
        store: SessionGrantStore,
    ) -> Self {
        Self {
            agent_id,
            configured: Mutex::new(normalize_additional_directories(configured, &source)),
            source,
            store,
            additional_directories_supported: AtomicBool::new(false),
            primary_by_session: Mutex::new(HashMap::new()),
            effective_by_session: Mutex::new(HashMap::new()),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        agent_id: String,
        source: AgentSource,
        configured: Vec<PathBuf>,
        store_path: PathBuf,
    ) -> Self {
        Self::with_store(
            agent_id,
            source,
            configured,
            SessionGrantStore::at(store_path),
        )
    }

    pub(crate) fn set_additional_directories_supported(&self, supported: bool) {
        self.additional_directories_supported
            .store(supported, Ordering::Relaxed);
    }

    pub(crate) fn new_request(&self, cwd: PathBuf) -> acp::schema::v1::NewSessionRequest {
        let mut request = acp::schema::v1::NewSessionRequest::new(cwd.clone());
        if self
            .additional_directories_supported
            .load(Ordering::Relaxed)
        {
            request.additional_directories = self.configured_for_cwd(&cwd);
        }
        request
    }

    pub(crate) fn load_request(
        &self,
        session_id: acp::schema::v1::SessionId,
        cwd: PathBuf,
    ) -> acp::schema::v1::LoadSessionRequest {
        let mut request = acp::schema::v1::LoadSessionRequest::new(session_id.clone(), cwd.clone());
        if self
            .additional_directories_supported
            .load(Ordering::Relaxed)
        {
            request.additional_directories =
                self.additional_for_session(&session_id.to_string(), &cwd);
        }
        request
    }

    pub(crate) fn remember(&self, session_id: &acp::schema::v1::SessionId, cwd: &Path) {
        self.primary_by_session
            .lock()
            .unwrap()
            .insert(session_id.to_string(), cwd.to_path_buf());
        let mut roots = vec![cwd.to_path_buf()];
        roots.extend(self.additional_for_session(&session_id.to_string(), cwd));
        self.effective_by_session
            .lock()
            .unwrap()
            .insert(session_id.to_string(), roots);
    }

    pub(crate) fn forget(&self, session_id: &acp::schema::v1::SessionId) {
        self.primary_by_session
            .lock()
            .unwrap()
            .remove(&session_id.to_string());
        self.effective_by_session
            .lock()
            .unwrap()
            .remove(&session_id.to_string());
    }

    pub(crate) fn effective(&self, session_id: &str) -> Vec<PathBuf> {
        self.effective_by_session
            .lock()
            .unwrap()
            .get(session_id)
            .cloned()
            .unwrap_or_default()
    }

    pub(crate) fn add_session_directory(
        &self,
        session_id: &str,
        directory: PathBuf,
    ) -> Result<bool, String> {
        if !is_absolute_for_source(&directory, &self.source) {
            return Err("directory must be absolute for the agent environment".to_string());
        }
        if self
            .effective_by_session
            .lock()
            .unwrap()
            .get(session_id)
            .is_some_and(|roots| {
                roots
                    .iter()
                    .any(|existing| paths_equal(existing, &directory, &self.source))
            })
            || self
                .configured
                .lock()
                .unwrap()
                .iter()
                .any(|existing| paths_equal(existing, &directory, &self.source))
        {
            return Ok(false);
        }
        let changed = self
            .store
            .add(&self.agent_id, &self.source, session_id, &directory)?;
        if changed {
            let mut effective = self.effective_by_session.lock().unwrap();
            let roots = effective.entry(session_id.to_string()).or_default();
            if !roots
                .iter()
                .any(|existing| paths_equal(existing, &directory, &self.source))
            {
                roots.push(directory);
            }
        }
        Ok(changed)
    }

    pub(crate) fn remove_session_directory(
        &self,
        session_id: &str,
        directory: &Path,
    ) -> Result<bool, String> {
        let changed = self
            .store
            .remove(&self.agent_id, &self.source, session_id, directory)?;
        if changed {
            if let Some(cwd) = self
                .primary_by_session
                .lock()
                .unwrap()
                .get(session_id)
                .cloned()
            {
                let mut roots = vec![cwd.clone()];
                roots.extend(self.additional_for_session(session_id, &cwd));
                self.effective_by_session
                    .lock()
                    .unwrap()
                    .insert(session_id.to_string(), roots);
            } else if let Some(roots) = self
                .effective_by_session
                .lock()
                .unwrap()
                .get_mut(session_id)
            {
                roots.retain(|existing| !paths_equal(existing, directory, &self.source));
                for configured in self.configured.lock().unwrap().iter() {
                    if paths_equal(configured, directory, &self.source)
                        && !roots
                            .iter()
                            .any(|existing| paths_equal(existing, configured, &self.source))
                    {
                        roots.push(configured.clone());
                    }
                }
            }
        }
        Ok(changed)
    }

    pub(crate) fn session_directories(&self, session_id: &str) -> Vec<PathBuf> {
        normalize_additional_directories(
            self.store
                .directories(&self.agent_id, &self.source, session_id),
            &self.source,
        )
    }

    pub(crate) fn configured_directories(&self) -> Vec<PathBuf> {
        self.configured.lock().unwrap().clone()
    }

    pub(crate) fn replace_configured_directories(&self, paths: Vec<PathBuf>) {
        *self.configured.lock().unwrap() = normalize_additional_directories(paths, &self.source);
        let sessions = self.primary_by_session.lock().unwrap().clone();
        for (session_id, cwd) in sessions {
            let mut roots = vec![cwd.clone()];
            roots.extend(self.additional_for_session(&session_id, &cwd));
            self.effective_by_session
                .lock()
                .unwrap()
                .insert(session_id, roots);
        }
    }

    pub(crate) fn is_absolute_directory(&self, path: &Path) -> bool {
        is_absolute_for_source(path, &self.source)
    }

    pub(crate) fn additional_directories_supported(&self) -> bool {
        self.additional_directories_supported
            .load(Ordering::Relaxed)
    }

    pub(crate) fn source(&self) -> &AgentSource {
        &self.source
    }

    fn additional_for_session(&self, session_id: &str, cwd: &Path) -> Vec<PathBuf> {
        let mut roots = self.configured.lock().unwrap().clone();
        roots.extend(
            self.store
                .directories(&self.agent_id, &self.source, session_id),
        );
        normalize_additional_directories(roots, &self.source)
            .into_iter()
            .filter(|path| !paths_equal(path, cwd, &self.source))
            .collect()
    }

    fn configured_for_cwd(&self, cwd: &Path) -> Vec<PathBuf> {
        self.configured
            .lock()
            .unwrap()
            .iter()
            .filter(|path| !paths_equal(path, cwd, &self.source))
            .cloned()
            .collect()
    }
}

fn normalize_additional_directories(paths: Vec<PathBuf>, source: &AgentSource) -> Vec<PathBuf> {
    let mut normalized: Vec<PathBuf> = Vec::new();
    for path in paths {
        if !is_absolute_for_source(&path, source) {
            tracing::warn!(
                target: "path_grants",
                "ignoring non-absolute additional directory"
            );
            continue;
        }
        if normalized
            .iter()
            .any(|existing| paths_equal(existing, &path, source))
        {
            continue;
        }
        normalized.push(path);
    }
    normalized
}

fn is_absolute_for_source(path: &Path, source: &AgentSource) -> bool {
    match source {
        AgentSource::Host => path.is_absolute(),
        AgentSource::Wsl { .. } => path.to_string_lossy().starts_with('/'),
    }
}

fn paths_equal(left: &Path, right: &Path, source: &AgentSource) -> bool {
    match source {
        AgentSource::Host => left
            .to_string_lossy()
            .trim_end_matches(['\\', '/'])
            .eq_ignore_ascii_case(right.to_string_lossy().trim_end_matches(['\\', '/'])),
        AgentSource::Wsl { .. } => left == right,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "wta-path-grants-{name}-{}-{}.json",
            std::process::id(),
            NEXT_TMP_ID.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn roots_at(path: PathBuf) -> SessionRoots {
        SessionRoots::with_store(
            "copilot".to_string(),
            AgentSource::Host,
            vec![PathBuf::from(r"C:\global")],
            SessionGrantStore::at(path),
        )
    }

    #[test]
    fn persisted_session_roots_are_replayed_on_load() {
        let path = temp_store("replay");
        let roots = roots_at(path.clone());
        roots
            .add_session_directory("session-1", PathBuf::from(r"D:\session"))
            .unwrap();
        roots.set_additional_directories_supported(true);

        let request = roots.load_request(
            acp::schema::v1::SessionId::new("session-1"),
            PathBuf::from(r"C:\work"),
        );

        assert_eq!(
            request.additional_directories,
            vec![PathBuf::from(r"C:\global"), PathBuf::from(r"D:\session")]
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn grants_are_isolated_by_agent_environment_and_session() {
        let path = temp_store("isolation");
        let copilot = roots_at(path.clone());
        copilot
            .add_session_directory("session-1", PathBuf::from(r"D:\copilot"))
            .unwrap();

        let claude = SessionRoots::with_store(
            "claude".to_string(),
            AgentSource::Host,
            Vec::new(),
            SessionGrantStore::at(path.clone()),
        );
        let wsl = SessionRoots::with_store(
            "copilot".to_string(),
            AgentSource::Wsl {
                distro: "Ubuntu".to_string(),
            },
            Vec::new(),
            SessionGrantStore::at(path.clone()),
        );

        assert!(claude.session_directories("session-1").is_empty());
        assert!(wsl.session_directories("session-1").is_empty());
        assert!(copilot.session_directories("session-2").is_empty());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn malformed_store_fails_closed() {
        let path = temp_store("malformed");
        fs::write(&path, br#"{"version":1,"records":"not-an-array"}"#).unwrap();
        let roots = roots_at(path.clone());

        assert!(roots.session_directories("session-1").is_empty());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn remove_deletes_empty_record() {
        let path = temp_store("remove");
        let roots = roots_at(path.clone());
        let directory = PathBuf::from(r"D:\session");
        roots
            .add_session_directory("session-1", directory.clone())
            .unwrap();

        assert!(roots
            .remove_session_directory("session-1", &directory)
            .unwrap());
        assert!(roots.session_directories("session-1").is_empty());
        let file = read_bounded(&path).unwrap();
        assert!(file.records.is_empty());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn existing_primary_and_global_roots_are_not_stored_as_session_grants() {
        let path = temp_store("existing-roots");
        let roots = roots_at(path.clone());
        let session_id = acp::schema::v1::SessionId::new("session-1");
        roots.remember(&session_id, Path::new(r"C:\work"));

        assert!(!roots
            .add_session_directory("session-1", PathBuf::from(r"C:\work"))
            .unwrap());
        assert!(!roots
            .add_session_directory("session-1", PathBuf::from(r"C:\global"))
            .unwrap());
        assert!(!path.exists());
    }
}
