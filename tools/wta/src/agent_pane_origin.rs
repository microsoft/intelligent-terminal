// tools/wta/src/agent_pane_origin.rs
//
// On-disk index of ACP sessions that WTA created on behalf of an
// Intelligent Terminal agent pane.
//
// Why a sidecar file (instead of ACP `_meta` or CLI-specific rename):
//   * ACP `_meta` reaches the agent but agent CLIs (Copilot/Claude/Gemini)
//     are observed not to persist it. So `_meta` cannot survive a restart.
//   * Agent CLIs each generate their own on-disk titles from conversation
//     content; we don't want to interfere with that.
//   * WTA itself owns the moment when a session is created from an agent
//     pane (it's the side that calls ACP `session/new` with `owner_tab_id`
//     in scope), so recording the fact locally is authoritative.
//
// Format
// ------
// JSONL, one record per ACP `session/new` success, appended atomically by
// the OS (`OpenOptions::append`). Records are intentionally small so the
// file stays compact under heavy use:
//
//   v1 (legacy, still readable):
//     {"v":1,"session_id":"<uuid>","origin":"agent_pane","started_at":"<RFC3339-ish>"}
//
//   v2 (legacy, adds `pane_session_id`):
//     {"v":2,"session_id":"<uuid>","origin":"agent_pane","pane_session_id":"<WT pane GUID>","started_at":"<RFC3339-ish>"}
//
//   v3 (current, qualifies the raw ACP id by provider and location):
//     {"v":3,"provider_id":"copilot","location":"Host","session_id":"<uuid>",
//      "origin":"agent_pane","pane_session_id":"<WT pane GUID>","started_at":"<RFC3339-ish>"}
//
// The provider/location pair is master-resolved provenance, not inferred from
// the ACP session id. It prevents equal raw ids reported by different agents
// or WSL distributions from hiding each other in history. Legacy v1/v2 rows
// remain readable through a raw-id compatibility bucket. Duplicates are
// tolerated and last-write wins within the same qualified or legacy key.
// Corrupt lines are skipped without invalidating the rest of the file.
//
// Lifetime
// --------
// The file is append-only; it is never read-then-written from this module.
// Old entries become orphans naturally when the corresponding CLI session
// directory is deleted by the user or the agent CLI itself — orphan entries
// in the index are harmless because the index is only ever consulted as a
// filter against session ids the agent itself still reports.

use std::collections::HashMap;
#[cfg(test)]
use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::time::SystemTime;

const INDEX_FILENAME: &str = "agent-pane-sessions.jsonl";
const SCHEMA_VERSION: u32 = 3;

/// Per-session metadata stored in the index. Identity lives in either the
/// qualified [`crate::session_registry::HistoryRowKey`] map or the legacy
/// raw-id compatibility map. `pane_session_id` is the WT pane GUID that
/// hosted this session; `None` for records written before that field existed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OriginRecord {
    pub pane_session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OriginScope {
    pub provider_id: String,
    pub location: crate::agent_sessions::SessionLocation,
    pub session_universe: Option<String>,
}

impl OriginScope {
    pub fn new(
        provider_id: impl AsRef<str>,
        location: crate::agent_sessions::SessionLocation,
        session_universe: Option<String>,
    ) -> Option<Self> {
        let provider_id = provider_id.as_ref().trim().to_ascii_lowercase();
        if provider_id.is_empty() || !location.is_actionable() {
            return None;
        }
        Some(Self {
            provider_id,
            location,
            session_universe: session_universe
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
        })
    }

    pub fn row_key(&self, session_id: &str) -> Option<crate::session_registry::HistoryRowKey> {
        crate::session_registry::HistoryRowKey::new(
            &self.provider_id,
            self.location.clone(),
            session_id,
            self.session_universe.clone(),
        )
    }
}

#[derive(Debug, Clone, Default)]
pub struct OriginIndex {
    qualified: HashMap<crate::session_registry::HistoryRowKey, OriginRecord>,
    legacy: HashMap<String, OriginRecord>,
}

impl OriginIndex {
    pub fn contains_key(&self, key: &crate::session_registry::HistoryRowKey) -> bool {
        self.qualified.contains_key(key) || self.legacy.contains_key(&key.session_id)
    }

    pub fn contains(
        &self,
        provider_id: &str,
        location: &crate::agent_sessions::SessionLocation,
        session_id: &str,
    ) -> bool {
        crate::session_registry::HistoryRowKey::new(provider_id, location.clone(), session_id, None)
            .is_some_and(|key| self.contains_key(&key))
    }

