//! Ordinary-SSH hook routing and the bounded v3 wire boundary.
//! Native pane identity comes only from a locally registered route, never JSON
//! received from the remote host.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};

use crate::agent_sessions::CliSource;
use crate::ssh_sessions::SshTarget;

pub(crate) const METHOD: &str = "_intellterm.wta/ssh_hooks";
pub(crate) const ROUTE_LEASE: Duration = Duration::from_secs(90);
pub(crate) const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
pub(crate) const MAX_RAW_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_CHUNK_BYTES: usize = 6000;
const MAX_PARTS: usize = 234;
const MAX_TRANSFERS: usize = 16;
const MAX_BUFFER_BYTES: usize = 4 * 1024 * 1024;
const TRANSFER_LIFETIME: Duration = Duration::from_secs(10);
const MAX_ENCODED_BYTES: usize = MAX_RAW_BYTES.div_ceil(3) * 4;
const MAX_COMPLETED: usize = 256;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct RouteId(uuid::Uuid);

impl RouteId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }

    pub fn parse(value: &str) -> Result<Self> {
        let id = uuid::Uuid::parse_str(value).context("Invalid SSH hook route UUID")?;
        ensure!(!id.is_nil(), "Nil SSH hook route UUID");
        Ok(Self(id))
    }
}

impl std::fmt::Display for RouteId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.hyphenated().fmt(f)
    }
}

impl std::fmt::Debug for RouteId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RouteId(<redacted>)")
    }
}

impl Serialize for RouteId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for RouteId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Registration {
    pub route: RouteId,
    pub target: SshTarget,
    pub pane_id: String,
    pub wrapper_pid: u32,
    pub no_hooks: bool,
}

