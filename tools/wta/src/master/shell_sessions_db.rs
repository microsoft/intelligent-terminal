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
//   * SQLite row  = { name (PK), cwd, saved_at, layout_json, buffer_guids }.
//     The authoritative index + WT layout JSON + the list of scrollback file
//     GUIDs that belong to this session.
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
    /// The closed tab's title — the session key (upsert overwrites same-name).
    pub name: String,
    /// Working directory captured at save time (display only; may be empty).
    pub cwd: String,
    /// Unix seconds when saved. Used to sort newest-first and for TTL.
    pub saved_at: i64,
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
             name         TEXT PRIMARY KEY,
             cwd          TEXT NOT NULL DEFAULT '',
             saved_at     INTEGER NOT NULL DEFAULT 0,
             layout_json  TEXT NOT NULL,
             buffer_guids TEXT NOT NULL DEFAULT '[]',
             agent_pane_session_id TEXT
         );
         CREATE INDEX IF NOT EXISTS idx_sessions_saved_at ON sessions(saved_at);
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

/// Insert or replace the row for `row.name`.
///
/// Returns the buffer GUIDs orphaned by the overwrite — the previous row's
/// GUIDs that the **new** row no longer references — so the caller can unlink
/// only the now-dead scrollback files. Crucially this is a set difference
/// (`old - new`), NOT all of the old GUIDs: a restored tab reuses its pane
/// session GUIDs, so re-saving the same session writes the same
/// `{guid}.txt` files C++ just refreshed; returning those as "orphaned" would
/// delete the freshly-written buffers and leave the row pointing at nothing.
/// Returns an empty vec when there was no prior row (or nothing was dropped).
pub fn upsert(conn: &Connection, row: &ShellSessionRow) -> Result<Vec<String>> {
    let previous = get_buffer_guids(conn, &row.name)?;
    let kept: std::collections::HashSet<&str> =
        row.buffer_guids.iter().map(String::as_str).collect();
    let orphaned: Vec<String> = previous
        .into_iter()
        .filter(|g| !kept.contains(g.as_str()))
        .collect();

    let guids_json = serde_json::to_string(&row.buffer_guids)
        .context("serializing buffer_guids")?;
    conn.execute(
        "INSERT INTO sessions (name, cwd, saved_at, layout_json, buffer_guids, agent_pane_session_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(name) DO UPDATE SET
             cwd          = excluded.cwd,
             saved_at     = excluded.saved_at,
             layout_json  = excluded.layout_json,
             buffer_guids = excluded.buffer_guids,
             agent_pane_session_id = excluded.agent_pane_session_id",
        rusqlite::params![
            row.name,
            row.cwd,
            row.saved_at,
            row.layout_json,
            guids_json,
            row.agent_pane_session_id
        ],
    )
    .with_context(|| format!("upserting shell session {}", row.name))?;

    Ok(orphaned)
}

/// All saved sessions, newest first.
pub fn list(conn: &Connection) -> Result<Vec<ShellSessionRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT name, cwd, saved_at, layout_json, buffer_guids, agent_pane_session_id
             FROM sessions ORDER BY saved_at DESC",
        )
        .context("preparing shell-session list query")?;
    let rows = stmt
        .query_map([], row_from_sqlite)
        .context("querying shell sessions")?
        .collect::<Result<Vec<_>, _>>()
        .context("collecting shell-session rows")?;
    Ok(rows)
}

/// Delete the row named `name` and return its buffer GUIDs, so the caller can
/// unlink the scrollback files for a clean removal. Returns an empty vec when
/// no such row existed (idempotent).
pub fn delete(conn: &Connection, name: &str) -> Result<Vec<String>> {
    let orphaned = get_buffer_guids(conn, name)?;
    conn.execute("DELETE FROM sessions WHERE name = ?1", [name])
        .with_context(|| format!("deleting shell session {name}"))?;
    Ok(orphaned)
}

