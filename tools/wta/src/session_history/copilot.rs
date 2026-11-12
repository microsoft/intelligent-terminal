//! Positive evidence for closed Copilot startup sessions, not a title heuristic.

use crate::agent_sessions::{CliSource, SessionLocation};
use agent_client_protocol::schema::v1::SessionInfo;
use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};

const MAX_STARTUP_BYTES: u64 = 32 * 1024;

pub(crate) fn session_root() -> Option<PathBuf> {
    let home = std::env::var_os("COPILOT_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join(".copilot"))
        })?;
    home.is_absolute().then(|| home.join("session-state"))
}

pub(crate) fn closed_startup_ids(
    sessions: &[SessionInfo],
    location: &SessionLocation,
    cli: &CliSource,
) -> HashSet<String> {
    if *cli != CliSource::Copilot || *location != SessionLocation::Host {
        return HashSet::new();
    }
    let Some(root) = session_root() else {
        return HashSet::new();
    };
    closed_startup_ids_in(sessions, &root)
}

fn closed_startup_ids_in(sessions: &[SessionInfo], root: &Path) -> HashSet<String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(250);
    sessions
        .iter()
        .filter(|session| session.title.as_deref().is_none_or(str::is_empty))
        .take_while(|_| std::time::Instant::now() < deadline)
        .filter_map(|session| {
            let id = session.session_id.to_string();
            if uuid::Uuid::parse_str(&id).is_err() {
                return None;
            }
            match is_closed_startup(root, &id) {
                Ok(true) => Some(id),
                Ok(false) => None,
                Err(error) => {
                    tracing::debug!(target: "master_history", %error,
                        "startup evidence unavailable; retaining untitled Copilot session");
                    None
                }
            }
        })
        .collect()
}