impl Registration {
    pub fn validate(&mut self) -> Result<()> {
        let pane = uuid::Uuid::parse_str(&self.pane_id).context("Invalid native SSH pane UUID")?;
        ensure!(!pane.is_nil(), "Nil native SSH pane UUID");
        ensure!(self.wrapper_pid != 0, "Missing SSH wrapper process");
        self.pane_id = pane.hyphenated().to_string();
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Request {
    Register { registration: Registration },
    Revoke { registration: Registration },
    Ensure,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum TrackingStatus {
    Disabled,
    OptedOut,
    Connecting,
    /// The owned control transport is attached; actual agent hook delivery is
    /// evidenced separately by received lifecycle events.
    Ready,
    Partial {
        unavailable_providers: Vec<String>,
    },
    Unavailable {
        reason: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Response {
    pub status: TrackingStatus,
}

pub(crate) fn parse_request(
    raw: &serde_json::value::RawValue,
) -> Result<Request, serde_json::Error> {
    serde_json::from_str(raw.get())
}

pub(crate) fn build_request(
    request: &Request,
) -> Result<agent_client_protocol::schema::v1::ExtRequest> {
    let raw = serde_json::value::to_raw_value(request)?;
    Ok(agent_client_protocol::schema::v1::ExtRequest::new(
        METHOD,
        raw.into(),
    ))
}

pub(crate) const EVENTS: &[&str] = &[
    "agent.session.start",
    "agent.session.end",
    "agent.prompt.submit",
    "agent.tool.starting",
    "agent.tool.finished",
    "agent.notification",
    "agent.stop",
    "agent.error",
];

#[derive(Clone, PartialEq, Eq, Hash)]
struct TransferKey {
    route: RouteId,
    transfer: String,
}

pub(crate) struct Frame<'a> {
    pub route: RouteId,
    pub cli: &'a str,
    event: &'a str,
    transfer: &'a str,
    index: usize,
    count: usize,
    data: &'a str,
}

pub(crate) fn parse_frame(line: &str) -> Result<Frame<'_>> {
    ensure!(
        line.len() <= MAX_CHUNK_BYTES + 256,
        "SSH hook frame exceeds limit"
    );
    let fields: Vec<_> = line.split(' ').collect();
    ensure!(
        fields.len() == 8 && fields[0] == "IT_AGENT_HOOK/3",
        "Invalid SSH hook frame"
    );
    let route = RouteId::parse(fields[1])?;
    ensure!(
        matches!(
            fields[2],
            "copilot" | "claude" | "codex" | "gemini" | "opencode"
        ),
        "Unknown SSH hook CLI"
    );
    ensure!(EVENTS.contains(&fields[3]), "Unknown SSH hook event");
    ensure!(
        !fields[4].is_empty()
            && fields[4].len() <= 64
            && fields[4]
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.')),
        "Invalid SSH hook transfer"
    );
    let number = |text: &str| -> Result<usize> {
        ensure!(
            !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()),
            "Invalid SSH hook part"
        );
        text.parse().context("SSH hook part overflow")
    };
    let index = number(fields[5])?;
    let count = number(fields[6])?;
    ensure!(
        count > 0 && count <= MAX_PARTS && index < count,
        "Invalid SSH hook part range"
    );
    ensure!(
        fields[7].len() <= MAX_CHUNK_BYTES
            && fields[7].len() % 4 == 0
            && (!fields[7].is_empty() || count == 1)
            && fields[7]
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=')),
        "Invalid SSH hook base64 chunk"
    );
    ensure!(
        index + 1 == count || (fields[7].len() == MAX_CHUNK_BYTES && !fields[7].contains('=')),
        "Invalid nonfinal SSH hook chunk"
    );
    Ok(Frame {
        route,
        cli: fields[2],
        event: fields[3],
        transfer: fields[4],
        index,
        count,
        data: fields[7],
    })
}

struct Transfer {
    started: Instant,
    cli: String,
    event: String,
    parts: Vec<Option<String>>,
    bytes: usize,
}

#[derive(Default)]
pub(crate) struct Reassembler {
    transfers: HashMap<TransferKey, Transfer>,
    completed: VecDeque<(TransferKey, Instant)>,
    bytes: usize,
}

impl Reassembler {
    pub fn reset_connection(&mut self) {
        self.transfers.clear();
        self.bytes = 0;
    }

    pub fn expire(&mut self, now: Instant) {
        self.transfers.retain(|_, transfer| {
            if now.duration_since(transfer.started) >= TRANSFER_LIFETIME {
                self.bytes -= transfer.bytes;
                false
            } else {
                true
            }
        });
        self.completed
            .retain(|(_, completed)| now.duration_since(*completed) < TRANSFER_LIFETIME);
    }

    pub fn remove_route(&mut self, route: RouteId) {
        self.transfers.retain(|key, transfer| {
            if key.route == route {
                self.bytes -= transfer.bytes;
                false
            } else {
                true
            }
        });
        self.completed.retain(|(key, _)| key.route != route);
    }

    /// The caller checks this frame's route against the reader's SSH target
    /// before calling; unknown/revoked routes never allocate a transfer.
    pub fn push(&mut self, frame: Frame<'_>, now: Instant) -> Result<Option<HookEvent>> {
        self.expire(now);
        let key = TransferKey {
            route: frame.route,
            transfer: frame.transfer.to_owned(),
        };
        if self.completed.iter().any(|(done, _)| done == &key) {
            return Ok(None);
        }
        if let Some(transfer) = self.transfers.get(&key) {
            let compatible = transfer.cli == frame.cli
                && transfer.event == frame.event
                && transfer.parts.len() == frame.count
                && transfer.parts[frame.index]
                    .as_deref()
                    .is_none_or(|data| data == frame.data);
            if !compatible {
                self.discard(&key, now);
                bail!("Conflicting SSH hook transfer");
            }
            if transfer.parts[frame.index].is_some() {
                return Ok(None);
            }
        } else {
            ensure!(
                self.transfers.len() < MAX_TRANSFERS,
                "Too many SSH hook transfers"
            );
            self.transfers.insert(
                key.clone(),
                Transfer {
                    started: now,
                    cli: frame.cli.to_owned(),
                    event: frame.event.to_owned(),
                    parts: vec![None; frame.count],
                    bytes: 0,
                },
            );
        }
        let current_bytes = self
            .transfers
            .get(&key)
            .map_or(0, |transfer| transfer.bytes);
        if self.bytes + frame.data.len() > MAX_BUFFER_BYTES
            || current_bytes + frame.data.len() > MAX_ENCODED_BYTES
        {
            self.discard(&key, now);
            bail!("SSH hook reassembly byte budget exceeded");
        }
        let transfer = self
            .transfers
            .get_mut(&key)
            .context("SSH hook transfer disappeared")?;
        transfer.bytes += frame.data.len();
        self.bytes += frame.data.len();
        transfer.parts[frame.index] = Some(frame.data.to_owned());
        if transfer.parts.iter().any(Option::is_none) {
            return Ok(None);
        }
        let transfer = self
            .transfers
            .remove(&key)
            .context("SSH hook transfer disappeared")?;
        self.bytes -= transfer.bytes;
        self.remember(key, now);
        let encoded: String = transfer.parts.into_iter().flatten().collect();
        let raw = decode_base64(&encoded)?;
        ensure!(raw.len() <= MAX_RAW_BYTES, "SSH hook payload exceeds limit");
        normalize_event(&transfer.cli, &transfer.event, &raw).map(Some)
    }

    fn discard(&mut self, key: &TransferKey, now: Instant) {
        if let Some(transfer) = self.transfers.remove(key) {
            self.bytes -= transfer.bytes;
        }
        self.remember(key.clone(), now);
    }

    fn remember(&mut self, key: TransferKey, now: Instant) {
        if self.completed.len() == MAX_COMPLETED {
            self.completed.pop_front();
        }
        self.completed.push_back((key, now));
    }
}

fn decode_base64(input: &str) -> Result<Vec<u8>> {
    ensure!(input.len() % 4 == 0, "Invalid SSH hook base64 length");
    let digit = |b: u8| -> Result<u8> {
        Ok(match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => bail!("Invalid SSH hook base64 digit"),
        })
    };
    let mut output = Vec::with_capacity(input.len() / 4 * 3);
    for (index, chunk) in input.as_bytes().chunks_exact(4).enumerate() {
        let a = digit(chunk[0])?;
        let b = digit(chunk[1])?;
        let last = (index + 1) * 4 == input.len();
        output.push((a << 2) | (b >> 4));
        if chunk[2] == b'=' {
            ensure!(
                last && chunk[3] == b'=' && b & 15 == 0,
                "Invalid SSH hook base64 padding"
            );
        } else {
            let c = digit(chunk[2])?;
            output.push((b << 4) | (c >> 2));
            if chunk[3] == b'=' {
                ensure!(last && c & 3 == 0, "Invalid SSH hook base64 padding");
            } else {
                output.push((c << 6) | digit(chunk[3])?);
            }
        }
    }
    Ok(output)
}

#[derive(Clone, Serialize)]
pub(crate) struct HookEvent {
    pub cli_source: String,
    pub event: String,
    pub raw_session_id: String,
    pub payload: serde_json::Value,
}

impl HookEvent {
    pub fn cli(&self) -> CliSource {
        CliSource::parse(Some(&self.cli_source))
    }
}

fn bounded_text(value: &str, budget: usize) -> String {
    let mut text = String::new();
    let mut bytes = 0;
    for ch in value.chars() {
        let escaped = match ch {
            '"' | '\\' | '\n' | '\r' | '\t' | '\u{0008}' | '\u{000c}' => 2,
            '\u{0000}'..='\u{001f}' => 6,
            _ => ch.len_utf8(),
        };
        if bytes + escaped > budget {
            break;
        }
        text.push(ch);
        bytes += escaped;
    }
    text
}

fn normalize_event(cli: &str, event: &str, raw: &[u8]) -> Result<HookEvent> {
    // The wire explicitly permits omitted stdin as one empty part. It carries
    // no identity and can therefore affect only an already-bound native pane.
    let value: serde_json::Value = if raw.is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_slice(raw).context("Invalid SSH hook UTF-8/JSON")?
    };
    let object = value
        .as_object()
        .context("SSH hook payload must be an object")?;
    let identity = |key: &str| {
        object
            .get(key)
            .map(|id| id.as_str().context("SSH hook session id must be a string"))
            .transpose()
    };
    let snake_id = identity("session_id")?;
    let camel_id = identity("sessionId")?;
    ensure!(
        snake_id.is_none() || camel_id.is_none() || snake_id == camel_id,
        "Conflicting SSH hook session identities"
    );
    let raw_session_id = snake_id.or(camel_id).unwrap_or("");
    ensure!(
        raw_session_id.len() <= 512 && !raw_session_id.chars().any(char::is_control),
        "Invalid SSH hook session id"
    );
    let mut payload = serde_json::Map::new();
    for (key, budget) in [
        ("cwd", 1024),
        ("tool_name", 128),
        ("toolName", 128),
        ("message", 1536),
        ("notification_type", 128),
        ("reason", 512),
        ("error", 1536),
    ] {
        if let Some(text) = object.get(key).and_then(|value| value.as_str()) {
            payload.insert(key.to_owned(), bounded_text(text, budget).into());
        }
    }
    let tool = payload
        .get("tool_name")
        .or_else(|| payload.get("toolName"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if crate::agent_sessions::is_user_input_tool(tool) {
        if let Some(input) = object.get("tool_input").and_then(|value| value.as_object()) {
            let projected: serde_json::Map<_, _> = crate::app::CONSUMED_TOOL_INPUT_KEYS
                .iter()
                .filter_map(|key| {
                    input
                        .get(*key)
                        .and_then(|value| value.as_str())
                        .map(|value| ((*key).to_owned(), bounded_text(value, 512).into()))
                })
                .collect();
            payload.insert("tool_input".to_owned(), projected.into());
        }
    }
    let result = HookEvent {
        cli_source: cli.to_owned(),
        event: event.to_owned(),
        raw_session_id: raw_session_id.to_owned(),
        payload: payload.into(),
    };
    ensure!(
        serde_json::to_vec(&result)?.len() <= 8192,
        "Normalized SSH hook exceeds limit"
    );
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(route: RouteId, transfer: &str, index: usize, count: usize, data: &str) -> String {
        format!(
            "IT_AGENT_HOOK/3 {route} copilot agent.notification {transfer} {index} {count} {data}"
        )
    }

    #[test]
    fn ssh_hook_frame_rejects_unknown_and_malformed_headers() {
        let route = RouteId::new();
        let good = frame(route, "tx", 0, 1, "e30=");
        assert!(parse_frame(&good).is_ok());
        for text in [
            good.replace("/3 ", "/2 "),
            good.replace("copilot", "custom"),
            good.replace("agent.notification", "agent.unknown"),
            good.replace(" tx ", " bad:tx "),
            good.replace(" 0 1 ", " 1 1 "),
            good.replace(" 0 1 ", " 0 235 "),
            good.replace(" 0 1 ", " +0 1 "),
            good.replace(" 0 1 ", " 0 0 "),
            good.replace(" 0 1 ", " 0  1 "),
            good.replace("e30=", "%%%%"),
            frame(route, "x", 0, 2, ""),
            frame(route, "x", 0, 1, &"A".repeat(6001)),
        ] {
            assert!(parse_frame(&text).is_err(), "{text}");
        }
        assert!(parse_frame(&frame(route, "x", 0, 1, "")).is_ok());
        assert!(RouteId::parse("00000000-0000-0000-0000-000000000000").is_err());
    }

    #[test]
    fn ssh_hook_base64_is_strict_and_utf8_json_is_required() {
        for bytes in [
            b"".as_slice(),
            b"f",
            b"fo",
            b"foo",
            b"\0\xff",
            "hello é".as_bytes(),
        ] {
            assert_eq!(
                decode_base64(&crate::osc52::base64_encode(bytes)).unwrap(),
                bytes
            );
        }
        for encoded in [
            "A", "AAAA=", "Zg=A", "Zh==", "Zm9=", "Zg==AAAA", "!!!!", " A==",
        ] {
            assert!(decode_base64(encoded).is_err(), "{encoded}");
        }
        for raw in [b"[]".as_slice(), b"{bad}", b"{\"x\":\"\xff\"}"] {
            assert!(normalize_event("copilot", "agent.stop", raw).is_err());
        }
        assert!(normalize_event("copilot", "agent.stop", b"")
            .unwrap()
            .raw_session_id
            .is_empty());
    }

    #[test]
    fn ssh_hook_reassembly_is_out_of_order_idempotent_and_route_scoped() {
        let now = Instant::now();
        let route = RouteId::new();
        let other_route = RouteId::new();
        let data = crate::osc52::base64_encode(
            &serde_json::to_vec(&serde_json::json!({
                "session_id": "sid", "message": "approve", "unused": "x".repeat(5000)
            }))
            .unwrap(),
        );
        let half = MAX_CHUNK_BYTES;
        let a = frame(route, "shared", 0, 2, &data[..half]);
        let b = frame(route, "shared", 1, 2, &data[half..]);
        let other = frame(other_route, "shared", 0, 2, &data[..half]);
        let mut assembler = Reassembler::default();
        assert!(assembler
            .push(parse_frame(&b).unwrap(), now)
            .unwrap()
            .is_none());
        assert!(assembler
            .push(parse_frame(&b).unwrap(), now)
            .unwrap()
            .is_none());
        assert!(assembler
            .push(parse_frame(&other).unwrap(), now)
            .unwrap()
            .is_none());
        let event = assembler
            .push(parse_frame(&a).unwrap(), now)
            .unwrap()
            .unwrap();
        assert_eq!(event.raw_session_id, "sid");
        assert_eq!(event.payload["message"], "approve");
        assert!(assembler
            .push(parse_frame(&a).unwrap(), now)
            .unwrap()
            .is_none());
        assert_eq!(assembler.transfers.len(), 1);
        assembler.remove_route(other_route);
        assert_eq!(assembler.bytes, 0);
        assert!(assembler.transfers.is_empty());
        assembler.push(parse_frame(&other).unwrap(), now).unwrap();
        assembler.reset_connection();
        assert_eq!(assembler.bytes, 0);
        assert!(assembler.transfers.is_empty());
    }

    #[test]
    fn ssh_hook_limits_bound_transfers_bytes_and_lifetime() {
        let now = Instant::now();
        let route = RouteId::new();
        let mut assembler = Reassembler::default();
        for n in 0..MAX_TRANSFERS {
            let text = frame(route, &n.to_string(), 0, 2, &"A".repeat(MAX_CHUNK_BYTES));
            assembler.push(parse_frame(&text).unwrap(), now).unwrap();
        }
        assert!(assembler
            .push(
                parse_frame(&frame(route, "extra", 0, 2, &"A".repeat(MAX_CHUNK_BYTES))).unwrap(),
                now
            )
            .is_err());
        assembler.expire(now + TRANSFER_LIFETIME);
        assert!(assembler.transfers.is_empty());
        assert_eq!(assembler.bytes, 0);

        let mut assembler = Reassembler::default();
        for transfer in ["a", "b", "c"] {
            for index in 0..233 {
                let text = frame(route, transfer, index, 234, &"A".repeat(6000));
                assembler.push(parse_frame(&text).unwrap(), now).unwrap();
            }
        }
        assert!(assembler.bytes <= MAX_BUFFER_BYTES);
        assert!(assembler
            .push(
                parse_frame(&frame(route, "d", 0, 2, &"A".repeat(6000))).unwrap(),
                now
            )
            .is_err());
        assert!(assembler.bytes <= MAX_BUFFER_BYTES);
        assembler.expire(now + TRANSFER_LIFETIME);
        assert_eq!(assembler.bytes, 0);
    }

    #[test]
    fn ssh_hook_conflicting_part_poisoning_and_normalized_budget_are_safe() {
        let now = Instant::now();
        let route = RouteId::new();
        let mut assembler = Reassembler::default();
        assembler
            .push(
                parse_frame(&frame(route, "x", 0, 2, &"A".repeat(MAX_CHUNK_BYTES))).unwrap(),
                now,
            )
            .unwrap();
        assert!(assembler
            .push(
                parse_frame(&frame(route, "x", 0, 2, &"B".repeat(MAX_CHUNK_BYTES))).unwrap(),
                now
            )
            .is_err());
        assert_eq!(assembler.bytes, 0);
        assert!(assembler
            .push(parse_frame(&frame(route, "x", 1, 2, "AAAA")).unwrap(), now)
            .unwrap()
            .is_none());

        let payload = serde_json::json!({
            "sessionId": "sid", "tool_name": "ask_user", "message": "\u{0001}".repeat(40000),
            "prompt": "secret prompt", "tool_output": "secret output", "access_token": "secret credential",
            "tool_input": { "question": "q".repeat(10000), "command": "never retained", "message": "m".repeat(10000) },
            "cwd": "/home/ü".repeat(4000), "error": "\n".repeat(10000), "reason": "r".repeat(10000),
        });
        let event = normalize_event(
            "copilot",
            "agent.notification",
            &serde_json::to_vec(&payload).unwrap(),
        )
        .unwrap();
        let normalized = serde_json::to_string(&event).unwrap();
        assert!(normalized.len() <= 8192);
        assert!(!normalized.contains("secret"));
        assert!(!normalized.contains("never retained"));
        assert!(
            event.payload["tool_input"]["question"]
                .as_str()
                .unwrap()
                .len()
                <= 512
        );
    }

    #[test]
    fn ssh_hook_accepts_exact_maximum_raw_payload_and_rejects_larger() {
        let route = RouteId::new();
        let mut raw = br#"{"session_id":"sid","secret":""#.to_vec();
        raw.extend(vec![b'x'; MAX_RAW_BYTES - raw.len() - 2]);
        raw.extend_from_slice(b"\"}");
        assert_eq!(raw.len(), MAX_RAW_BYTES);
        let encoded = crate::osc52::base64_encode(&raw);
        let count = encoded.len().div_ceil(MAX_CHUNK_BYTES);
        assert_eq!(count, MAX_PARTS);
        let mut assembler = Reassembler::default();
        let now = Instant::now();
        let mut result = None;
        for (index, bytes) in encoded.as_bytes().chunks(MAX_CHUNK_BYTES).enumerate() {
            let text = frame(
                route,
                "max",
                index,
                count,
                std::str::from_utf8(bytes).unwrap(),
            );
            result = assembler.push(parse_frame(&text).unwrap(), now).unwrap();
        }
        assert_eq!(result.unwrap().raw_session_id, "sid");
        assert_eq!(assembler.bytes, 0);
        let oversized = crate::osc52::base64_encode(&vec![b'x'; MAX_RAW_BYTES + 1]);
        let mut rejected = false;
        for (index, bytes) in oversized.as_bytes().chunks(MAX_CHUNK_BYTES).enumerate() {
            let text = frame(
                route,
                "over",
                index,
                count,
                std::str::from_utf8(bytes).unwrap(),
            );
            rejected |= assembler.push(parse_frame(&text).unwrap(), now).is_err();
        }
        assert!(rejected);
    }

    #[test]
    fn ssh_hook_alias_conflicts_are_rejected_without_provider_specific_identity_rules() {
        for cli in ["copilot", "claude", "codex", "gemini", "opencode"] {
            for body in [
                br#"{"session_id":"sid"}"#.as_slice(),
                br#"{"sessionId":"sid"}"#,
                br#"{"session_id":"sid","sessionId":"sid"}"#,
            ] {
                assert_eq!(
                    normalize_event(cli, "agent.stop", body)
                        .unwrap()
                        .raw_session_id,
                    "sid"
                );
            }
        }
        for body in [
            br#"{"session_id":"one","sessionId":"two"}"#.as_slice(),
            br#"{"session_id":"","sessionId":"two"}"#,
            br#"{"sessionId":12}"#,
        ] {
            assert!(normalize_event("copilot", "agent.stop", body).is_err());
        }
        assert!(normalize_event(
            "copilot",
            "agent.stop",
            br#"{"agent_session_id":"not-a-native-identity"}"#
        )
        .unwrap()
        .raw_session_id
        .is_empty());
    }
}
