// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

pub(in crate::agent_center) mod check_worker;
pub(in crate::agent_center) mod release_worker;
pub(in crate::agent_center) mod scenario;

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use scenario::Scenario;
use std::{
    fs::{File, OpenOptions as FileOptions},
    path::{Path, PathBuf},
};

const DATABASE: &str = "work-story.sqlite3";

pub(in crate::agent_center) struct Store {
    connection: Connection,
    _writer: File,
    root: PathBuf,
}

impl Store {
    pub(in crate::agent_center) fn open(root: &Path, reset: bool) -> Result<(Self, Scenario)> {
        std::fs::create_dir_all(root).context("Creating isolated demo directory")?;
        let writer = FileOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join("work-story.lock"))
            .context("Opening demo writer lock")?;
        writer
            .try_lock()
            .context("This demo is already open; close its window before reopening or resetting")?;
        let connection = Connection::open(root.join(DATABASE)).context("Opening demo database")?;
        connection.execute_batch(
            "PRAGMA synchronous=FULL;
             CREATE TABLE IF NOT EXISTS scenario (
                 id INTEGER PRIMARY KEY CHECK (id = 1),
                 payload TEXT NOT NULL
             );",
        )?;
        let store = Self {
            connection,
            _writer: writer,
            root: root.to_path_buf(),
        };
        let saved: Option<String> = store
            .connection
            .query_row("SELECT payload FROM scenario WHERE id=1", [], |row| {
                row.get(0)
            })
            .optional()?;
        let state = if reset || saved.is_none() {
            let state = Scenario::new();
            store.save(&state)?;
            state
        } else {
            decode(saved.as_deref().context("Missing demo snapshot")?)?
        };
        Ok((store, state))
    }

    pub(in crate::agent_center) fn save(&self, scenario: &Scenario) -> Result<()> {
        scenario.validate()?;
        self.connection.execute(
            "INSERT INTO scenario(id,payload) VALUES(1,?1)
             ON CONFLICT(id) DO UPDATE SET payload=excluded.payload",
            [serde_json::to_string(scenario)?],
        )?;
        Ok(())
    }

    pub(in crate::agent_center) fn root(&self) -> &Path {
        &self.root
    }
}

fn decode(payload: &str) -> Result<Scenario> {
    let state: Scenario = serde_json::from_str(payload).context("Reading saved demo story")?;
    state.validate().context("Invalid saved demo story")?;
    Ok(state)
}

fn inspect(root: &Path) -> Result<Scenario> {
    let connection =
        Connection::open_with_flags(root.join(DATABASE), OpenFlags::SQLITE_OPEN_READ_ONLY)
            .context("Opening saved demo for inspection; run the demo first")?;
    let payload: String =
        connection.query_row("SELECT payload FROM scenario WHERE id=1", [], |row| {
            row.get(0)
        })?;
    decode(&payload)
}

pub async fn run(state_dir: Option<PathBuf>, reset: bool, read_only: bool) -> Result<()> {
    let root = state_dir
        .or_else(|| {
            crate::runtime_paths::intelligent_terminal_root()
                .map(|root| root.join("work-story-demo"))
        })
        .context("Cannot resolve package-private demo storage")?;
    if read_only {
        anyhow::ensure!(!reset, "Inspection cannot reset the demo");
        let state = tokio::task::spawn_blocking(move || inspect(&root)).await??;
        println!("{}", serde_json::to_string_pretty(&state)?);
        return Ok(());
    }
    let (store, scenario) =
        tokio::task::spawn_blocking(move || Store::open(&root, reset)).await??;
    super::ui::run_demo(store, scenario).await
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!("wta-work-demo-{}", uuid::Uuid::new_v4())))
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            if self.0.exists() {
                std::fs::remove_dir_all(&self.0).unwrap();
            }
        }
    }

    #[test]
    fn demo_store_reopens_exact_state_and_inspects_while_writer_is_live() {
        let root = Directory::new();
        let (store, mut state) = Store::open(&root.0, false).unwrap();
        state.apply("Continue fixing issue #4821").unwrap();
        store.save(&state).unwrap();
        assert_eq!(inspect(&root.0).unwrap(), state);
        assert!(
            Store::open(&root.0, true).is_err(),
            "reset cannot race another writer"
        );
        drop(store);
        let (store, loaded) = Store::open(&root.0, false).unwrap();
        assert_eq!(state, loaded);
        drop(store);
        let (_store, reset) = Store::open(&root.0, true).unwrap();
        assert_eq!(reset, Scenario::new());
    }

    #[test]
    fn inspection_does_not_create_state_and_corrupt_state_never_silently_resets() {
        let root = Directory::new();
        assert!(inspect(&root.0).is_err());
        assert!(!root.0.exists());
        let (store, _) = Store::open(&root.0, false).unwrap();
        store
            .connection
            .execute("UPDATE scenario SET payload='{}'", [])
            .unwrap();
        drop(store);
        assert!(Store::open(&root.0, false).is_err());
        assert!(inspect(&root.0).is_err());
        let (_store, _) = Store::open(&root.0, true).unwrap();
    }

    #[test]
    fn invalid_candidate_cannot_replace_saved_demo() {
        let root = Directory::new();
        let (store, initial) = Store::open(&root.0, false).unwrap();
        let mut invalid = initial.clone();
        invalid.revision += 1;
        assert!(store.save(&invalid).is_err());
        assert_eq!(inspect(&root.0).unwrap(), initial);
    }
}