    pub fn insert_qualified(
        &mut self,
        key: crate::session_registry::HistoryRowKey,
        record: OriginRecord,
    ) {
        self.qualified.insert(key, record);
    }

    #[cfg(test)]
    fn qualified_len(&self) -> usize {
        self.qualified.len()
    }
}

/// Resolve the canonical on-disk location for the index. Returns `None`
/// only if neither `%LOCALAPPDATA%` nor `%APPDATA%` is set, which is
/// extremely unusual on Windows but matches the rest of `runtime_paths`.
pub fn default_index_path() -> Option<PathBuf> {
    crate::runtime_paths::intelligent_terminal_root().map(|root| root.join(INDEX_FILENAME))
}

/// Append an `agent_pane` record for `session_id` to the default index.
/// `pane_session_id` should be the WT pane GUID hosting the session
/// (typically `std::env::var("WT_SESSION")`) — pass `None` only when it
/// is genuinely unavailable.
///
/// Best-effort: any IO error is logged and discarded. The caller must
/// not depend on the write succeeding — a failed append simply means the
/// next history scan won't badge this session, which is graceful
/// degradation rather than breakage.
pub fn append_default(session_id: &str, pane_session_id: Option<&str>) {
    let Some(path) = default_index_path() else {
        tracing::warn!(
            target: "agent_pane_origin",
            session_id = %session_id,
            "skipping append: no runtime root available",
        );
        return;
    };
    if let Err(err) = append_to(&path, session_id, pane_session_id) {
        tracing::warn!(
            target: "agent_pane_origin",
            session_id = %session_id,
            error = %err,
            "failed to append origin record",
        );
    }
}

pub fn append_default_qualified(
    scope: &OriginScope,
    session_id: &str,
    pane_session_id: Option<&str>,
) {
    let Some(path) = default_index_path() else {
        tracing::warn!(
            target: "agent_pane_origin",
            session_id = %session_id,
            "skipping qualified append: no runtime root available",
        );
        return;
    };
    if let Err(err) = append_qualified_to(&path, scope, session_id, pane_session_id) {
        tracing::warn!(
            target: "agent_pane_origin",
            session_id = %session_id,
            error = %err,
            "failed to append qualified origin record",
        );
    }
}

/// Append an `agent_pane` record to a caller-supplied path. Public to
/// support unit tests that exercise round-tripping against a tempdir.
pub fn append_to(
    path: &std::path::Path,
    session_id: &str,
    pane_session_id: Option<&str>,
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    let record = match pane_session_id {
        Some(pane) if !pane.is_empty() => serde_json::json!({
            "v": 2,
            "session_id": session_id,
            "origin": "agent_pane",
            "pane_session_id": pane,
            "started_at": rfc3339_now(),
        }),
        _ => serde_json::json!({
            "v": 2,
            "session_id": session_id,
            "origin": "agent_pane",
            "started_at": rfc3339_now(),
        }),
    };
    writeln!(file, "{}", record)?;
    Ok(())
}

pub fn append_qualified_to(
    path: &std::path::Path,
    scope: &OriginScope,
    session_id: &str,
    pane_session_id: Option<&str>,
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let Some(key) = scope.row_key(session_id) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "origin record requires provider, location, and session id",
        ));
    };
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    let mut record = serde_json::json!({
        "v": SCHEMA_VERSION,
        "session_id": key.session_id,
        "provider_id": key.provider_id,
        "location": key.location,
        "origin": "agent_pane",
        "started_at": rfc3339_now(),
    });
    if let Some(universe) = key.session_universe {
        record["session_universe"] = serde_json::Value::String(universe);
    }
    if let Some(pane) = pane_session_id.filter(|pane| !pane.is_empty()) {
        record["pane_session_id"] = serde_json::Value::String(pane.to_string());
    }
    writeln!(file, "{}", record)?;
    Ok(())
}

pub fn load_default_index() -> OriginIndex {
    let mut out = OriginIndex::default();
    for path in default_index_paths() {
        let next = load_index_from(&path);
        out.qualified.extend(next.qualified);
        out.legacy.extend(next.legacy);
    }
    out
}

/// Load legacy v1/v2 records from `path` into a HashSet. Public for unit tests.
#[cfg(test)]
pub fn load_set_from(path: &std::path::Path) -> HashSet<String> {
    load_records_from(path).into_keys().collect()
}