fn is_closed_startup(root: &Path, id: &str) -> std::io::Result<bool> {
    let root = root.canonicalize()?;
    let directory = root.join(id).canonicalize()?;
    if directory.parent() != Some(root.as_path()) {
        return Ok(false);
    }
    // Even an unused session may still be open in a CLI outside IT. A stale
    // marker is also retained: uncertainty must never hide a real session.
    for (index, entry) in std::fs::read_dir(&directory)?.enumerate() {
        let entry = entry?;
        if index >= 64 || entry.file_name().to_string_lossy().starts_with("inuse.") {
            return Ok(false);
        }
    }
    let mut bytes = Vec::new();
    std::fs::File::open(directory.join("events.jsonl"))?
        .take(MAX_STARTUP_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_STARTUP_BYTES {
        return Ok(false);
    }
    Ok(startup_only_transcript(&bytes, id))
}

fn startup_only_transcript(bytes: &[u8], id: &str) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let mut started = false;
    let mut stopped = false;
    for line in text.lines() {
        let Ok(record) = serde_json::from_str::<serde_json::Value>(line) else {
            return false;
        };
        if stopped {
            return false;
        }
        match record.get("type").and_then(serde_json::Value::as_str) {
            Some("session.start") if !started => {
                if record.pointer("/data/sessionId").and_then(|v| v.as_str()) != Some(id)
                    || record.pointer("/data/producer").and_then(|v| v.as_str())
                        != Some("copilot-agent")
                {
                    return false;
                }
                started = true;
            }
            Some("session.model_change") if started => {}
            Some("hook.start" | "hook.end") if started => {
                if !matches!(
                    record.pointer("/data/hookType").and_then(|v| v.as_str()),
                    Some("sessionStart" | "sessionEnd")
                ) {
                    return false;
                }
            }
            Some("session.shutdown") if started => {
                stopped = record
                    .pointer("/data/shutdownType")
                    .and_then(|v| v.as_str())
                    == Some("routine")
                    && record
                        .pointer("/data/totalApiDurationMs")
                        .and_then(|v| v.as_u64())
                        == Some(0)
                    && record
                        .pointer("/data/conversationTokens")
                        .and_then(|v| v.as_u64())
                        == Some(0);
                if !stopped {
                    return false;
                }
            }
            // Messages, resume/fork records, tools, and future formats are not
            // evidence of an unused startup session.
            _ => return false,
        }
    }
    stopped
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "9a7a4fd9-2112-4c64-bb33-fec1384d4a3e";

    fn transcript(extra: &str) -> String {
        format!(
            "{{\"type\":\"session.start\",\"data\":{{\"sessionId\":\"{ID}\",\"producer\":\"copilot-agent\"}}}}\n\
             {{\"type\":\"session.model_change\",\"data\":{{}}}}\n\
             {{\"type\":\"hook.start\",\"data\":{{\"hookType\":\"sessionEnd\"}}}}\n\
             {{\"type\":\"hook.end\",\"data\":{{\"hookType\":\"sessionEnd\"}}}}\n\
             {extra}\
             {{\"type\":\"session.shutdown\",\"data\":{{\"shutdownType\":\"routine\",\"totalApiDurationMs\":0,\"conversationTokens\":0}}}}\n"
        )
    }

    #[test]
    fn closed_startup_requires_complete_matching_lifecycle() {
        let text = transcript("");
        assert!(startup_only_transcript(text.as_bytes(), ID));
        assert!(!startup_only_transcript(text.as_bytes(), "another-session"));
        let shutdown = text.rfind("{\"type\":\"session.shutdown\"").unwrap();
        assert!(!startup_only_transcript(text[..shutdown].as_bytes(), ID));
        assert!(!startup_only_transcript(b"", ID));
        for modified in [
            text.replace("\"routine\"", "\"error\""),
            text.replace("\"totalApiDurationMs\":0", "\"totalApiDurationMs\":1"),
            text.replace("\"conversationTokens\":0", "\"conversationTokens\":1"),
            text.replace("\"conversationTokens\":0", "\"unknownField\":0"),
            text.replace("\"sessionEnd\"", "\"userPromptSubmitted\""),
            format!("{text}{{\"type\":\"session.resume\"}}\n"),
            format!("{text}{{"),
        ] {
            assert!(!startup_only_transcript(modified.as_bytes(), ID));
        }
    }

    #[test]
    fn untitled_conversations_and_unknown_records_are_not_startup_only() {
        for kind in [
            "user.message",
            "assistant.message",
            "assistant.turn_start",
            "tool.execution_start",
            "session.resume",
            "session.fork",
            "session.title_changed",
            "future.event",
        ] {
            let text = transcript(&format!("{{\"type\":\"{kind}\",\"data\":{{}}}}\n"));
            assert!(!startup_only_transcript(text.as_bytes(), ID), "{kind}");
        }
    }

    #[test]
    fn history_exclusion_preserves_named_missing_and_large_sessions() {
        let root = std::env::current_dir()
            .unwrap()
            .join("Generated Files")
            .join(format!("copilot-startup-{}", uuid::Uuid::new_v4()));
        let directory = root.join(ID);
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("events.jsonl");
        let mut session = SessionInfo::new(ID, PathBuf::from("C:\\project"));
        std::fs::write(&path, transcript("")).unwrap();
        assert_eq!(
            closed_startup_ids_in(&[session.clone()], &root),
            HashSet::from([ID.to_string()])
        );
        session.title = Some("A deliberately named empty session".into());
        assert!(closed_startup_ids_in(&[session.clone()], &root).is_empty());
        session.title = None;
        std::fs::write(directory.join("inuse.42.lock"), b"42").unwrap();
        assert!(closed_startup_ids_in(&[session.clone()], &root).is_empty());
        std::fs::remove_file(directory.join("inuse.42.lock")).unwrap();
        std::fs::write(
            &path,
            transcript("{\"type\":\"user.message\",\"data\":{}}\n"),
        )
        .unwrap();
        assert!(closed_startup_ids_in(&[session.clone()], &root).is_empty());
        std::fs::write(&path, vec![b' '; MAX_STARTUP_BYTES as usize + 1]).unwrap();
        assert!(closed_startup_ids_in(&[session.clone()], &root).is_empty());
        std::fs::remove_file(&path).unwrap();
        assert!(closed_startup_ids_in(&[session], &root).is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn startup_rule_never_reads_host_files_for_other_sources() {
        let sessions = [SessionInfo::new(ID, PathBuf::new())];
        for cli in [
            CliSource::Claude,
            CliSource::Codex,
            CliSource::Gemini,
            CliSource::OpenCode,
            CliSource::Antigravity,
            CliSource::Unknown("custom:agent".into()),
        ] {
            assert!(closed_startup_ids(&sessions, &SessionLocation::Host, &cli).is_empty());
        }
        for location in [
            SessionLocation::Unknown,
            SessionLocation::Wsl {
                distro: "Ubuntu".into(),
            },
        ] {
            assert!(closed_startup_ids(&sessions, &location, &CliSource::Copilot).is_empty());
        }
    }
}
