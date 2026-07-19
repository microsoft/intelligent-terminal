//! Master-owned durable shell-session metadata and scrollback storage.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use uuid::Uuid;

const DATABASE_FILE: &str = "shell-sessions.db";
const BUFFER_DIRECTORY: &str = "shell-sessions";
const STAGING_DIRECTORY: &str = "shell-session-staging";
const RESTORE_CACHE_DIRECTORY: &str = "shell-session-restore-cache";
const RETENTION_SECONDS: i64 = 15 * 24 * 60 * 60;
const TRANSIENT_RETENTION_SECONDS: i64 = 24 * 60 * 60;
const MAINTENANCE_INTERVAL_SECONDS: i64 = 60 * 60;
const LEGACY_BUFFER_PREFIXES: [&str; 2] = ["shell_buffer_", "shell_elevated_"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShellSessionRecord {
    pub id: String,
    pub name: String,
    pub layout_json: String,
    pub elevated: bool,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_used_at: i64,
    pub revision: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShellSessionBufferInput {
    pub pane_key: String,
    pub staging_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShellSessionBuffer {
    pub pane_key: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShellSessionsListParams {
    pub elevated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShellSessionsListResponse {
    pub sessions: Vec<ShellSessionRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShellSessionSaveParams {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub expected_revision: Option<i64>,
    pub name: String,
    pub layout_json: String,
    pub elevated: bool,
    pub buffers: Vec<ShellSessionBufferInput>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShellSessionSaveResponse {
    pub id: String,
    pub revision: i64,
    pub forked: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShellSessionGetParams {
    pub id: String,
    pub elevated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShellSessionGetResponse {
    pub session: ShellSessionRecord,
    pub buffers: Vec<ShellSessionBuffer>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShellSessionDeleteParams {
    pub id: String,
    pub elevated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShellSessionDeleteResponse {
    pub deleted: bool,
}

/// Cloneable async facade over the dedicated SQLite/file actor thread.
#[derive(Clone)]
pub struct ShellSessionStore {
    tx: mpsc::Sender<StoreCommand>,
}

enum StoreCommand {
    List(
        ShellSessionsListParams,
        oneshot::Sender<Result<ShellSessionsListResponse>>,
    ),
    Save(
        ShellSessionSaveParams,
        oneshot::Sender<Result<ShellSessionSaveResponse>>,
    ),
    Get(
        ShellSessionGetParams,
        oneshot::Sender<Result<Option<ShellSessionGetResponse>>>,
    ),
    Delete(
        ShellSessionDeleteParams,
        oneshot::Sender<Result<ShellSessionDeleteResponse>>,
    ),
}

impl ShellSessionStore {
    pub async fn open_runtime() -> Result<Self> {
        let root = crate::runtime_paths::shell_session_runtime_root()
            .ok_or_else(|| anyhow!("shell-session state root is unavailable"))?;
        Self::open_actor(root).await
    }

    async fn open_actor(root: PathBuf) -> Result<Self> {
        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = oneshot::channel();
        std::thread::Builder::new()
            .name("wta-shell-session-store".to_string())
            .spawn(move || {
                let legacy_settings_directory =
                    crate::runtime_paths::shell_session_settings_directory();
                if legacy_settings_directory.is_none() {
                    tracing::debug!(
                        target: "shell_sessions",
                        "Cascadia SettingsDirectory is not safely resolvable; skipping legacy buffer cleanup"
                    );
                }
                let store = StoreCore::open(root, unix_now(), legacy_settings_directory.as_deref());
                match store {
                    Ok(mut store) => {
                        let _ = ready_tx.send(Ok(()));
                        while let Ok(command) = rx.recv() {
                            store.handle(command);
                        }
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                    }
                }
            })
            .context("failed to spawn shell-session store actor")?;

        ready_rx
            .await
            .context("shell-session store actor exited during startup")??;
        Ok(Self { tx })
    }

    pub async fn list(&self, params: ShellSessionsListParams) -> Result<ShellSessionsListResponse> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(StoreCommand::List(params, tx))
            .map_err(|_| anyhow!("shell-session store actor is unavailable"))?;
        rx.await
            .context("shell-session store actor dropped list response")?
    }

    pub async fn save(&self, params: ShellSessionSaveParams) -> Result<ShellSessionSaveResponse> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(StoreCommand::Save(params, tx))
            .map_err(|_| anyhow!("shell-session store actor is unavailable"))?;
        rx.await
            .context("shell-session store actor dropped save response")?
    }

    pub async fn get(
        &self,
        params: ShellSessionGetParams,
    ) -> Result<Option<ShellSessionGetResponse>> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(StoreCommand::Get(params, tx))
            .map_err(|_| anyhow!("shell-session store actor is unavailable"))?;
        rx.await
            .context("shell-session store actor dropped get response")?
    }

    pub async fn delete(
        &self,
        params: ShellSessionDeleteParams,
    ) -> Result<ShellSessionDeleteResponse> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(StoreCommand::Delete(params, tx))
            .map_err(|_| anyhow!("shell-session store actor is unavailable"))?;
        rx.await
            .context("shell-session store actor dropped delete response")?
    }
}

struct StoreCore {
    connection: Connection,
    buffer_root: PathBuf,
    staging_root: PathBuf,
    restore_root: PathBuf,
    last_durable_maintenance_at: i64,
    last_transient_maintenance_at: i64,
}

impl StoreCore {
    fn open(root: PathBuf, now: i64, legacy_settings_directory: Option<&Path>) -> Result<Self> {
        if let Some(directory) = legacy_settings_directory {
            match cleanup_legacy_one_shot_buffers(directory) {
                Ok(removed) if removed > 0 => tracing::info!(
                    target: "shell_sessions",
                    removed,
                    directory = %directory.display(),
                    "removed legacy one-shot shell-session buffers"
                ),
                Ok(_) => {}
                Err(error) => tracing::warn!(
                    target: "shell_sessions",
                    directory = %directory.display(),
                    error = %error,
                    "failed to clean legacy one-shot shell-session buffers"
                ),
            }
        }
        fs::create_dir_all(&root)
            .with_context(|| format!("failed to create state root {}", root.display()))?;
        let buffer_root = root.join(BUFFER_DIRECTORY);
        let staging_root = root.join(STAGING_DIRECTORY);
        let restore_root = root.join(RESTORE_CACHE_DIRECTORY);
        fs::create_dir_all(&buffer_root).with_context(|| {
            format!(
                "failed to create shell-session buffer root {}",
                buffer_root.display()
            )
        })?;
        fs::create_dir_all(&staging_root).with_context(|| {
            format!(
                "failed to create shell-session staging root {}",
                staging_root.display()
            )
        })?;
        fs::create_dir_all(&restore_root).with_context(|| {
            format!(
                "failed to create shell-session restore cache {}",
                restore_root.display()
            )
        })?;
        let buffer_root = fs::canonicalize(&buffer_root).with_context(|| {
            format!(
                "failed to canonicalize shell-session buffer root {}",
                buffer_root.display()
            )
        })?;

        let connection = Connection::open(root.join(DATABASE_FILE))
            .context("failed to open shell-session database")?;
        connection
            .busy_timeout(std::time::Duration::from_secs(5))
            .context("failed to set shell-session database busy timeout")?;
        connection
            .execute_batch(
                "
                PRAGMA foreign_keys = ON;
                PRAGMA journal_mode = WAL;
                CREATE TABLE IF NOT EXISTS shell_sessions (
                    id            TEXT PRIMARY KEY NOT NULL,
                    name          TEXT NOT NULL,
                    layout_json   TEXT NOT NULL,
                    elevated      INTEGER NOT NULL CHECK (elevated IN (0, 1)),
                    created_at    INTEGER NOT NULL,
                    updated_at    INTEGER NOT NULL,
                    last_used_at  INTEGER NOT NULL,
                    revision      INTEGER NOT NULL CHECK (revision > 0)
                );
                CREATE TABLE IF NOT EXISTS shell_session_buffers (
                    buffer_id  TEXT PRIMARY KEY NOT NULL,
                    session_id TEXT NOT NULL
                        REFERENCES shell_sessions(id) ON DELETE CASCADE,
                    pane_key   TEXT NOT NULL,
                    path       TEXT NOT NULL,
                    UNIQUE(session_id, pane_key)
                );
                CREATE INDEX IF NOT EXISTS shell_sessions_last_used_idx
                    ON shell_sessions(last_used_at DESC);
                ",
            )
            .context("failed to initialize shell-session database")?;

        let mut store = Self {
            connection,
            buffer_root,
            staging_root,
            restore_root,
            last_durable_maintenance_at: now,
            last_transient_maintenance_at: now,
        };
        store.run_startup_maintenance(now);
        Ok(store)
    }

    fn handle(&mut self, command: StoreCommand) {
        let now = unix_now();
        self.run_durable_maintenance_if_due(now);
        self.run_transient_maintenance_if_due(now);
        match command {
            StoreCommand::List(params, response) => {
                let _ = response.send(self.list(&params, now));
            }
            StoreCommand::Save(params, response) => {
                let _ = response.send(self.save(params, now));
            }
            StoreCommand::Get(params, response) => {
                let _ = response.send(self.get(&params, now));
            }
            StoreCommand::Delete(params, response) => {
                let _ = response.send(self.delete(&params));
            }
        }
    }

    fn list(
        &self,
        params: &ShellSessionsListParams,
        now: i64,
    ) -> Result<ShellSessionsListResponse> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT id, name, layout_json, elevated, created_at, updated_at,
                        last_used_at, revision
                   FROM shell_sessions
                  WHERE elevated = ?1 AND last_used_at >= ?2
                  ORDER BY last_used_at DESC, updated_at DESC, id ASC",
            )
            .context("failed to prepare shell-session list query")?;
        let sessions = statement
            .query_map(
                params![params.elevated, now - RETENTION_SECONDS],
                record_from_row,
            )
            .context("failed to query shell sessions")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to decode shell-session rows")?;
        Ok(ShellSessionsListResponse { sessions })
    }

    fn save(
        &mut self,
        params: ShellSessionSaveParams,
        now: i64,
    ) -> Result<ShellSessionSaveResponse> {
        validate_save_params(&params)?;
        let staging_files = self.validate_staging_files(&params.buffers)?;
        let buffer_root = self.buffer_root.clone();
        let mut new_buffers: Vec<(String, String, PathBuf)> =
            Vec::with_capacity(params.buffers.len());
        let transaction_result = (|| -> Result<(ShellSessionSaveResponse, Vec<PathBuf>)> {
            let transaction = self
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .context("failed to begin shell-session save transaction")?;
            let existing_revision = match params.id.as_deref() {
                Some(id) => transaction
                    .query_row(
                        "SELECT revision
                           FROM shell_sessions
                          WHERE id = ?1 AND elevated = ?2 AND last_used_at >= ?3",
                        params![id, params.elevated, now - RETENTION_SECONDS],
                        |row| row.get::<_, i64>(0),
                    )
                    .optional()
                    .context("failed to inspect existing shell session")?,
                None => None,
            };
            let updates_existing = matches!(
                (existing_revision, params.expected_revision),
                (Some(actual), Some(expected)) if actual == expected
            );
            let forked = params.id.is_some() && !updates_existing;
            let id = if updates_existing {
                params.id.clone().context("existing update had no id")?
            } else {
                Uuid::new_v4().to_string()
            };
            let revision = existing_revision
                .filter(|_| updates_existing)
                .map_or(1, |value| value + 1);
            let old_paths = if updates_existing {
                Self::buffer_paths_for_session(&transaction, &buffer_root, &id)?
            } else {
                Vec::new()
            };

            let session_directory = buffer_root.join(&id);
            fs::create_dir_all(&session_directory).with_context(|| {
                format!(
                    "failed to create shell-session directory {}",
                    session_directory.display()
                )
            })?;
            let session_directory = fs::canonicalize(&session_directory).with_context(|| {
                format!(
                    "failed to canonicalize shell-session directory {}",
                    session_directory.display()
                )
            })?;
            if !session_directory.starts_with(&buffer_root) {
                return Err(anyhow!(
                    "shell-session directory escapes buffer root: {}",
                    session_directory.display()
                ));
            }

            for (buffer, staging_path) in params.buffers.iter().zip(staging_files.iter()) {
                let buffer_id = Uuid::new_v4().to_string();
                let destination = session_directory.join(format!("{buffer_id}.buffer"));
                place_file_atomically(staging_path, &destination)?;
                new_buffers.push((buffer_id, buffer.pane_key.clone(), destination));
            }

            if updates_existing {
                let changed = transaction
                    .execute(
                        "UPDATE shell_sessions
                            SET name = ?1, layout_json = ?2, updated_at = ?3,
                                last_used_at = ?3, revision = ?4
                          WHERE id = ?5 AND elevated = ?6 AND revision = ?7",
                        params![
                            params.name,
                            params.layout_json,
                            now,
                            revision,
                            id,
                            params.elevated,
                            revision - 1
                        ],
                    )
                    .context("failed to update shell session")?;
                if changed != 1 {
                    return Err(anyhow!(
                        "shell-session revision changed during serialized update"
                    ));
                }
                transaction
                    .execute(
                        "DELETE FROM shell_session_buffers WHERE session_id = ?1",
                        params![id],
                    )
                    .context("failed to replace shell-session buffers")?;
            } else {
                transaction
                    .execute(
                        "INSERT INTO shell_sessions
                            (id, name, layout_json, elevated, created_at, updated_at,
                             last_used_at, revision)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?5, ?6)",
                        params![
                            id,
                            params.name,
                            params.layout_json,
                            params.elevated,
                            now,
                            revision
                        ],
                    )
                    .context("failed to insert shell session")?;
            }
            for (buffer_id, pane_key, path) in &new_buffers {
                transaction
                    .execute(
                        "INSERT INTO shell_session_buffers
                            (buffer_id, session_id, pane_key, path)
                         VALUES (?1, ?2, ?3, ?4)",
                        params![buffer_id, id, pane_key, path.to_string_lossy()],
                    )
                    .context("failed to insert shell-session buffer")?;
            }
            transaction
                .commit()
                .context("failed to commit shell-session save")?;
            Ok((
                ShellSessionSaveResponse {
                    id,
                    revision,
                    forked,
                },
                old_paths,
            ))
        })();

        match transaction_result {
            Ok((response, old_paths)) => {
                remove_files(old_paths.iter());
                remove_empty_parent_directories(&buffer_root, &old_paths);
                Ok(response)
            }
            Err(error) => {
                remove_files(new_buffers.iter().map(|(_, _, path)| path));
                Err(error)
            }
        }
    }

    fn get(
        &mut self,
        params: &ShellSessionGetParams,
        now: i64,
    ) -> Result<Option<ShellSessionGetResponse>> {
        validate_durable_id(&params.id)?;
        let buffer_root = self.buffer_root.clone();
        let restore_root = self.restore_root.clone();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("failed to begin shell-session get transaction")?;
        let mut session = transaction
            .query_row(
                "SELECT id, name, layout_json, elevated, created_at, updated_at,
                        last_used_at, revision
                   FROM shell_sessions
                  WHERE id = ?1 AND elevated = ?2 AND last_used_at >= ?3",
                params![params.id, params.elevated, now - RETENTION_SECONDS],
                record_from_row,
            )
            .optional()
            .context("failed to query shell session")?;
        let Some(session) = session.as_mut() else {
            return Ok(None);
        };

        let buffer_rows = {
            let mut statement = transaction
                .prepare(
                    "SELECT pane_key, session_id, buffer_id
                       FROM shell_session_buffers
                      WHERE session_id = ?1
                      ORDER BY pane_key ASC",
                )
                .context("failed to prepare shell-session buffer query")?;
            let buffers = statement
                .query_map(params![params.id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })
                .context("failed to query shell-session buffers")?
                .collect::<rusqlite::Result<Vec<_>>>()
                .context("failed to decode shell-session buffers")?;
            buffers
        };
        let source_buffers = buffer_rows
            .into_iter()
            .map(|(pane_key, session_id, buffer_id)| {
                Ok(ShellSessionBuffer {
                    pane_key,
                    path: Self::resolve_existing_buffer_path(
                        &buffer_root,
                        &session_id,
                        &buffer_id,
                    )?,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let (snapshot_directory, buffers) =
            Self::create_restore_snapshot(&restore_root, &source_buffers, now)?;
        let commit_result = (|| -> Result<()> {
            let changed = transaction
                .execute(
                    "UPDATE shell_sessions
                        SET last_used_at = ?1
                      WHERE id = ?2 AND elevated = ?3 AND last_used_at >= ?4",
                    params![now, params.id, params.elevated, now - RETENTION_SECONDS],
                )
                .context("failed to mark shell-session restore access")?;
            if changed != 1 {
                return Err(anyhow!(
                    "shell session disappeared before restore access was recorded"
                ));
            }
            transaction
                .commit()
                .context("failed to commit shell-session restore access")
        })();
        if let Err(error) = commit_result {
            let _ = fs::remove_dir_all(snapshot_directory);
            return Err(error);
        }

        session.last_used_at = now;
        Ok(Some(ShellSessionGetResponse {
            session: session.clone(),
            buffers,
        }))
    }

    fn create_restore_snapshot(
        restore_root: &Path,
        source_buffers: &[ShellSessionBuffer],
        now: i64,
    ) -> Result<(PathBuf, Vec<ShellSessionBuffer>)> {
        let snapshot_directory = restore_root.join(format!("{now}-{}", Uuid::new_v4()));
        fs::create_dir(&snapshot_directory).with_context(|| {
            format!(
                "failed to create shell-session restore snapshot {}",
                snapshot_directory.display()
            )
        })?;

        let snapshot_result = source_buffers
            .iter()
            .map(|source| {
                let destination = snapshot_directory.join(format!("{}.buffer", Uuid::new_v4()));
                snapshot_file(&source.path, &destination)?;
                Ok(ShellSessionBuffer {
                    pane_key: source.pane_key.clone(),
                    path: destination,
                })
            })
            .collect::<Result<Vec<_>>>();
        match snapshot_result {
            Ok(buffers) => Ok((snapshot_directory, buffers)),
            Err(error) => {
                let _ = fs::remove_dir_all(&snapshot_directory);
                Err(error)
            }
        }
    }

    fn delete(&mut self, params: &ShellSessionDeleteParams) -> Result<ShellSessionDeleteResponse> {
        validate_durable_id(&params.id)?;
        let buffer_root = self.buffer_root.clone();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("failed to begin shell-session delete transaction")?;
        let paths = Self::buffer_paths_for_scoped_session(
            &transaction,
            &buffer_root,
            &params.id,
            params.elevated,
        )?;
        let deleted = transaction
            .execute(
                "DELETE FROM shell_sessions WHERE id = ?1 AND elevated = ?2",
                params![params.id, params.elevated],
            )
            .context("failed to delete shell session")?
            != 0;
        transaction
            .commit()
            .context("failed to commit shell-session delete")?;
        if deleted {
            remove_files(paths.iter());
            remove_empty_parent_directories(&buffer_root, &paths);
        }
        Ok(ShellSessionDeleteResponse { deleted })
    }

    fn validate_staging_files(&self, buffers: &[ShellSessionBufferInput]) -> Result<Vec<PathBuf>> {
        let staging_root = fs::canonicalize(&self.staging_root).with_context(|| {
            format!(
                "failed to canonicalize shell-session staging root {}",
                self.staging_root.display()
            )
        })?;
        let mut pane_keys = HashSet::new();
        let mut paths = HashSet::new();
        buffers
            .iter()
            .map(|buffer| {
                if buffer.pane_key.trim().is_empty() {
                    return Err(anyhow!("shell-session pane_key must not be empty"));
                }
                if !pane_keys.insert(buffer.pane_key.clone()) {
                    return Err(anyhow!(
                        "duplicate shell-session pane_key {:?}",
                        buffer.pane_key
                    ));
                }
                let path = fs::canonicalize(&buffer.staging_path).with_context(|| {
                    format!(
                        "shell-session staging file does not exist: {}",
                        buffer.staging_path.display()
                    )
                })?;
                if !path.starts_with(&staging_root) {
                    return Err(anyhow!(
                        "shell-session staging file is outside {}: {}",
                        staging_root.display(),
                        path.display()
                    ));
                }
                if !path
                    .metadata()
                    .with_context(|| format!("failed to inspect {}", path.display()))?
                    .is_file()
                {
                    return Err(anyhow!(
                        "shell-session staging path is not a file: {}",
                        path.display()
                    ));
                }
                if !paths.insert(path.clone()) {
                    return Err(anyhow!(
                        "duplicate shell-session staging file {}",
                        path.display()
                    ));
                }
                Ok(path)
            })
            .collect()
    }

    fn buffer_paths_for_session(
        connection: &Connection,
        buffer_root: &Path,
        id: &str,
    ) -> Result<Vec<PathBuf>> {
        let mut statement = connection
            .prepare(
                "SELECT session_id, buffer_id
                   FROM shell_session_buffers
                  WHERE session_id = ?1",
            )
            .context("failed to prepare shell-session path query")?;
        let rows = statement
            .query_map(params![id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .context("failed to query shell-session paths")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to decode shell-session paths")?;
        Self::resolve_removable_buffer_paths(buffer_root, rows)
    }

    fn buffer_paths_for_scoped_session(
        connection: &Connection,
        buffer_root: &Path,
        id: &str,
        elevated: bool,
    ) -> Result<Vec<PathBuf>> {
        let mut statement = connection
            .prepare(
                "SELECT b.session_id, b.buffer_id
                   FROM shell_session_buffers b
                   JOIN shell_sessions s ON s.id = b.session_id
                  WHERE s.id = ?1 AND s.elevated = ?2",
            )
            .context("failed to prepare scoped shell-session path query")?;
        let rows = statement
            .query_map(params![id, elevated], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .context("failed to query scoped shell-session paths")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to decode scoped shell-session paths")?;
        Self::resolve_removable_buffer_paths(buffer_root, rows)
    }

    fn resolve_removable_buffer_paths(
        buffer_root: &Path,
        rows: Vec<(String, String)>,
    ) -> Result<Vec<PathBuf>> {
        let mut paths = Vec::with_capacity(rows.len());
        for (session_id, buffer_id) in rows {
            if let Some(path) =
                Self::resolve_optional_buffer_path(buffer_root, &session_id, &buffer_id)?
            {
                paths.push(path);
            }
        }
        Ok(paths)
    }

    fn resolve_existing_buffer_path(
        buffer_root: &Path,
        session_id: &str,
        buffer_id: &str,
    ) -> Result<PathBuf> {
        Self::resolve_optional_buffer_path(buffer_root, session_id, buffer_id)?.ok_or_else(|| {
            anyhow!("durable shell-session buffer is missing: {session_id}/{buffer_id}.buffer")
        })
    }

    fn resolve_optional_buffer_path(
        buffer_root: &Path,
        session_id: &str,
        buffer_id: &str,
    ) -> Result<Option<PathBuf>> {
        let session_id = canonical_db_uuid(session_id, "session_id")?;
        let buffer_id = canonical_db_uuid(buffer_id, "buffer_id")?;
        let candidate = buffer_root
            .join(session_id)
            .join(format!("{buffer_id}.buffer"));
        let canonical = match fs::canonicalize(&candidate) {
            Ok(path) => path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "durable shell-session buffer is inaccessible: {}",
                        candidate.display()
                    )
                });
            }
        };
        if !canonical.starts_with(buffer_root) {
            return Err(anyhow!(
                "durable shell-session buffer escapes buffer root: {}",
                canonical.display()
            ));
        }
        if !canonical
            .metadata()
            .with_context(|| format!("failed to inspect durable buffer {}", canonical.display()))?
            .is_file()
        {
            return Err(anyhow!(
                "durable shell-session buffer is not a file: {}",
                canonical.display()
            ));
        }
        Ok(Some(canonical))
    }

    fn run_startup_maintenance(&mut self, now: i64) {
        self.run_durable_maintenance(now);
        self.run_transient_maintenance(now);
    }

    fn run_durable_maintenance_if_due(&mut self, now: i64) {
        if now.saturating_sub(self.last_durable_maintenance_at) >= MAINTENANCE_INTERVAL_SECONDS {
            self.run_durable_maintenance(now);
        }
    }

    fn run_durable_maintenance(&mut self, now: i64) {
        if let Err(error) = self.expire_sessions(now) {
            tracing::warn!(
                target: "shell_sessions",
                error = %error,
                "failed to run durable shell-session retention"
            );
        }
        if let Err(error) = self.collect_orphan_buffers() {
            tracing::warn!(
                target: "shell_sessions",
                error = %error,
                "failed to collect orphan durable shell-session buffers"
            );
        }
        self.last_durable_maintenance_at = now;
    }

    fn expire_sessions(&mut self, now: i64) -> Result<()> {
        let expiration_cutoff = now - RETENTION_SECONDS;
        let buffer_root = self.buffer_root.clone();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("failed to begin shell-session retention transaction")?;
        let expired_rows = {
            let mut statement = transaction
                .prepare(
                    "SELECT b.session_id, b.buffer_id
                       FROM shell_session_buffers b
                       JOIN shell_sessions s ON s.id = b.session_id
                      WHERE s.last_used_at < ?1",
                )
                .context("failed to prepare expired shell-session path query")?;
            let rows = statement
                .query_map(params![expiration_cutoff], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .context("failed to query expired shell-session paths")?
                .collect::<rusqlite::Result<Vec<_>>>()
                .context("failed to decode expired shell-session paths")?;
            rows
        };
        let expired_paths = expired_rows
            .into_iter()
            .filter_map(|(session_id, buffer_id)| {
                match Self::resolve_existing_buffer_path(&buffer_root, &session_id, &buffer_id) {
                    Ok(path) => Some(path),
                    Err(error) => {
                        tracing::warn!(
                            target: "shell_sessions",
                            %session_id,
                            %buffer_id,
                            error = %error,
                            "skipping unsafe or missing expired shell-session buffer"
                        );
                        None
                    }
                }
            })
            .collect::<Vec<_>>();
        transaction
            .execute(
                "DELETE FROM shell_sessions WHERE last_used_at < ?1",
                params![expiration_cutoff],
            )
            .context("failed to expire shell sessions")?;
        transaction
            .commit()
            .context("failed to commit shell-session retention")?;
        remove_files(expired_paths.iter());
        remove_empty_parent_directories(&buffer_root, &expired_paths);
        Ok(())
    }

    fn run_transient_maintenance_if_due(&mut self, now: i64) {
        if now.saturating_sub(self.last_transient_maintenance_at) >= MAINTENANCE_INTERVAL_SECONDS {
            self.run_transient_maintenance(now);
        }
    }

    fn run_transient_maintenance(&mut self, now: i64) {
        cleanup_stale_staging_files(&self.staging_root, now);
        cleanup_stale_restore_snapshots(&self.restore_root, now);
        self.last_transient_maintenance_at = now;
    }

    fn collect_orphan_buffers(&mut self) -> Result<()> {
        let buffer_root = self.buffer_root.clone();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("failed to begin shell-session orphan GC transaction")?;
        let rows = {
            let mut statement = transaction
                .prepare("SELECT session_id, buffer_id FROM shell_session_buffers")
                .context("failed to prepare referenced buffer query")?;
            let rows = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .context("failed to query referenced buffers")?
                .collect::<rusqlite::Result<Vec<_>>>()
                .context("failed to decode referenced buffers")?;
            rows
        };
        let referenced = rows
            .into_iter()
            .filter_map(|(session_id, buffer_id)| {
                match Self::resolve_existing_buffer_path(&buffer_root, &session_id, &buffer_id) {
                    Ok(path) => Some(path),
                    Err(error) => {
                        tracing::warn!(
                            target: "shell_sessions",
                            %session_id,
                            %buffer_id,
                            error = %error,
                            "ignoring unsafe or missing shell-session buffer reference"
                        );
                        None
                    }
                }
            })
            .collect::<HashSet<_>>();
        collect_orphans_recursive(&buffer_root, &buffer_root, &referenced)?;
        transaction
            .commit()
            .context("failed to commit shell-session orphan GC transaction")?;
        Ok(())
    }
}

fn validate_save_params(params: &ShellSessionSaveParams) -> Result<()> {
    if params.name.trim().is_empty() {
        return Err(anyhow!("shell-session name must not be empty"));
    }
    if let Some(id) = params.id.as_deref() {
        validate_durable_id(id)?;
    }
    let layout: serde_json::Value = serde_json::from_str(&params.layout_json)
        .context("shell-session layout_json must be valid JSON")?;
    if !layout.is_object() {
        return Err(anyhow!("shell-session layout_json must be a JSON object"));
    }
    Ok(())
}

fn validate_durable_id(id: &str) -> Result<()> {
    Uuid::parse_str(id)
        .map(|_| ())
        .context("shell-session id must be a valid UUID")
}

fn canonical_db_uuid(id: &str, column: &str) -> Result<String> {
    Uuid::parse_str(id)
        .map(|id| id.to_string())
        .with_context(|| format!("corrupt shell-session {column}: expected UUID"))
}

fn record_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ShellSessionRecord> {
    Ok(ShellSessionRecord {
        id: row.get(0)?,
        name: row.get(1)?,
        layout_json: row.get(2)?,
        elevated: row.get(3)?,
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
        last_used_at: row.get(6)?,
        revision: row.get(7)?,
    })
}

fn place_file_atomically(source: &Path, destination: &Path) -> Result<()> {
    match fs::rename(source, destination) {
        Ok(()) => Ok(()),
        Err(rename_error) => {
            let temporary = destination.with_extension(format!("copy-{}.tmp", Uuid::new_v4()));
            let copy_result = (|| -> Result<()> {
                fs::copy(source, &temporary).with_context(|| {
                    format!(
                        "failed to move {} to {} ({rename_error}); copy fallback failed",
                        source.display(),
                        destination.display()
                    )
                })?;
                fs::File::open(&temporary)
                    .and_then(|file| file.sync_all())
                    .with_context(|| format!("failed to flush {}", temporary.display()))?;
                fs::rename(&temporary, destination).with_context(|| {
                    format!(
                        "failed to atomically publish copied buffer {}",
                        destination.display()
                    )
                })?;
                fs::remove_file(source).with_context(|| {
                    format!("failed to remove copied staging file {}", source.display())
                })
            })();
            if copy_result.is_err() {
                let _ = fs::remove_file(&temporary);
            }
            copy_result
        }
    }
}

fn snapshot_file(source: &Path, destination: &Path) -> Result<()> {
    match fs::hard_link(source, destination) {
        Ok(()) => Ok(()),
        Err(link_error) => {
            let temporary = destination.with_extension(format!("copy-{}.tmp", Uuid::new_v4()));
            let copy_result = (|| -> Result<()> {
                fs::copy(source, &temporary).with_context(|| {
                    format!(
                        "failed to snapshot {} to {} ({link_error}); copy fallback failed",
                        source.display(),
                        destination.display()
                    )
                })?;
                fs::File::open(&temporary)
                    .and_then(|file| file.sync_all())
                    .with_context(|| format!("failed to flush {}", temporary.display()))?;
                fs::rename(&temporary, destination).with_context(|| {
                    format!(
                        "failed to publish shell-session restore snapshot {}",
                        destination.display()
                    )
                })
            })();
            if copy_result.is_err() {
                let _ = fs::remove_file(&temporary);
            }
            copy_result
        }
    }
}

fn cleanup_stale_staging_files(directory: &Path, now: i64) {
    let cutoff = now - TRANSIENT_RETENTION_SECONDS;
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) => {
            tracing::warn!(
                target: "shell_sessions",
                directory = %directory.display(),
                error = %error,
                "failed to scan shell-session staging directory"
            );
            return;
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                tracing::warn!(
                    target: "shell_sessions",
                    directory = %directory.display(),
                    error = %error,
                    "failed to read shell-session staging entry"
                );
                continue;
            }
        };
        let path = entry.path();
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                tracing::warn!(
                    target: "shell_sessions",
                    path = %path.display(),
                    error = %error,
                    "failed to inspect shell-session staging file"
                );
                continue;
            }
        };
        if !metadata.is_file() {
            continue;
        }
        let modified_at = match metadata.modified() {
            Ok(modified) => modified
                .duration_since(UNIX_EPOCH)
                .map_or(0, |duration| duration.as_secs() as i64),
            Err(error) => {
                tracing::warn!(
                    target: "shell_sessions",
                    path = %path.display(),
                    error = %error,
                    "failed to read shell-session staging file age"
                );
                continue;
            }
        };
        if modified_at < cutoff {
            if let Err(error) = fs::remove_file(&path) {
                tracing::warn!(
                    target: "shell_sessions",
                    path = %path.display(),
                    error = %error,
                    "failed to remove stale shell-session staging file"
                );
            }
        }
    }
}

fn cleanup_stale_restore_snapshots(directory: &Path, now: i64) {
    let cutoff = now - TRANSIENT_RETENTION_SECONDS;
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) => {
            tracing::warn!(
                target: "shell_sessions",
                directory = %directory.display(),
                error = %error,
                "failed to scan shell-session restore cache"
            );
            return;
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                tracing::warn!(
                    target: "shell_sessions",
                    directory = %directory.display(),
                    error = %error,
                    "failed to read shell-session restore snapshot"
                );
                continue;
            }
        };
        let Some(created_at) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.split_once('-'))
            .and_then(|(timestamp, _)| timestamp.parse::<i64>().ok())
        else {
            continue;
        };
        if created_at >= cutoff {
            continue;
        }

        let path = entry.path();
        let removal = match entry.file_type() {
            Ok(file_type) if file_type.is_dir() => fs::remove_dir_all(&path),
            Ok(_) => fs::remove_file(&path),
            Err(error) => {
                tracing::warn!(
                    target: "shell_sessions",
                    path = %path.display(),
                    error = %error,
                    "failed to inspect stale shell-session restore snapshot"
                );
                continue;
            }
        };
        if let Err(error) = removal {
            tracing::warn!(
                target: "shell_sessions",
                path = %path.display(),
                error = %error,
                "failed to remove stale shell-session restore snapshot"
            );
        }
    }
}

