// tools/wta/src/master/shell_sessions_db.rs
//
// Durable shell-session SQLite store, owned **exclusively** by `wta-master`.
//
// Why master-only: the store is written on tab close and read by the
// `/shell-sessions` picker. Rather than let C++ and every helper open the
// same DB (cross-process + cross-language locking, plus the elevated /
// non-elevated split that WT's `state.json` needs), master is the single
// owner. C++ ships the metadata via a `save_shell_session` WT event; helpers
// query via the `intellterm.wta/shell_sessions/list` ACP ext method. This
// keeps the DB single-writer/single-reader, so no WAL/busy-timeout dance is
// needed.
//
// What lives here vs. on disk:
//   * SQLite row  = { session_id (PK = anchor pane GUID), name, cwd,
//     updated_at, last_used_at, layout_json, buffer_guids }. Keyed by
//     session_id (NOT the title), so different tabs may share a name.
//     `updated_at` (last save) drives list order; `last_used_at` (last save OR
//     restore) drives the TTL.
//   * Scrollback  = files on disk (`<it_root>\shell-session-buffers\{guid}.txt`),
//     written/read by WT's terminal core via a file handle. Too large to ship
//     base64 through JSON-RPC, so they stay as files; the row only references
//     them by GUID. TTL cleanup deletes the row and its buffer files together
//     (see [`ttl_sweep`]), and an upsert over an existing name returns the
//     superseded GUIDs so the caller can unlink the now-orphaned files.

use anyhow::{Context, Result};
use rusqlite::Connection;

/// One durable shell session as stored in the DB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellSessionRow {
    /// Stable per-tab identity and the row's primary key — the anchor terminal
    /// pane's WT session GUID (a restored tab reuses it, so re-saving updates
    /// the same row instead of duplicating). NOT the title: two different tabs
    /// may share a `name`, so the display name can't be the key.
    pub session_id: String,
    /// The tab's title — display label only (no longer unique across rows).
    pub name: String,
    /// Working directory captured at save time (display only; may be empty).
    pub cwd: String,
    /// Unix seconds of the last **save** (content change). Drives the
    /// newest-first list ordering.
    pub updated_at: i64,
    /// Unix seconds of the last **use** — a save or a restore. Drives the TTL
    /// (a session you keep restoring stays alive even if never re-saved).
    pub last_used_at: i64,
    /// Opaque WT `WindowLayout` JSON. C++ replays this verbatim on restore.
    pub layout_json: String,
    /// Pane session GUIDs whose scrollback lives in
    /// `<it_root>\shell-session-buffers\{guid}.txt`. Used by TTL / upsert to
    /// delete the orphaned buffer files.
    pub buffer_guids: Vec<String>,
    /// The agent pane's WT session GUID (`pane_session_id`) at save time, if the
    /// saved tab had an open agent pane. `None` when it didn't. Resolved to an
    /// ACP session id (via `agent_pane_origin`) on restore, so the agent
    /// conversation is resumed into the rebuilt tab.
    pub agent_pane_session_id: Option<String>,
}

/// Open (creating if needed) the shell-session DB at `db_path` and ensure the
/// schema exists.
///
/// `buffer_guids` is stored as a JSON array string — a session rarely has more
/// than a handful of panes, so a child table would be overkill.
pub fn open(db_path: &std::path::Path) -> Result<Connection> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating shell-session DB dir {}", parent.display()))?;
    }
    let conn = Connection::open(db_path)
        .with_context(|| format!("opening shell-session DB {}", db_path.display()))?;
    // master is the only writer, but a `/shell-sessions` list can race a
    // concurrent save; a short busy timeout turns a rare SQLITE_BUSY into a
    // brief wait instead of an error.
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .context("setting shell-session DB busy_timeout")?;
    init_schema(&conn)?;
    Ok(conn)
}