fn default_index_paths() -> Vec<PathBuf> {
    // Load installed-package indices first (dev/unpackaged only) and the
    // current runtime's index last so current qualified/legacy records win
    // within their own identity bucket.
    let mut paths = Vec::new();
    if crate::runtime_paths::current_package_family_name().is_none() {
        paths.extend(installed_package_index_paths());
    }
    if let Some(path) = default_index_path() {
        paths.push(path);
    }
    paths
}

fn installed_package_index_paths() -> Vec<PathBuf> {
    // Memoize the `%LOCALAPPDATA%\Packages` walk for the process lifetime:
    // `load_default_index` calls this on every routed event in unpackaged/dev mode,
    // and the relevant package directories don't change mid-run.
    static CACHE: std::sync::OnceLock<Vec<PathBuf>> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(installed_package_index_paths_uncached)
        .clone()
}

fn installed_package_index_paths_uncached() -> Vec<PathBuf> {
    let Some(local) = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("APPDATA"))
        .map(PathBuf::from)
    else {
        return Vec::new();
    };
    let packages = local.join("Packages");
    let Ok(entries) = std::fs::read_dir(packages) else {
        return Vec::new();
    };
    let mut dev = Vec::new();
    let mut store = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let path = entry
            .path()
            .join("LocalState")
            .join("IntelligentTerminal")
            .join(INDEX_FILENAME);
        if !path.exists() {
            continue;
        }
        if name.starts_with("IntelligentTerminal_") {
            dev.push(path);
        } else if name.starts_with("Microsoft.IntelligentTerminal_") {
            store.push(path);
        }
    }
    dev.extend(store);
    dev
}

/// Load the legacy compatibility bucket from a caller-supplied path.
#[cfg(test)]
pub fn load_records_from(path: &std::path::Path) -> HashMap<String, OriginRecord> {
    load_index_from(path).legacy
}

pub fn load_index_from(path: &std::path::Path) -> OriginIndex {
    let mut out = OriginIndex::default();
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return out, // most commonly: file does not exist yet
    };
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let parsed: Result<serde_json::Value, _> = serde_json::from_str(trimmed);
        let Ok(value) = parsed else { continue }; // skip corrupt line
        let Some(id) = value.get("session_id").and_then(|v| v.as_str()) else {
            continue;
        };
        if id.is_empty() {
            continue;
        }
        let pane_session_id = value
            .get("pane_session_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let record = OriginRecord { pane_session_id };
        let version = value.get("v").and_then(|v| v.as_u64()).unwrap_or(1);
        if version < SCHEMA_VERSION as u64 {
            out.legacy.insert(id.to_string(), record);
            continue;
        }
        let qualified = value
            .get("provider_id")
            .and_then(|v| v.as_str())
            .zip(value.get("location"))
            .and_then(|(provider_id, location)| {
                serde_json::from_value::<crate::agent_sessions::SessionLocation>(location.clone())
                    .ok()
                    .and_then(|location| {
                        crate::session_registry::HistoryRowKey::new(
                            provider_id,
                            location,
                            id,
                            value
                                .get("session_universe")
                                .and_then(|v| v.as_str())
                                .map(str::to_string),
                        )
                    })
            });
        if let Some(key) = qualified {
            out.qualified.insert(key, record);
        }
    }
    out
}

fn rfc3339_now() -> String {
    // Tiny RFC3339 emitter — we don't pull in chrono just for this. The
    // exact format is unspecified by callers (the index is for our own
    // consumption); a sortable UTC timestamp is enough for `tail -f`
    // debugging.
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // YYYY-MM-DDTHH:MM:SSZ via simple integer math (UTC). Years 1970-2099
    // suffice for our lifetime.
    let (y, mo, d, h, mi, s) = unix_secs_to_ymdhms(secs);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, mo, d, h, mi, s)
}

fn unix_secs_to_ymdhms(secs: u64) -> (u32, u32, u32, u32, u32, u32) {
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let h = (rem / 3600) as u32;
    let mi = ((rem % 3600) / 60) as u32;
    let s = (rem % 60) as u32;

    // Days since 1970-01-01 → calendar date (Gregorian).
    let mut year: u32 = 1970;
    let mut days_left = days as i64;
    loop {
        let dy = if is_leap_year(year) { 366 } else { 365 };
        if days_left < dy {
            break;
        }
        days_left -= dy;
        year += 1;
    }
    let months: [u32; 12] = if is_leap_year(year) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let mut month: u32 = 1;
    for &dm in &months {
        if days_left < dm as i64 {
            break;
        }
        days_left -= dm as i64;
        month += 1;
    }
    let day = (days_left as u32) + 1;
    (year, month, day, h, mi, s)
}