/// Delete rows saved before `cutoff_unix_secs` and return the buffer GUIDs of
/// every deleted row, so the caller can unlink their scrollback files.
///
/// Lock-free and safe because master is the only writer: no other process can
/// be mid-write on a row master is deleting.
pub fn ttl_sweep(conn: &Connection, cutoff_unix_secs: i64) -> Result<Vec<String>> {
    let orphaned = {
        let mut stmt = conn
            .prepare("SELECT buffer_guids FROM sessions WHERE saved_at < ?1")
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

    conn.execute("DELETE FROM sessions WHERE saved_at < ?1", [cutoff_unix_secs])
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

/// Look up the buffer GUIDs currently stored under `name` (empty when absent).
fn get_buffer_guids(conn: &Connection, name: &str) -> Result<Vec<String>> {
    let json: Option<String> = conn
        .query_row(
            "SELECT buffer_guids FROM sessions WHERE name = ?1",
            [name],
            |r| r.get(0),
        )
        .or_else(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })
        .with_context(|| format!("looking up buffer guids for {name}"))?;
    Ok(json.map(|j| parse_guids(&j)).unwrap_or_default())
}

/// Decode a `buffer_guids` JSON array, tolerating corruption by returning an
/// empty list (a malformed cell must not abort a TTL sweep or list).
fn parse_guids(json: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(json).unwrap_or_default()
}

/// Map one SQLite row to a [`ShellSessionRow`].
fn row_from_sqlite(r: &rusqlite::Row<'_>) -> rusqlite::Result<ShellSessionRow> {
    let buffer_guids: String = r.get(4)?;
    Ok(ShellSessionRow {
        name: r.get(0)?,
        cwd: r.get(1)?,
        saved_at: r.get(2)?,
        layout_json: r.get(3)?,
        buffer_guids: parse_guids(&buffer_guids),
        agent_pane_session_id: r.get(5)?,
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

    fn row(name: &str, saved_at: i64, guids: &[&str]) -> ShellSessionRow {
        ShellSessionRow {
            name: name.to_string(),
            cwd: format!("C:\\{name}"),
            saved_at,
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
    fn upsert_same_name_overwrites_and_returns_orphaned_guids() {
        let conn = mem();
        upsert(&conn, &row("tab", 100, &["old-a", "old-b"])).unwrap();

        // Re-close the same-named tab with entirely fresh GUIDs: both old ones
        // are orphaned (not referenced by the new row).
        let orphaned = upsert(&conn, &row("tab", 200, &["new-a"])).unwrap();
        assert_eq!(orphaned, vec!["old-a".to_string(), "old-b".to_string()]);

        let all = list(&conn).unwrap();
        assert_eq!(all.len(), 1, "same name must not create a second row");
        assert_eq!(all[0].saved_at, 200);
        assert_eq!(all[0].buffer_guids, vec!["new-a".to_string()]);
    }

    #[test]
    fn upsert_reusing_same_guids_orphans_nothing() {
        // A restored tab reuses its pane session GUIDs, so re-saving the same
        // session writes the same {guid}.txt files. Those must NOT be reported
        // as orphaned (that would delete the freshly-written buffers).
        let conn = mem();
        upsert(&conn, &row("tab", 100, &["a", "b"])).unwrap();
        let orphaned = upsert(&conn, &row("tab", 200, &["a", "b"])).unwrap();
        assert!(orphaned.is_empty(), "re-saving identical guids must orphan nothing");
        assert_eq!(list(&conn).unwrap()[0].buffer_guids, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn upsert_partial_overlap_orphans_only_dropped_guids() {
        let conn = mem();
        upsert(&conn, &row("tab", 100, &["a", "b"])).unwrap();
        // New row keeps `a`, drops `b`, adds `c` → only `b` is orphaned.
        let orphaned = upsert(&conn, &row("tab", 200, &["a", "c"])).unwrap();
        assert_eq!(orphaned, vec!["b".to_string()]);
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
        upsert(&conn, &row("keep", 200, &["k1"])).unwrap();
        upsert(&conn, &row("drop", 100, &["d1", "d2"])).unwrap();

        let mut guids = delete(&conn, "drop").unwrap();
        guids.sort();
        assert_eq!(guids, vec!["d1".to_string(), "d2".to_string()]);

        let all = list(&conn).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "keep");
    }

    #[test]
    fn delete_missing_name_is_noop() {
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
            "INSERT INTO sessions (name, cwd, saved_at, layout_json, buffer_guids)
             VALUES ('bad', '', 1, '{}', 'not-json')",
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
    fn ensure_agent_pane_session_id_column_migrates_legacy_table() {
        // Simulate a DB created before the column existed, then run the
        // migration path (init_schema calls it) and confirm reads/writes work.
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
             VALUES ('legacy', '', 1, '{}', '[]')",
            [],
        )
        .unwrap();

        // Idempotent: running the migration twice must not error.
        ensure_agent_pane_session_id_column(&conn).unwrap();
        ensure_agent_pane_session_id_column(&conn).unwrap();

        // The preexisting row reads back with a NULL (None) agent id.
        let all = list(&conn).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].agent_pane_session_id, None);

        // And new upserts can now store an agent id.
        let mut r = row("fresh", 200, &["g1"]);
        r.agent_pane_session_id = Some("pane-2".to_string());
        upsert(&conn, &r).unwrap();
        let all = list(&conn).unwrap();
        assert_eq!(all[0].name, "fresh");
        assert_eq!(all[0].agent_pane_session_id.as_deref(), Some("pane-2"));
    }
}