/// Create the `sessions` table if it does not exist. Split out so tests can run
/// it against an in-memory connection.
fn init_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS sessions (
             session_id   TEXT PRIMARY KEY,
             name         TEXT NOT NULL DEFAULT '',
             cwd          TEXT NOT NULL DEFAULT '',
             updated_at   INTEGER NOT NULL DEFAULT 0,
             last_used_at INTEGER NOT NULL DEFAULT 0,
             layout_json  TEXT NOT NULL,
             buffer_guids TEXT NOT NULL DEFAULT '[]',
             agent_pane_session_id TEXT
         );
         CREATE TABLE IF NOT EXISTS meta (
             key   TEXT PRIMARY KEY,
             value INTEGER NOT NULL
         );",
    )
    .context("initializing shell-session schema")?;
    // Migrate DBs created before `agent_pane_session_id` existed (the column was
    // added when durable agent-session resume landed). Idempotent: skipped once
    // the column is present.
    ensure_agent_pane_session_id_column(conn)?;
    // Migrate DBs whose `sessions` table still keys on `name` (the pre-refactor
    // shape) to the `session_id` primary key, so different tabs can share a name.
    // Must run before the `updated_at` index below: a legacy table has no such
    // column until the migration rebuilds it.
    migrate_name_pk_to_session_id(conn)?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_sessions_updated_at ON sessions(updated_at)",
        [],
    )
    .context("creating shell-session updated_at index")?;
    Ok(())
}

/// Add the `agent_pane_session_id` column to a preexisting `sessions` table
/// that was created before that column existed. No-op when it's already there.
fn ensure_agent_pane_session_id_column(conn: &Connection) -> Result<()> {
    let has_column = conn
        .prepare("SELECT 1 FROM pragma_table_info('sessions') WHERE name = 'agent_pane_session_id'")
        .and_then(|mut stmt| stmt.exists([]))
        .context("checking for agent_pane_session_id column")?;
    if !has_column {
        conn.execute("ALTER TABLE sessions ADD COLUMN agent_pane_session_id TEXT", [])
            .context("adding agent_pane_session_id column")?;
    }
    Ok(())
}

/// Rebuild a legacy `sessions` table (title as PRIMARY KEY, single `saved_at`)
/// into the current shape (`session_id` PRIMARY KEY, `name` a plain column,
/// split `updated_at` / `last_used_at`), preserving rows. The old title becomes
/// `name`; the new `session_id` is derived from the row's first buffer GUID (the
/// anchor pane, matching how C++ now keys sessions), falling back to the title
/// when a legacy row had no buffers; the old `saved_at` seeds both timestamps.
/// No-op once the `session_id` column is present.
fn migrate_name_pk_to_session_id(conn: &Connection) -> Result<()> {
    let has_session_id = conn
        .prepare("SELECT 1 FROM pragma_table_info('sessions') WHERE name = 'session_id'")
        .and_then(|mut stmt| stmt.exists([]))
        .context("checking for session_id column")?;
    if has_session_id {
        return Ok(());
    }
    conn.execute_batch(
        "BEGIN;
         ALTER TABLE sessions RENAME TO sessions_legacy;
         CREATE TABLE sessions (
             session_id   TEXT PRIMARY KEY,
             name         TEXT NOT NULL DEFAULT '',
             cwd          TEXT NOT NULL DEFAULT '',
             updated_at   INTEGER NOT NULL DEFAULT 0,
             last_used_at INTEGER NOT NULL DEFAULT 0,
             layout_json  TEXT NOT NULL,
             buffer_guids TEXT NOT NULL DEFAULT '[]',
             agent_pane_session_id TEXT
         );
         INSERT OR IGNORE INTO sessions
             (session_id, name, cwd, updated_at, last_used_at, layout_json, buffer_guids, agent_pane_session_id)
         SELECT
             COALESCE(NULLIF(json_extract(buffer_guids, '$[0]'), ''), name),
             name, cwd, saved_at, saved_at, layout_json, buffer_guids, agent_pane_session_id
         FROM sessions_legacy;
         DROP TABLE sessions_legacy;
         CREATE INDEX IF NOT EXISTS idx_sessions_updated_at ON sessions(updated_at);
         COMMIT;",
    )
    .context("migrating sessions table to session_id primary key")?;
    Ok(())
}