fn is_leap_year(y: u32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_index_path(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("wta-agent-pane-origin-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{}-{}.jsonl", label, std::process::id()));
        let _ = std::fs::remove_file(&path);
        path
    }

    #[test]
    fn append_then_load_roundtrip() {
        let path = tmp_index_path("roundtrip");
        append_to(&path, "abc-123", None).unwrap();
        append_to(&path, "def-456", Some("pane-xyz")).unwrap();
        let set = load_set_from(&path);
        assert!(set.contains("abc-123"));
        assert!(set.contains("def-456"));
        assert_eq!(set.len(), 2);

        let records = load_records_from(&path);
        assert_eq!(
            records
                .get("abc-123")
                .and_then(|r| r.pane_session_id.as_deref()),
            None
        );
        assert_eq!(
            records
                .get("def-456")
                .and_then(|r| r.pane_session_id.as_deref()),
            Some("pane-xyz")
        );
    }

    #[test]
    fn qualified_records_keep_same_raw_id_separate() {
        let path = tmp_index_path("qualified-collision");
        let host = OriginScope::new(
            "copilot",
            crate::agent_sessions::SessionLocation::Host,
            None,
        )
        .unwrap();
        let wsl = OriginScope::new(
            "copilot",
            crate::agent_sessions::SessionLocation::Wsl {
                distro: "Ubuntu".to_string(),
            },
            None,
        )
        .unwrap();
        append_qualified_to(&path, &host, "same-id", Some("pane-host")).unwrap();
        append_qualified_to(&path, &wsl, "same-id", Some("pane-wsl")).unwrap();

        let index = load_index_from(&path);
        assert_eq!(index.qualified_len(), 2);
        assert!(index.contains(
            "copilot",
            &crate::agent_sessions::SessionLocation::Host,
            "same-id"
        ));
        assert!(index.contains(
            "copilot",
            &crate::agent_sessions::SessionLocation::Wsl {
                distro: "Ubuntu".to_string(),
            },
            "same-id"
        ));
        assert!(!index.contains(
            "claude",
            &crate::agent_sessions::SessionLocation::Host,
            "same-id"
        ));
        assert!(
            load_records_from(&path).is_empty(),
            "qualified records must never enter the raw compatibility map"
        );
    }

    #[test]
    fn qualified_record_round_trips_provider_and_location() {
        let path = tmp_index_path("qualified-roundtrip");
        let scope = OriginScope::new(
            "Claude",
            crate::agent_sessions::SessionLocation::Host,
            Some("tenant-a".to_string()),
        )
        .unwrap();
        append_qualified_to(&path, &scope, "session-1", None).unwrap();

        let index = load_index_from(&path);
        let key = crate::session_registry::HistoryRowKey::new(
            "claude",
            crate::agent_sessions::SessionLocation::Host,
            "session-1",
            Some("tenant-a".to_string()),
        )
        .unwrap();
        assert!(index.qualified.contains_key(&key));
        assert!(index.contains_key(&key));
        let other_universe = crate::session_registry::HistoryRowKey::new(
            "claude",
            crate::agent_sessions::SessionLocation::Host,
            "session-1",
            Some("tenant-b".to_string()),
        )
        .unwrap();
        assert!(!index.contains_key(&other_universe));
    }

    #[test]
    fn malformed_v3_record_does_not_fall_back_to_legacy_raw_id() {
        let path = tmp_index_path("malformed-v3");
        std::fs::write(
            &path,
            "{\"v\":3,\"session_id\":\"same-id\",\"provider_id\":\"copilot\",\"origin\":\"agent_pane\"}\n",
        )
        .unwrap();

        let index = load_index_from(&path);
        assert!(!index.contains(
            "copilot",
            &crate::agent_sessions::SessionLocation::Host,
            "same-id"
        ));
        assert!(load_records_from(&path).is_empty());
    }

    #[test]
    fn duplicate_appends_collapse_in_set() {
        let path = tmp_index_path("dup");
        append_to(&path, "same-id", None).unwrap();
        append_to(&path, "same-id", None).unwrap();
        append_to(&path, "same-id", None).unwrap();
        let set = load_set_from(&path);
        assert_eq!(set.len(), 1);
        assert!(set.contains("same-id"));
    }

    #[test]
    fn duplicate_appends_last_pane_wins_in_records() {
        // If the same session_id is written twice with different
        // pane_session_id, the latest pane wins. Defensive: not expected
        // in practice but keeps the contract simple.
        let path = tmp_index_path("dup-pane");
        append_to(&path, "same-id", Some("pane-old")).unwrap();
        append_to(&path, "same-id", Some("pane-new")).unwrap();
        let records = load_records_from(&path);
        assert_eq!(records.len(), 1);
        assert_eq!(
            records
                .get("same-id")
                .and_then(|r| r.pane_session_id.as_deref()),
            Some("pane-new")
        );
    }

    #[test]
    fn v1_entries_still_load_with_no_pane() {
        // Backward-compat: a pre-v2 record (no pane_session_id field)
        // must still appear in load_records_from with pane_session_id = None.
        let path = tmp_index_path("v1-compat");
        std::fs::write(
            &path,
            "{\"v\":1,\"session_id\":\"legacy-001\",\"origin\":\"agent_pane\",\"started_at\":\"2024-01-01T00:00:00Z\"}\n",
        )
        .unwrap();
        let records = load_records_from(&path);
        let rec = records.get("legacy-001").expect("v1 entry must still load");
        assert_eq!(rec.pane_session_id, None);
    }

    #[test]
    fn missing_file_yields_empty_set() {
        let path = std::env::temp_dir().join("does-not-exist-9f8d3c2.jsonl");
        let _ = std::fs::remove_file(&path);
        let set = load_set_from(&path);
        assert!(set.is_empty());
    }

    #[test]
    fn corrupt_lines_are_skipped() {
        let path = tmp_index_path("corrupt");
        // Pre-seed with garbage + a valid record + more garbage.
        std::fs::write(
            &path,
            "this is not json\n\
             {\"v\":1,\"session_id\":\"good-1\",\"origin\":\"agent_pane\"}\n\
             {malformed\n\
             \n\
             {\"v\":2,\"session_id\":\"good-2\",\"pane_session_id\":\"pane-2\"}\n",
        )
        .unwrap();
        let set = load_set_from(&path);
        assert!(set.contains("good-1"));
        assert!(set.contains("good-2"));
        assert_eq!(set.len(), 2);
        let records = load_records_from(&path);
        assert_eq!(
            records
                .get("good-1")
                .and_then(|r| r.pane_session_id.as_deref()),
            None
        );
        assert_eq!(
            records
                .get("good-2")
                .and_then(|r| r.pane_session_id.as_deref()),
            Some("pane-2")
        );
    }

    #[test]
    fn empty_session_id_is_ignored() {
        let path = tmp_index_path("empty-id");
        std::fs::write(
            &path,
            "{\"v\":1,\"session_id\":\"\",\"origin\":\"agent_pane\"}\n\
             {\"v\":1,\"origin\":\"agent_pane\"}\n",
        )
        .unwrap();
        let set = load_set_from(&path);
        assert!(set.is_empty());
    }

    #[test]
    fn empty_pane_session_id_is_treated_as_none() {
        let path = tmp_index_path("empty-pane");
        std::fs::write(
            &path,
            "{\"v\":2,\"session_id\":\"abc\",\"pane_session_id\":\"\",\"origin\":\"agent_pane\"}\n",
        )
        .unwrap();
        let records = load_records_from(&path);
        assert_eq!(
            records
                .get("abc")
                .and_then(|r| r.pane_session_id.as_deref()),
            None
        );
    }

    #[test]
    fn rfc3339_now_has_expected_shape() {
        let s = rfc3339_now();
        assert_eq!(s.len(), 20, "expected YYYY-MM-DDTHH:MM:SSZ: {:?}", s);
        assert!(s.ends_with('Z'), "expected trailing Z: {:?}", s);
        assert_eq!(&s[4..5], "-");
        assert_eq!(&s[10..11], "T");
    }

    #[test]
    fn ymdhms_known_dates() {
        // 1779393382 in UTC is 2026-05-21T19:56:22Z; the local time observed
        // in wta-main.log (12:56:22 local) maps to the same UTC instant
        // (PDT = UTC-7 in May).
        let secs = 1_779_393_382;
        let (y, mo, d, h, mi, s) = unix_secs_to_ymdhms(secs);
        assert_eq!((y, mo, d, h, mi, s), (2026, 5, 21, 19, 56, 22));
        // Unix epoch sanity.
        let (y, mo, d, h, mi, s) = unix_secs_to_ymdhms(0);
        assert_eq!((y, mo, d, h, mi, s), (1970, 1, 1, 0, 0, 0));
        // Leap-year boundary: 2024-02-29T00:00:00Z = 1709164800.
        let (y, mo, d, ..) = unix_secs_to_ymdhms(1_709_164_800);
        assert_eq!((y, mo, d), (2024, 2, 29));
    }
}