fn cleanup_legacy_one_shot_buffers(directory: &Path) -> Result<usize> {
    let mut removed = 0;
    for entry in fs::read_dir(directory).with_context(|| {
        format!(
            "failed to scan Cascadia SettingsDirectory {}",
            directory.display()
        )
    })? {
        let entry = entry.with_context(|| {
            format!(
                "failed to read Cascadia SettingsDirectory {}",
                directory.display()
            )
        })?;
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        if !file_name.ends_with(".txt")
            || !LEGACY_BUFFER_PREFIXES
                .iter()
                .any(|prefix| file_name.starts_with(prefix))
        {
            continue;
        }
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to inspect legacy buffer {}", path.display()))?;
        if !file_type.is_file() && !file_type.is_symlink() {
            continue;
        }
        match fs::remove_file(&path) {
            Ok(()) => removed += 1,
            Err(error) => tracing::warn!(
                target: "shell_sessions",
                path = %path.display(),
                error = %error,
                "failed to remove legacy one-shot shell-session buffer"
            ),
        }
    }
    Ok(removed)
}

fn collect_orphans_recursive(
    root: &Path,
    directory: &Path,
    referenced: &HashSet<PathBuf>,
) -> Result<bool> {
    let mut empty = true;
    for entry in fs::read_dir(directory)
        .with_context(|| format!("failed to scan {}", directory.display()))?
    {
        let entry = entry.with_context(|| format!("failed to read {}", directory.display()))?;
        let path = entry.path();
        let canonical = match fs::canonicalize(&path) {
            Ok(path) => path,
            Err(error) => {
                tracing::warn!(
                    target: "shell_sessions",
                    path = %path.display(),
                    error = %error,
                    "failed to canonicalize orphan candidate; leaving it untouched"
                );
                empty = false;
                continue;
            }
        };
        if !canonical.starts_with(root) {
            tracing::warn!(
                target: "shell_sessions",
                path = %path.display(),
                canonical = %canonical.display(),
                "orphan candidate escapes buffer root; leaving it untouched"
            );
            empty = false;
            continue;
        }
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to inspect {}", path.display()))?;
        if file_type.is_dir() {
            if collect_orphans_recursive(root, &path, referenced)? {
                if let Err(error) = fs::remove_dir(&path) {
                    tracing::warn!(
                        target: "shell_sessions",
                        path = %path.display(),
                        error = %error,
                        "failed to remove orphan shell-session directory"
                    );
                    empty = false;
                }
            } else {
                empty = false;
            }
        } else if referenced.contains(&canonical) {
            empty = false;
        } else {
            if let Err(error) = fs::remove_file(&path) {
                tracing::warn!(
                    target: "shell_sessions",
                    path = %path.display(),
                    error = %error,
                    "failed to remove orphan shell-session buffer"
                );
                empty = false;
            }
        }
    }
    Ok(empty)
}