/// Insert or replace the row for `row.session_id`.
///
/// Returns the buffer GUIDs orphaned by the overwrite — the previous row's
/// GUIDs that the **new** row no longer references — so the caller can unlink
/// only the now-dead scrollback files. Crucially this is a set difference
/// (`old - new`), NOT all of the old GUIDs: a restored tab reuses its pane
/// session GUIDs, so re-saving the same session writes the same
/// `{guid}.txt` files C++ just refreshed; returning those as "orphaned" would
/// delete the freshly-written buffers and leave the row pointing at nothing.
/// Returns an empty vec when there was no prior row (or nothing was dropped).
///
/// Keyed by `session_id` (the anchor pane GUID), NOT the title — so two tabs
/// that share a `name` are stored as distinct rows.
pub fn upsert(conn: &Connection, row: &ShellSessionRow) -> Result<Vec<String>> {
    let previous = get_buffer_guids(conn, &row.session_id)?;
    let kept: std::collections::HashSet<&str> =
        row.buffer_guids.iter().map(String::as_str).collect();
    let orphaned: Vec<String> = previous
        .into_iter()
        .filter(|g| !kept.contains(g.as_str()))
        .collect();

    let guids_json = serde_json::to_string(&row.buffer_guids)
        .context("serializing buffer_guids")?;
    conn.execute(
        "INSERT INTO sessions
             (session_id, name, cwd, updated_at, last_used_at, layout_json, buffer_guids, agent_pane_session_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(session_id) DO UPDATE SET
             name         = excluded.name,
             cwd          = excluded.cwd,
             updated_at   = excluded.updated_at,
             last_used_at = excluded.last_used_at,
             layout_json  = excluded.layout_json,
             buffer_guids = excluded.buffer_guids,
             agent_pane_session_id = excluded.agent_pane_session_id",
        rusqlite::params![
            row.session_id,
            row.name,
            row.cwd,
            row.updated_at,
            row.last_used_at,
            row.layout_json,
            guids_json,
            row.agent_pane_session_id
        ],
    )
    .with_context(|| format!("upserting shell session {}", row.session_id))?;

    Ok(orphaned)
}

/// Mark a session as used **now** without changing its content: bumps
/// `last_used_at` only, so a restored-but-unmodified session survives the TTL.
/// No-op when `session_id` is unknown.
pub fn touch(conn: &Connection, session_id: &str, now: i64) -> Result<()> {
    conn.execute(
        "UPDATE sessions SET last_used_at = ?2 WHERE session_id = ?1",
        rusqlite::params![session_id, now],
    )
    .with_context(|| format!("touching shell session {session_id}"))?;
    Ok(())
}

/// All saved sessions, newest-updated first.
pub fn list(conn: &Connection) -> Result<Vec<ShellSessionRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT session_id, name, cwd, updated_at, last_used_at, layout_json, buffer_guids, agent_pane_session_id
             FROM sessions ORDER BY updated_at DESC",
        )
        .context("preparing shell-session list query")?;
    let rows = stmt
        .query_map([], row_from_sqlite)
        .context("querying shell sessions")?
        .collect::<Result<Vec<_>, _>>()
        .context("collecting shell-session rows")?;
    Ok(rows)
}

/// Delete the row with `session_id` and return its buffer GUIDs, so the caller
/// can unlink the scrollback files for a clean removal. Returns an empty vec
/// when no such row existed (idempotent).
pub fn delete(conn: &Connection, session_id: &str) -> Result<Vec<String>> {
    let orphaned = get_buffer_guids(conn, session_id)?;
    conn.execute("DELETE FROM sessions WHERE session_id = ?1", [session_id])
        .with_context(|| format!("deleting shell session {session_id}"))?;
    Ok(orphaned)
}

/// Delete rows whose last use predates `cutoff_unix_secs` and return the buffer
/// GUIDs of every deleted row, so the caller can unlink their scrollback files.
///
/// TTL is keyed on `last_used_at` (save OR restore), not on the save time — a
/// session you keep restoring won't be swept just because its content is old.
/// Lock-free and safe because master is the only writer: no other process can
/// be mid-write on a row master is deleting.
pub fn ttl_sweep(conn: &Connection, cutoff_unix_secs: i64) -> Result<Vec<String>> {
    let orphaned = {
        let mut stmt = conn
            .prepare("SELECT buffer_guids FROM sessions WHERE last_used_at < ?1")
            .context("preparing TTL select")?;
        let guid_lists = stmt
            .query_map([cutoff_unix_secs], |r| r.get::<_, String>(0))
            .context("querying expired sessions")?
            .collect::<Result<Vec<_>, _>>()
            .context("collecting expired buffer guid lists")?;
        guid_lists
            .iter()
            .flat_map(|json| parse_guids(json))
            .collect::<Vec<_>>()
    };

    conn.execute("DELETE FROM sessions WHERE last_used_at < ?1", [cutoff_unix_secs])
        .context("deleting expired shell sessions")?;

    Ok(orphaned)
}

