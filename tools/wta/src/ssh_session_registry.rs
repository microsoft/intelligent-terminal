//! Private helper/master protocol for source-scoped SSH session history and resume.

use agent_client_protocol as acp;
use serde::{Deserialize, Serialize};

pub const METHOD: &str = "_intellterm.wta/ssh_sessions";
pub const CHANGED_METHOD: &str = "_intellterm.wta/ssh_sessions/changed";

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub target: crate::ssh_sessions::SshTarget,
    pub agent_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    /// Read already-known rows only. Unlike List's first read, this never
    /// discovers history, connects SSH, or starts an agent CLI.
    Snapshot {
        source: Source,
    },
    /// Return cached state immediately and refresh remote history in the
    /// background when this source's shared refresh interval has elapsed.
    Poll {
        source: Source,
    },
    List {
        source: Source,
        refresh_history: bool,
    },
    Activate {
        source: Source,
        session_id: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub source: Source,
    #[serde(with = "epoch_serde")]
    pub epoch: uuid::Uuid,
    pub revision: u64,
    pub sessions: Vec<crate::session_registry::SessionInfo>,
}

pub(crate) fn parse_request(
    raw: &serde_json::value::RawValue,
) -> Result<Request, serde_json::Error> {
    serde_json::from_str(raw.get())
}

pub(crate) fn build_changed_notification(source: &Source) -> acp::schema::v1::ExtNotification {
    let raw = serde_json::value::to_raw_value(source)
        .expect("validated SSH source is trivially serializable");
    acp::schema::v1::ExtNotification::new(CHANGED_METHOD, raw.into())
}

// Keep the existing uuid dependency features unchanged; the wire epoch is a
// canonical UUID string, not a platform-dependent byte representation.
mod epoch_serde {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(epoch: &uuid::Uuid, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(epoch)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<uuid::Uuid, D::Error> {
        let text = String::deserialize(deserializer)?;
        uuid::Uuid::parse_str(&text).map_err(serde::de::Error::custom)
    }
}