fn remove_files<'a>(paths: impl IntoIterator<Item = &'a PathBuf>) {
    for path in paths {
        if let Err(error) = fs::remove_file(path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    target: "shell_sessions",
                    path = %path.display(),
                    error = %error,
                    "failed to remove obsolete shell-session buffer"
                );
            }
        }
    }
}

fn remove_empty_parent_directories(root: &Path, paths: &[PathBuf]) {
    for path in paths {
        if let Some(parent) = path.parent().filter(|parent| *parent != root) {
            let _ = fs::remove_dir(parent);
        }
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs() as i64)
}

/// Returns whether this helper process runs with an elevated token.
pub fn current_process_is_elevated() -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Security::{
        GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut returned = 0;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            &mut elevation as *mut TOKEN_ELEVATION as *mut std::ffi::c_void,
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        );
        CloseHandle(token);
        ok != 0 && elevation.TokenIsElevated != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Result<Self> {
            let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("target")
                .join("shell-session-tests")
                .join(Uuid::new_v4().to_string());
            fs::create_dir_all(&root)?;
            Ok(Self(root))
        }

        fn staging_file(&self, name: &str, contents: &[u8]) -> Result<PathBuf> {
            let directory = self.0.join(STAGING_DIRECTORY);
            fs::create_dir_all(&directory)?;
            let path = directory.join(name);
            fs::write(&path, contents)?;
            Ok(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn save_params(
        directory: &TestDirectory,
        name: &str,
        file_name: &str,
    ) -> Result<ShellSessionSaveParams> {
        Ok(ShellSessionSaveParams {
            id: None,
            expected_revision: None,
            name: name.to_string(),
            layout_json: r#"{"actions":[]}"#.to_string(),
            elevated: false,
            buffers: vec![ShellSessionBufferInput {
                pane_key: "pane-1".to_string(),
                staging_path: directory.staging_file(file_name, file_name.as_bytes())?,
            }],
        })
    }

    fn stored_buffer_ids(store: &StoreCore, session_id: &str) -> Result<(String, String)> {
        store
            .connection
            .query_row(
                "SELECT session_id, buffer_id
                   FROM shell_session_buffers
                  WHERE session_id = ?1",
                params![session_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .context("saved buffer row missing")
    }

    #[test]
    fn duplicate_names_remain_distinct() -> Result<()> {
        let directory = TestDirectory::new()?;
        let mut store = StoreCore::open(directory.0.clone(), 100, None)?;
        let first = store.save(save_params(&directory, "same", "first.tmp")?, 100)?;
        let second = store.save(save_params(&directory, "same", "second.tmp")?, 101)?;

        let list = store.list(&ShellSessionsListParams { elevated: false }, 101)?;
        assert_eq!(list.sessions.len(), 2);
        assert_ne!(first.id, second.id);
        assert_eq!(list.sessions[0].name, "same");
        assert_eq!(list.sessions[1].name, "same");
        Ok(())
    }

    #[test]
    fn save_rejects_empty_names_and_invalid_layouts() -> Result<()> {
        let directory = TestDirectory::new()?;
        let mut store = StoreCore::open(directory.0.clone(), 100, None)?;
        let mut params = save_params(&directory, "valid", "validation.tmp")?;

        params.name = " \t".to_string();
        let error = store.save(params.clone(), 100).unwrap_err();
        assert!(error
            .to_string()
            .contains("shell-session name must not be empty"));

        params.name = "valid".to_string();
        params.layout_json = "{".to_string();
        let error = store.save(params.clone(), 100).unwrap_err();
        assert!(error
            .to_string()
            .contains("shell-session layout_json must be valid JSON"));

        params.layout_json = "[]".to_string();
        let error = store.save(params, 100).unwrap_err();
        assert!(error
            .to_string()
            .contains("shell-session layout_json must be a JSON object"));
        Ok(())
    }

    #[test]
    fn supplied_durable_ids_must_be_uuids() -> Result<()> {
        let directory = TestDirectory::new()?;
        let mut store = StoreCore::open(directory.0.clone(), 100, None)?;
        let mut save = save_params(&directory, "valid", "invalid-id.tmp")?;
        save.id = Some("not-a-uuid".to_string());
        let error = store.save(save, 100).unwrap_err();
        assert!(error
            .to_string()
            .contains("shell-session id must be a valid UUID"));

        let error = store
            .get(
                &ShellSessionGetParams {
                    id: "not-a-uuid".to_string(),
                    elevated: false,
                },
                100,
            )
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("shell-session id must be a valid UUID"));

        let error = store
            .delete(&ShellSessionDeleteParams {
                id: "not-a-uuid".to_string(),
                elevated: false,
            })
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("shell-session id must be a valid UUID"));
        Ok(())
    }

    #[test]
    fn repeated_get_does_not_consume_session() -> Result<()> {
        let directory = TestDirectory::new()?;
        let mut store = StoreCore::open(directory.0.clone(), 100, None)?;
        let saved = store.save(save_params(&directory, "repeat", "repeat.tmp")?, 100)?;
        let params = ShellSessionGetParams {
            id: saved.id,
            elevated: false,
        };

        let first = store.get(&params, 200)?.context("first get missed")?;
        let second = store.get(&params, 300)?.context("second get missed")?;
        assert_eq!(first.buffers[0].pane_key, second.buffers[0].pane_key);
        assert_ne!(first.buffers[0].path, second.buffers[0].path);
        assert_eq!(second.session.last_used_at, 300);
        assert!(first.buffers[0].path.exists());
        assert!(second.buffers[0].path.exists());
        assert_eq!(fs::read(&first.buffers[0].path)?, b"repeat.tmp");
        assert_eq!(fs::read(&second.buffers[0].path)?, b"repeat.tmp");
        Ok(())
    }

    #[test]
    fn matching_revision_update_preserves_restore_snapshot() -> Result<()> {
        let directory = TestDirectory::new()?;
        let mut store = StoreCore::open(directory.0.clone(), 100, None)?;
        let first = store.save(save_params(&directory, "old", "old.tmp")?, 100)?;
        let old_path = store
            .get(
                &ShellSessionGetParams {
                    id: first.id.clone(),
                    elevated: false,
                },
                100,
            )?
            .context("saved session missing")?
            .buffers[0]
            .path
            .clone();
        let mut update = save_params(&directory, "new", "new.tmp")?;
        update.id = Some(first.id.clone());
        update.expected_revision = Some(first.revision);

        let second = store.save(update, 200)?;
        assert_eq!(second.id, first.id);
        assert_eq!(second.revision, 2);
        assert!(!second.forked);
        assert!(old_path.exists());
        assert_eq!(fs::read(&old_path)?, b"old.tmp");

        let updated = store
            .get(
                &ShellSessionGetParams {
                    id: first.id,
                    elevated: false,
                },
                200,
            )?
            .context("updated session missing")?;
        assert_ne!(updated.buffers[0].path, old_path);
        assert_eq!(fs::read(&updated.buffers[0].path)?, b"new.tmp");
        Ok(())
    }

    #[test]
    fn missing_old_buffers_do_not_block_update_or_delete() -> Result<()> {
        let directory = TestDirectory::new()?;
        let mut store = StoreCore::open(directory.0.clone(), 100, None)?;
        let first = store.save(save_params(&directory, "old", "missing-old.tmp")?, 100)?;
        let (session_id, buffer_id) = stored_buffer_ids(&store, &first.id)?;
        let old_path =
            StoreCore::resolve_existing_buffer_path(&store.buffer_root, &session_id, &buffer_id)?;
        fs::remove_file(old_path)?;

        let mut update = save_params(&directory, "new", "replacement.tmp")?;
        update.id = Some(first.id.clone());
        update.expected_revision = Some(first.revision);
        let updated = store.save(update, 200)?;
        assert_eq!(updated.id, first.id);
        assert_eq!(updated.revision, 2);

        let (session_id, buffer_id) = stored_buffer_ids(&store, &updated.id)?;
        let replacement =
            StoreCore::resolve_existing_buffer_path(&store.buffer_root, &session_id, &buffer_id)?;
        fs::remove_file(replacement)?;
        assert!(
            store
                .delete(&ShellSessionDeleteParams {
                    id: updated.id,
                    elevated: false,
                })?
                .deleted
        );
        Ok(())
    }

    #[test]
    fn database_path_column_is_never_authoritative() -> Result<()> {
        let directory = TestDirectory::new()?;
        let mut store = StoreCore::open(directory.0.clone(), 100, None)?;
        let first = store.save(save_params(&directory, "old", "old.tmp")?, 100)?;
        let (session_id, buffer_id) = stored_buffer_ids(&store, &first.id)?;
        let original_path =
            StoreCore::resolve_existing_buffer_path(&store.buffer_root, &session_id, &buffer_id)?;
        let outside = directory.0.join("outside.buffer");
        fs::write(&outside, b"must survive")?;
        store.connection.execute(
            "UPDATE shell_session_buffers SET path = ?1 WHERE session_id = ?2",
            params![outside.to_string_lossy(), first.id],
        )?;

        let restored = store
            .get(
                &ShellSessionGetParams {
                    id: first.id.clone(),
                    elevated: false,
                },
                101,
            )?
            .context("session with tampered compatibility path missing")?;
        assert_eq!(fs::read(&restored.buffers[0].path)?, b"old.tmp");

        let mut update = save_params(&directory, "new", "new.tmp")?;
        update.id = Some(first.id.clone());
        update.expected_revision = Some(first.revision);
        store.save(update, 102)?;
        assert!(!original_path.exists());
        assert_eq!(fs::read(&outside)?, b"must survive");

        let (session_id, buffer_id) = stored_buffer_ids(&store, &first.id)?;
        let updated_path =
            StoreCore::resolve_existing_buffer_path(&store.buffer_root, &session_id, &buffer_id)?;
        store.connection.execute(
            "UPDATE shell_session_buffers SET path = ?1 WHERE session_id = ?2",
            params![outside.to_string_lossy(), first.id],
        )?;
        assert!(
            store
                .delete(&ShellSessionDeleteParams {
                    id: first.id,
                    elevated: false,
                })?
                .deleted
        );
        assert!(!updated_path.exists());
        assert_eq!(fs::read(&outside)?, b"must survive");
        Ok(())
    }

    #[test]
    fn corrupt_buffer_id_fails_operations_but_not_maintenance() -> Result<()> {
        let directory = TestDirectory::new()?;
        let mut store = StoreCore::open(directory.0.clone(), 100, None)?;
        let saved = store.save(save_params(&directory, "corrupt", "corrupt.tmp")?, 100)?;
        let outside = directory.0.join("outside-expiry.buffer");
        fs::write(&outside, b"must survive expiry")?;
        store.connection.execute(
            "UPDATE shell_session_buffers SET buffer_id = 'not-a-uuid', path = ?1
              WHERE session_id = ?2",
            params![outside.to_string_lossy(), saved.id],
        )?;

        let error = store
            .get(
                &ShellSessionGetParams {
                    id: saved.id.clone(),
                    elevated: false,
                },
                101,
            )
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("corrupt shell-session buffer_id: expected UUID"));

        store.run_durable_maintenance(100 + RETENTION_SECONDS + 1);
        assert!(store
            .list(
                &ShellSessionsListParams { elevated: false },
                100 + RETENTION_SECONDS + 1,
            )?
            .sessions
            .is_empty());
        assert_eq!(fs::read(&outside)?, b"must survive expiry");
        Ok(())
    }

    #[test]
    fn junction_escape_is_never_read_or_deleted() -> Result<()> {
        use std::os::windows::fs::symlink_dir;

        let directory = TestDirectory::new()?;
        let mut store = StoreCore::open(directory.0.clone(), 100, None)?;
        let saved = store.save(save_params(&directory, "junction", "junction.tmp")?, 100)?;
        let (session_id, buffer_id) = stored_buffer_ids(&store, &saved.id)?;
        let original =
            StoreCore::resolve_existing_buffer_path(&store.buffer_root, &session_id, &buffer_id)?;
        let session_directory = original
            .parent()
            .context("stored buffer had no session directory")?
            .to_path_buf();
        let file_name = original
            .file_name()
            .context("stored buffer had no file name")?
            .to_owned();
        fs::remove_file(&original)?;
        fs::remove_dir(&session_directory)?;

        let outside_directory = directory.0.join("outside-junction-target");
        fs::create_dir_all(&outside_directory)?;
        let outside_buffer = outside_directory.join(file_name);
        fs::write(&outside_buffer, b"outside")?;
        if let Err(error) = symlink_dir(&outside_directory, &session_directory) {
            eprintln!("skipping junction containment test: {error}");
            return Ok(());
        }

        let get_error = store
            .get(
                &ShellSessionGetParams {
                    id: saved.id.clone(),
                    elevated: false,
                },
                101,
            )
            .unwrap_err();
        assert!(get_error.to_string().contains("escapes buffer root"));
        assert!(store
            .delete(&ShellSessionDeleteParams {
                id: saved.id.clone(),
                elevated: false,
            })
            .is_err());
        assert_eq!(fs::read(&outside_buffer)?, b"outside");

        store.run_durable_maintenance(100 + RETENTION_SECONDS + 1);
        assert_eq!(fs::read(&outside_buffer)?, b"outside");
        Ok(())
    }

    #[test]
    fn stale_revision_forks_without_overwriting_original() -> Result<()> {
        let directory = TestDirectory::new()?;
        let mut store = StoreCore::open(directory.0.clone(), 100, None)?;
        let first = store.save(save_params(&directory, "original", "original.tmp")?, 100)?;
        let mut stale = save_params(&directory, "fork", "fork.tmp")?;
        stale.id = Some(first.id.clone());
        stale.expected_revision = Some(0);

        let fork = store.save(stale, 200)?;
        assert!(fork.forked);
        assert_ne!(fork.id, first.id);
        assert_eq!(fork.revision, 1);
        assert!(store
            .get(
                &ShellSessionGetParams {
                    id: first.id,
                    elevated: false,
                },
                200,
            )?
            .is_some());
        Ok(())
    }

    #[test]
    fn stale_revision_from_second_connection_forks() -> Result<()> {
        let directory = TestDirectory::new()?;
        let mut first_store = StoreCore::open(directory.0.clone(), 100, None)?;
        let mut second_store = StoreCore::open(directory.0.clone(), 100, None)?;
        let first = first_store.save(
            save_params(&directory, "original", "original-shared.tmp")?,
            100,
        )?;

        let mut first_update = save_params(&directory, "winner", "winner.tmp")?;
        first_update.id = Some(first.id.clone());
        first_update.expected_revision = Some(first.revision);
        let mut stale_update = save_params(&directory, "stale", "stale-shared.tmp")?;
        stale_update.id = Some(first.id.clone());
        stale_update.expected_revision = Some(first.revision);

        let winner = first_store.save(first_update, 200)?;
        let fork = second_store.save(stale_update, 201)?;
        assert_eq!(winner.id, first.id);
        assert_eq!(winner.revision, 2);
        assert!(!winner.forked);
        assert_ne!(fork.id, first.id);
        assert_eq!(fork.revision, 1);
        assert!(fork.forked);
        Ok(())
    }

    #[test]
    fn orphan_gc_waits_for_precommit_buffer_reference() -> Result<()> {
        use std::sync::mpsc::TryRecvError;
        use std::time::Duration;

        let directory = TestDirectory::new()?;
        let mut writer = StoreCore::open(directory.0.clone(), 100, None)?;
        let mut collector = StoreCore::open(directory.0.clone(), 100, None)?;
        let buffer_root = writer.buffer_root.clone();
        let session_id = Uuid::new_v4().to_string();
        let buffer_id = Uuid::new_v4().to_string();
        let session_directory = buffer_root.join(&session_id);
        fs::create_dir_all(&session_directory)?;
        let buffer_path = session_directory.join(format!("{buffer_id}.buffer"));

        let transaction = writer
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        fs::write(&buffer_path, b"precommit")?;
        transaction.execute(
            "INSERT INTO shell_sessions
                (id, name, layout_json, elevated, created_at, updated_at,
                 last_used_at, revision)
             VALUES (?1, 'shared', '{}', 0, 100, 100, 100, 1)",
            params![session_id],
        )?;
        transaction.execute(
            "INSERT INTO shell_session_buffers
                (buffer_id, session_id, pane_key, path)
             VALUES (?1, ?2, 'pane-1', ?3)",
            params![buffer_id, session_id, buffer_path.to_string_lossy()],
        )?;

        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let collector_thread = std::thread::spawn(move || {
            let _ = started_tx.send(());
            let result = collector.collect_orphan_buffers();
            let _ = done_tx.send(result);
        });
        started_rx.recv_timeout(Duration::from_secs(1))?;
        std::thread::sleep(Duration::from_millis(100));
        assert!(matches!(done_rx.try_recv(), Err(TryRecvError::Empty)));
        assert!(buffer_path.exists());

        transaction.commit()?;
        done_rx.recv_timeout(Duration::from_secs(2))??;
        collector_thread
            .join()
            .map_err(|_| anyhow!("orphan collector thread panicked"))?;
        assert_eq!(fs::read(&buffer_path)?, b"precommit");
        Ok(())
    }

    #[test]
    fn startup_expires_sessions_from_last_used_time() -> Result<()> {
        let directory = TestDirectory::new()?;
        let old_path = {
            let mut store = StoreCore::open(directory.0.clone(), 100, None)?;
            let saved = store.save(save_params(&directory, "old", "old.tmp")?, 100)?;
            store
                .get(
                    &ShellSessionGetParams {
                        id: saved.id,
                        elevated: false,
                    },
                    100,
                )?
                .context("saved session missing")?
                .buffers[0]
                .path
                .clone()
        };

        let store = StoreCore::open(directory.0.clone(), 100 + RETENTION_SECONDS + 1, None)?;
        assert!(store
            .list(
                &ShellSessionsListParams { elevated: false },
                100 + RETENTION_SECONDS + 1,
            )?
            .sessions
            .is_empty());
        assert!(!old_path.exists());
        Ok(())
    }

    #[test]
    fn open_store_filters_then_periodically_expires_stale_session() -> Result<()> {
        let directory = TestDirectory::new()?;
        let mut store = StoreCore::open(directory.0.clone(), 100, None)?;
        let saved = store.save(save_params(&directory, "old", "periodic.tmp")?, 100)?;
        let (session_id, buffer_id) = stored_buffer_ids(&store, &saved.id)?;
        let durable_path =
            StoreCore::resolve_existing_buffer_path(&store.buffer_root, &session_id, &buffer_id)?;
        let expired_now = 100 + RETENTION_SECONDS + 1;

        assert!(store
            .list(&ShellSessionsListParams { elevated: false }, expired_now)?
            .sessions
            .is_empty());
        assert!(store
            .get(
                &ShellSessionGetParams {
                    id: saved.id.clone(),
                    elevated: false,
                },
                expired_now,
            )?
            .is_none());
        let last_used: i64 = store.connection.query_row(
            "SELECT last_used_at FROM shell_sessions WHERE id = ?1",
            params![saved.id],
            |row| row.get(0),
        )?;
        assert_eq!(last_used, 100);

        store.run_durable_maintenance_if_due(expired_now);
        assert!(!durable_path.exists());
        assert_eq!(
            store
                .connection
                .query_row("SELECT COUNT(*) FROM shell_sessions", [], |row| row
                    .get::<_, i64>(0),)?,
            0
        );
        Ok(())
    }

    #[test]
    fn startup_removes_orphan_buffers() -> Result<()> {
        let directory = TestDirectory::new()?;
        let orphan = directory
            .0
            .join(BUFFER_DIRECTORY)
            .join("orphan")
            .join("buffer.bin");
        fs::create_dir_all(orphan.parent().context("orphan had no parent")?)?;
        fs::write(&orphan, b"orphan")?;

        let _store = StoreCore::open(directory.0.clone(), 100, None)?;
        assert!(!orphan.exists());
        Ok(())
    }

    #[test]
    fn startup_removes_stale_staging_and_preserves_fresh_files() -> Result<()> {
        let directory = TestDirectory::new()?;
        let stale = directory.staging_file("stale.tmp", b"stale")?;
        let future = unix_now() + TRANSIENT_RETENTION_SECONDS + 2;
        let mut store = StoreCore::open(directory.0.clone(), future, None)?;
        assert!(!stale.exists());

        let fresh = directory.staging_file("fresh.tmp", b"fresh")?;
        store.run_startup_maintenance(unix_now());
        assert!(fresh.exists());
        Ok(())
    }

    #[test]
    fn startup_removes_only_stale_restore_snapshots() -> Result<()> {
        let directory = TestDirectory::new()?;
        let restore_root = directory.0.join(RESTORE_CACHE_DIRECTORY);
        let stale = restore_root.join(format!("100-{}", Uuid::new_v4()));
        let fresh_created_at = 100 + TRANSIENT_RETENTION_SECONDS;
        let fresh = restore_root.join(format!("{fresh_created_at}-{}", Uuid::new_v4()));
        fs::create_dir_all(&stale)?;
        fs::create_dir_all(&fresh)?;
        fs::write(stale.join("buffer"), b"stale")?;
        fs::write(fresh.join("buffer"), b"fresh")?;

        let _store = StoreCore::open(
            directory.0.clone(),
            100 + TRANSIENT_RETENTION_SECONDS + 1,
            None,
        )?;
        assert!(!stale.exists());
        assert!(fresh.exists());
        Ok(())
    }

    #[test]
    fn startup_removes_only_legacy_one_shot_buffers() -> Result<()> {
        let directory = TestDirectory::new()?;
        let old_normal = directory.0.join("shell_buffer_session.txt");
        let old_elevated = directory.0.join("shell_elevated_session.txt");
        let unrelated = directory.0.join("shell_buffer_session.bin");
        let near_match = directory.0.join("other_shell_buffer_session.txt");
        for path in [&old_normal, &old_elevated, &unrelated, &near_match] {
            fs::write(path, b"buffer")?;
        }

        let _store = StoreCore::open(directory.0.clone(), 100, Some(&directory.0))?;
        assert!(!old_normal.exists());
        assert!(!old_elevated.exists());
        assert!(unrelated.exists());
        assert!(near_match.exists());
        Ok(())
    }
}