/// Minimum wall-clock interval between TTL sweeps. master runs the sweep at
/// most once per this window regardless of how many times it restarts in a day
/// (each WT relaunch respawns master), so a user who opens/closes the terminal
/// repeatedly doesn't pay the scan every time.
pub const TTL_SWEEP_MIN_INTERVAL_SECS: i64 = 24 * 60 * 60;

/// Outcome of [`ttl_sweep_if_due`].
#[derive(Debug)]
pub enum TtlSweepOutcome {
    /// The sweep ran; carries the orphaned buffer GUIDs for the caller to
    /// unlink.
    Swept(Vec<String>),
    /// Skipped because the previous sweep was within
    /// [`TTL_SWEEP_MIN_INTERVAL_SECS`].
    Skipped,
}

/// Run [`ttl_sweep`] at most once per [`TTL_SWEEP_MIN_INTERVAL_SECS`].
///
/// Reads the last-sweep timestamp from the `meta` table; if the window hasn't
/// elapsed, returns [`TtlSweepOutcome::Skipped`] without touching the sessions
/// table. Otherwise, deletes rows older than `now - ttl_secs`, records `now` as
/// the new last-sweep time, and returns their orphaned buffer GUIDs. Storing
/// the timestamp in the DB (rather than a separate file) keeps the whole store
/// in one master-owned place.
pub fn ttl_sweep_if_due(conn: &Connection, now: i64, ttl_secs: i64) -> Result<TtlSweepOutcome> {
    if let Some(last) = last_ttl_sweep(conn)? {
        if now.saturating_sub(last) < TTL_SWEEP_MIN_INTERVAL_SECS {
            return Ok(TtlSweepOutcome::Skipped);
        }
    }
    let orphaned = ttl_sweep(conn, now - ttl_secs)?;
    record_ttl_sweep(conn, now)?;
    Ok(TtlSweepOutcome::Swept(orphaned))
}

/// Read the recorded Unix time of the last TTL sweep, or `None` if never swept.
fn last_ttl_sweep(conn: &Connection) -> Result<Option<i64>> {
    conn.query_row(
        "SELECT value FROM meta WHERE key = 'last_ttl_sweep'",
        [],
        |r| r.get::<_, i64>(0),
    )
    .map(Some)
    .or_else(|err| match err {
        rusqlite::Error::QueryReturnedNoRows => Ok(None),
        other => Err(other),
    })
    .context("reading last_ttl_sweep")
}

/// Record `now` as the last-sweep time.
fn record_ttl_sweep(conn: &Connection, now: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES ('last_ttl_sweep', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [now],
    )
    .context("recording last_ttl_sweep")?;
    Ok(())
}

/// Look up the buffer GUIDs currently stored under `session_id` (empty when absent).
fn get_buffer_guids(conn: &Connection, session_id: &str) -> Result<Vec<String>> {
    let json: Option<String> = conn
        .query_row(
            "SELECT buffer_guids FROM sessions WHERE session_id = ?1",
            [session_id],
            |r| r.get(0),
        )
        .or_else(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })
        .with_context(|| format!("looking up buffer guids for {session_id}"))?;
    Ok(json.map(|j| parse_guids(&j)).unwrap_or_default())
}

/// Decode a `buffer_guids` JSON array, tolerating corruption by returning an
/// empty list (a malformed cell must not abort a TTL sweep or list).
fn parse_guids(json: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(json).unwrap_or_default()
}

