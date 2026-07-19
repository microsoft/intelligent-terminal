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
             buffer_guids TEXT NOT NULL DEFAULT '[]'
         );
         CREATE INDEX IF NOT EXISTS idx_sessions_saved_at ON sessions(saved_at);
         CREATE TABLE IF NOT EXISTS meta (
             key   TEXT PRIMARY KEY,
             value INTEGER NOT NULL
         );",
    )
    .context("initializing shell-session schema")?;
    Ok(())
}

/// Insert or replace the row for `row.name`.
///
/// Returns the buffer GUIDs of any **superseded** row with the same name, so
/// the caller can unlink the now-orphaned scrollback files (each tab close
/// mints fresh per-connection GUIDs, so an overwrite orphans the old files).
/// Returns an empty vec when there was no prior row.
pub fn upsert(conn: &Connection, row: &ShellSessionRow) -> Result<Vec<String>> {
    let orphaned = get_buffer_guids(conn, &row.name)?;

    let guids_json = serde_json::to_string(&row.buffer_guids)
        .context("serializing buffer_guids")?;
    conn.execute(
        "INSERT INTO sessions (name, cwd, saved_at, layout_json, buffer_guids)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(name) DO UPDATE SET
             cwd          = excluded.cwd,
             saved_at     = excluded.saved_at,
             layout_json  = excluded.layout_json,
             buffer_guids = excluded.buffer_guids",
        rusqlite::params![row.name, row.cwd, row.saved_at, row.layout_json, guids_json],
    )
    .with_context(|| format!("upserting shell session {}", row.name))?;

    Ok(orphaned)
}

/// All saved sessions, newest first.
pub fn list(conn: &Connection) -> Result<Vec<ShellSessionRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT name, cwd, saved_at, layout_json, buffer_guids
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
/// table. Otherwise deletes rows older than `now - ttl_secs`, records `now` as
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

        // Re-close the same-named tab: fresh GUIDs, old ones orphaned.
        let orphaned = upsert(&conn, &row("tab", 200, &["new-a"])).unwrap();
        assert_eq!(orphaned, vec!["old-a".to_string(), "old-b".to_string()]);

        let all = list(&conn).unwrap();
        assert_eq!(all.len(), 1, "same name must not create a second row");
        assert_eq!(all[0].saved_at, 200);
        assert_eq!(all[0].buffer_guids, vec!["new-a".to_string()]);
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
}