/// Map one SQLite row to a [`ShellSessionRow`]. Column order must match the
/// SELECTs in [`list`].
fn row_from_sqlite(r: &rusqlite::Row<'_>) -> rusqlite::Result<ShellSessionRow> {
    let buffer_guids: String = r.get(6)?;
    Ok(ShellSessionRow {
        session_id: r.get(0)?,
        name: r.get(1)?,
        cwd: r.get(2)?,
        updated_at: r.get(3)?,
        last_used_at: r.get(4)?,
        layout_json: r.get(5)?,
        buffer_guids: parse_guids(&buffer_guids),
        agent_pane_session_id: r.get(7)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        conn
    }

    // Build a row whose session_id is anchored to the first buffer guid (as C++
    // does), or the name when no guids are given. `ts` seeds both timestamps.
    fn row(name: &str, ts: i64, guids: &[&str]) -> ShellSessionRow {
        let session_id = guids
            .first()
            .map(|s| s.to_string())
            .unwrap_or_else(|| name.to_string());
        row_with_id(&session_id, name, ts, guids)
    }

    // Build a row with an explicit session_id — for tests that re-save the same
    // session with different buffer guids (where the guid can't be the key).
    fn row_with_id(session_id: &str, name: &str, ts: i64, guids: &[&str]) -> ShellSessionRow {
        ShellSessionRow {
            session_id: session_id.to_string(),
            name: name.to_string(),
            cwd: format!("C:\\{name}"),
            updated_at: ts,
            last_used_at: ts,
            layout_json: format!("{{\"tab\":\"{name}\"}}"),
            buffer_guids: guids.iter().map(|s| s.to_string()).collect(),
            agent_pane_session_id: None,
        }
    }

    #[test]
    fn upsert_then_list_newest_first() {
        let conn = mem();
        assert!(upsert(&conn, &row("old", 100, &["g1"])).unwrap().is_empty());
        assert!(upsert(&conn, &row("new", 200, &["g2"])).unwrap().is_empty());

        let all = list(&conn).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].name, "new");
        assert_eq!(all[1].name, "old");
        assert_eq!(all[0].cwd, "C:\\new");
        assert_eq!(all[0].buffer_guids, vec!["g2".to_string()]);
    }

    #[test]
    fn different_tabs_can_share_a_name() {
        // The point of keying on session_id: two tabs both titled "PowerShell"
        // are distinct rows, not one overwriting the other.
        let conn = mem();
        upsert(&conn, &row_with_id("s-1", "PowerShell", 100, &["a"])).unwrap();
        upsert(&conn, &row_with_id("s-2", "PowerShell", 200, &["b"])).unwrap();
        let all = list(&conn).unwrap();
        assert_eq!(all.len(), 2, "same name, different session_id -> two rows");
        assert!(all.iter().all(|r| r.name == "PowerShell"));
    }

    #[test]
    fn upsert_same_session_overwrites_and_returns_orphaned_guids() {
        let conn = mem();
        upsert(&conn, &row_with_id("s", "tab", 100, &["old-a", "old-b"])).unwrap();

        // Same session_id, entirely fresh GUIDs: both old ones are orphaned.
        let orphaned = upsert(&conn, &row_with_id("s", "tab", 200, &["new-a"])).unwrap();
        assert_eq!(orphaned, vec!["old-a".to_string(), "old-b".to_string()]);

        let all = list(&conn).unwrap();
        assert_eq!(all.len(), 1, "same session_id must not create a second row");
        assert_eq!(all[0].updated_at, 200);
        assert_eq!(all[0].buffer_guids, vec!["new-a".to_string()]);
    }

    #[test]
    fn upsert_reusing_same_guids_orphans_nothing() {
        // A restored tab reuses its pane session GUIDs, so re-saving the same
        // session writes the same {guid}.txt files. Those must NOT be reported
        // as orphaned (that would delete the freshly-written buffers).
        let conn = mem();
        upsert(&conn, &row_with_id("s", "tab", 100, &["a", "b"])).unwrap();
        let orphaned = upsert(&conn, &row_with_id("s", "tab", 200, &["a", "b"])).unwrap();
        assert!(orphaned.is_empty(), "re-saving identical guids must orphan nothing");
        assert_eq!(list(&conn).unwrap()[0].buffer_guids, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn upsert_partial_overlap_orphans_only_dropped_guids() {
        let conn = mem();
        upsert(&conn, &row_with_id("s", "tab", 100, &["a", "b"])).unwrap();
        // New row keeps `a`, drops `b`, adds `c` → only `b` is orphaned.
        let orphaned = upsert(&conn, &row_with_id("s", "tab", 200, &["a", "c"])).unwrap();
        assert_eq!(orphaned, vec!["b".to_string()]);
    }

    #[test]
    fn touch_bumps_last_used_at_only_and_list_orders_by_updated_at() {
        let conn = mem();
        upsert(&conn, &row("a", 100, &["ga"])).unwrap();
        upsert(&conn, &row("b", 200, &["gb"])).unwrap();
        // Touch `a` (bumps last_used_at) — updated_at and list order unchanged.
        touch(&conn, "ga", 999).unwrap();
        let all = list(&conn).unwrap();
        assert_eq!(all[0].name, "b", "list sorts by updated_at, not last_used_at");
        let a = all.iter().find(|r| r.name == "a").unwrap();
        assert_eq!(a.updated_at, 100, "touch must not change updated_at");
        assert_eq!(a.last_used_at, 999);
    }

    #[test]
    fn touch_missing_session_is_noop() {
        let conn = mem();
        upsert(&conn, &row("tab", 100, &["g"])).unwrap();
        touch(&conn, "nope", 500).unwrap();
        assert_eq!(list(&conn).unwrap()[0].last_used_at, 100);
    }

    #[test]
    fn ttl_sweep_keys_on_last_used_at() {
        let conn = mem();
        // stale: saved long ago AND never re-used → expired.
        upsert(&conn, &row("stale", 100, &["s1", "s2"])).unwrap();
        // kept: saved long ago but recently *used* (restored) → survives.
        upsert(&conn, &row("kept", 100, &["k1"])).unwrap();
        touch(&conn, "k1", 500).unwrap();

        let mut orphaned = ttl_sweep(&conn, 300).unwrap();
        orphaned.sort();
        assert_eq!(orphaned, vec!["s1".to_string(), "s2".to_string()]);
        let all = list(&conn).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "kept", "recently-used session survives the TTL");
    }

    #[test]
    fn ttl_sweep_deletes_expired_and_returns_their_guids() {
        let conn = mem();
        upsert(&conn, &row("stale", 100, &["s1", "s2"])).unwrap();
        upsert(&conn, &row("fresh", 500, &["f1"])).unwrap();

        // Cutoff 300 → only "stale" (saved_at 100) is expired.
        let mut orphaned = ttl_sweep(&conn, 300).unwrap();
        orphaned.sort();
        assert_eq!(orphaned, vec!["s1".to_string(), "s2".to_string()]);

        let all = list(&conn).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "fresh");
    }

    #[test]
    fn ttl_sweep_noop_when_nothing_expired() {
        let conn = mem();
        upsert(&conn, &row("fresh", 500, &["f1"])).unwrap();
        assert!(ttl_sweep(&conn, 100).unwrap().is_empty());
        assert_eq!(list(&conn).unwrap().len(), 1);
    }

    #[test]
    fn delete_removes_row_and_returns_its_guids() {
        let conn = mem();
        upsert(&conn, &row_with_id("s-keep", "keep", 200, &["k1"])).unwrap();
        upsert(&conn, &row_with_id("s-drop", "drop", 100, &["d1", "d2"])).unwrap();

        let mut guids = delete(&conn, "s-drop").unwrap();
        guids.sort();
        assert_eq!(guids, vec!["d1".to_string(), "d2".to_string()]);

        let all = list(&conn).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "keep");
    }

    #[test]
    fn delete_missing_session_is_noop() {
        let conn = mem();
        upsert(&conn, &row("keep", 200, &["k1"])).unwrap();
        assert!(delete(&conn, "nope").unwrap().is_empty());
        assert_eq!(list(&conn).unwrap().len(), 1);
    }

    #[test]
    fn ttl_sweep_if_due_runs_first_time_then_skips_within_a_day() {
        let conn = mem();
        // saved_at 0 → far older than the 15-day TTL relative to `now`.
        upsert(&conn, &row("stale", 0, &["s1"])).unwrap();
        let ttl = 15 * 24 * 60 * 60;
        let now = 100 * 24 * 60 * 60; // day 100

        // First run: never swept before → sweeps.
        match ttl_sweep_if_due(&conn, now, ttl).unwrap() {
            TtlSweepOutcome::Swept(g) => assert_eq!(g, vec!["s1".to_string()]),
            other => panic!("expected Swept, got {other:?}"),
        }
        assert!(list(&conn).unwrap().is_empty());

        // A second stale row + another call a few hours later: within 24h → skip.
        upsert(&conn, &row("stale2", 0, &["s2"])).unwrap();
        let later = now + 6 * 60 * 60; // +6h
        assert!(matches!(
            ttl_sweep_if_due(&conn, later, ttl).unwrap(),
            TtlSweepOutcome::Skipped
        ));
        assert_eq!(list(&conn).unwrap().len(), 1, "skip must not delete");
    }

    #[test]
    fn ttl_sweep_if_due_runs_again_after_a_day() {
        let conn = mem();
        upsert(&conn, &row("stale", 0, &["s1"])).unwrap();
        let ttl = 15 * 24 * 60 * 60;
        let day0 = 100 * 24 * 60 * 60;

        assert!(matches!(
            ttl_sweep_if_due(&conn, day0, ttl).unwrap(),
            TtlSweepOutcome::Swept(_)
        ));

        // >24h later a newly-expired row is swept on the next due run.
        upsert(&conn, &row("stale2", 0, &["s2"])).unwrap();
        let day2 = day0 + 25 * 60 * 60; // +25h
        match ttl_sweep_if_due(&conn, day2, ttl).unwrap() {
            TtlSweepOutcome::Swept(g) => assert_eq!(g, vec!["s2".to_string()]),
            other => panic!("expected Swept, got {other:?}"),
        }
    }

    #[test]
    fn corrupt_buffer_guids_cell_yields_empty_not_panic() {
        let conn = mem();
        conn.execute(
            "INSERT INTO sessions (session_id, name, cwd, updated_at, last_used_at, layout_json, buffer_guids)
             VALUES ('s', 'bad', '', 1, 1, '{}', 'not-json')",
            [],
        )
        .unwrap();
        let all = list(&conn).unwrap();
        assert_eq!(all.len(), 1);
        assert!(all[0].buffer_guids.is_empty());
    }

    #[test]
    fn upsert_round_trips_agent_pane_session_id() {
        let conn = mem();
        let mut with_agent = row("with-agent", 100, &["g1"]);
        with_agent.agent_pane_session_id = Some("pane-guid-1".to_string());
        upsert(&conn, &with_agent).unwrap();
        upsert(&conn, &row("no-agent", 50, &["g2"])).unwrap();

        let all = list(&conn).unwrap();
        assert_eq!(all[0].name, "with-agent");
        assert_eq!(
            all[0].agent_pane_session_id.as_deref(),
            Some("pane-guid-1")
        );
        assert_eq!(all[1].name, "no-agent");
        assert_eq!(all[1].agent_pane_session_id, None);
    }

    #[test]
    fn migrates_legacy_name_pk_table_to_session_id() {
        // A DB created before the session_id refactor: `name` PRIMARY KEY, a
        // single `saved_at`, no agent column. `init_schema` must rebuild it —
        // deriving session_id from the first buffer guid and seeding both
        // timestamps from `saved_at` — while preserving the row.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                 name         TEXT PRIMARY KEY,
                 cwd          TEXT NOT NULL DEFAULT '',
                 saved_at     INTEGER NOT NULL DEFAULT 0,
                 layout_json  TEXT NOT NULL,
                 buffer_guids TEXT NOT NULL DEFAULT '[]'
             );
             CREATE TABLE meta (key TEXT PRIMARY KEY, value INTEGER NOT NULL);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions (name, cwd, saved_at, layout_json, buffer_guids)
             VALUES ('legacy', 'C:\\x', 42, '{}', '[\"anchor-guid\"]')",
            [],
        )
        .unwrap();

        // Run the full migration (idempotent — must survive a second call).
        init_schema(&conn).unwrap();
        init_schema(&conn).unwrap();

        let all = list(&conn).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].session_id, "anchor-guid", "session_id from first buffer guid");
        assert_eq!(all[0].name, "legacy");
        assert_eq!(all[0].updated_at, 42);
        assert_eq!(all[0].last_used_at, 42);
        assert_eq!(all[0].agent_pane_session_id, None);

        // New upserts work on the migrated table.
        let mut r = row("fresh", 200, &["g1"]);
        r.agent_pane_session_id = Some("pane-2".to_string());
        upsert(&conn, &r).unwrap();
        assert_eq!(list(&conn).unwrap()[0].name, "fresh");
    }
}
