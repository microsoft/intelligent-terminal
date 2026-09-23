// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

//! Single-owner transactional work authority. No executor is called inside a transaction.
//!
//! Experimental project.configure approves exact local capabilities and finite budgets.
//! A separate human work.start binds its grant preview before workspace/task admission.
//! Authority-expanding migrations, human command gates, cross-work graph edits and publication return
//! explicit unsupported responses; they never record simulated advancement.
//!
//! Bootstrap extensions:
//! - project.configure: ProjectConfigure; human approval of the named root, capabilities
//!   and finite limits. Returns projectId, version and policyRevision.
//! - project.list: {limit, afterId?}; explicit project discovery, never an implicit default.
//! - console.open: {consoleSessionId, projectId, conversationId}; registers UUID identities
//!   and returns contextVersion. First conversation.submit can use the same validated path.
//! Runtime adapter configuration is separate trusted host configuration keyed by capabilityId;
//! adapter executable/environment fields are not accepted in invocation protocol payloads.
//! The human installs adapters.json with `wta center configure --input-json <file>` while
//! the authority is stopped, then starts it with `wta center serve`.
//! It contains capabilities: [{id, adapter: {kind: "ACP", executable, args, model?,
//! environment?, approvedModelDestination}}]. The destination is explicit human approval
//! metadata for the provider/account or endpoint, not an inferred environment default.
//! Capability IDs in ProjectConfigure must match registered runtime IDs and invocation kinds.
//! Startup order is Engine::open, Runtime::new(handle, root), runtime.register().await, then
//! client admission/effect dispatch. Registration never grants project or model execution.
//! Restart disconnects saved runtime registrations; only freshly registered runtimes dispatch.
//! Approval selects a configured provider/model capability, not an OS-enforced network boundary.
//! work.propose_change takes the complete brief plus revision=currentSpecRevision.
//! Preview the current spec/policy grant, then work.apply_change binds Work+ChangeProposal
//! versions and that exact preview. No limits reset or authority expansion occurs.
//! Application supersedes current outputs and waits for tracked release before replanning.

#[path = "engine\\actions.rs"]
mod actions;
#[path = "engine\\changes.rs"]
mod changes;
#[path = "engine\\collaboration.rs"]
mod collaboration;
#[path = "engine\\continuation.rs"]
mod continuation;
#[path = "engine\\evaluation.rs"]
mod evaluation;
#[path = "engine\\executor.rs"]
mod executor;
#[path = "engine\\global.rs"]
mod global;
#[path = "engine\\memory.rs"]
mod memory;
#[path = "engine\\planning.rs"]
mod planning;
#[path = "engine\\runtime.rs"]
mod runtime;
#[cfg(test)]
#[path = "engine\\tests.rs"]
mod tests;

use super::wire::*;
use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::path::Path;
use uuid::Uuid;

type DomainResult<T> = std::result::Result<T, Response>;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Effect {
    pub id: String,
    pub method: String,
    pub params: Value,
}

pub struct Engine {
    db: Connection,
    _owner: File,
    store_id: String,
    position: u64,
    records: BTreeMap<String, Value>,
    pending_events: Vec<Value>,
    correlation: String,
}

fn id() -> String {
    Uuid::new_v4().to_string()
}

fn utc_after(seconds: u64) -> String {
    let now = time::OffsetDateTime::now_utc()
        + time::Duration::seconds(seconds.min(i64::MAX as u64) as i64);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        now.year(),
        u8::from(now.month()),
        now.day(),
        now.hour(),
        now.minute(),
        now.second()
    )
}

fn bad(code: &str, message: impl Into<String>) -> Response {
    Response::fail("", code, message)
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn number(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn values(value: &Value, key: &str) -> Vec<Value> {
    value
        .get(key)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn parse<T: DeserializeOwned>(value: &Value) -> DomainResult<T> {
    fn has_null(value: &Value) -> bool {
        match value {
            Value::Null => true,
            Value::Array(items) => items.iter().any(has_null),
            Value::Object(fields) => fields.values().any(has_null),
            _ => false,
        }
    }
    if has_null(value) {
        return Err(bad(
            "INVALID_ARGUMENT",
            "Optional fields must be omitted, not null",
        ));
    }
    serde_json::from_value(value.clone())
        .map_err(|error| bad("INVALID_ARGUMENT", error.to_string()))
}

fn encode<T: Serialize>(value: &T) -> DomainResult<Value> {
    let mut value =
        serde_json::to_value(value).map_err(|error| bad("INVALID_ARGUMENT", error.to_string()))?;
    omit_nulls(&mut value);
    Ok(value)
}

fn omit_nulls(value: &mut Value) {
    match value {
        Value::Object(object) => {
            object.retain(|_, value| !value.is_null());
            object.values_mut().for_each(omit_nulls);
        }
        Value::Array(array) => array.iter_mut().for_each(omit_nulls),
        _ => {}
    }
}

/// Contract manifests contain integers, strings and closed objects, never floating-point data.
fn digest(value: &Value) -> DomainResult<String> {
    fn canonical(value: &Value, output: &mut String) -> DomainResult<()> {
        match value {
            Value::Object(object) => {
                let mut keys: Vec<_> = object.keys().collect();
                keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
                output.push('{');
                for (index, key) in keys.into_iter().enumerate() {
                    if index != 0 {
                        output.push(',');
                    }
                    output.push_str(
                        &serde_json::to_string(key)
                            .map_err(|e| bad("INVALID_ARGUMENT", e.to_string()))?,
                    );
                    output.push(':');
                    canonical(&object[key], output)?;
                }
                output.push('}');
            }
            Value::Array(array) => {
                output.push('[');
                for (index, item) in array.iter().enumerate() {
                    if index != 0 {
                        output.push(',');
                    }
                    canonical(item, output)?;
                }
                output.push(']');
            }
            Value::Number(n) if !n.is_i64() && !n.is_u64() => {
                return Err(bad(
                    "INVALID_ARGUMENT",
                    "Floating-point contract values are unsupported",
                ));
            }
            _ => output.push_str(
                &serde_json::to_string(value)
                    .map_err(|e| bad("INVALID_ARGUMENT", e.to_string()))?,
            ),
        }
        Ok(())
    }
    let mut canonical_json = String::new();
    canonical(value, &mut canonical_json)?;
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(canonical_json.as_bytes())
    ))
}

fn uuid(value: &str) -> DomainResult<()> {
    Uuid::parse_str(value)
        .map(|_| ())
        .map_err(|_| bad("INVALID_ARGUMENT", "Expected a UUID"))
}

fn nonempty(value: &str, field: &str) -> DomainResult<()> {
    if value.trim().is_empty() {
        return Err(bad(
            "INVALID_ARGUMENT",
            format!("{field} must not be empty"),
        ));
    }
    Ok(())
}

fn closed(value: &Value, required: &[&str], optional: &[&str]) -> DomainResult<()> {
    let object = value
        .as_object()
        .ok_or_else(|| bad("INVALID_ARGUMENT", "Expected an object"))?;
    if let Some(key) = object
        .keys()
        .find(|key| !required.contains(&key.as_str()) && !optional.contains(&key.as_str()))
    {
        return Err(bad("INVALID_ARGUMENT", format!("Unknown field {key}")));
    }
    for key in required {
        if !object.contains_key(*key) || object[*key].is_null() {
            return Err(bad("INVALID_ARGUMENT", format!("Missing field {key}")));
        }
    }
    if object.values().any(Value::is_null) {
        return Err(bad(
            "INVALID_ARGUMENT",
            "Optional fields must be omitted, not null",
        ));
    }
    Ok(())
}

impl Engine {
    pub fn open(root: &Path) -> Result<Self> {
        std::fs::create_dir_all(root).context("Create Agent Center state directory")?;
        let owner = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join("owner.lock"))
            .context("Open Agent Center lifetime lock")?;
        owner
            .try_lock()
            .context("Another Agent Center authority owns this state root")?;
        let db =
            Connection::open(root.join("work.db")).context("Open Agent Center SQLite store")?;
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        db.execute_batch(
            "PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;",
        )?;
        let version: i64 = db.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        anyhow::ensure!(
            version == 0 || version == 1,
            "Unsupported Agent Center schema version {version}; explicit migration required"
        );
        if version == 0 {
            let count: i64 = db.query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'", [], |row| row.get(0))?;
            anyhow::ensure!(
                count == 0,
                "Unversioned nonempty Agent Center store; refusing to replace data"
            );
            db.execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 CREATE TABLE records(id TEXT PRIMARY KEY, kind TEXT NOT NULL, version INTEGER NOT NULL CHECK(version>0), parent_id TEXT REFERENCES records(id) DEFERRABLE INITIALLY DEFERRED, body TEXT NOT NULL CHECK(json_valid(body)));
                 CREATE INDEX records_kind ON records(kind,id);
                 CREATE TABLE commands(principal TEXT NOT NULL, command_id TEXT NOT NULL, fingerprint TEXT NOT NULL, response TEXT NOT NULL CHECK(json_valid(response)), PRIMARY KEY(principal,command_id));
                 CREATE TABLE events(sequence INTEGER PRIMARY KEY AUTOINCREMENT, event_id TEXT NOT NULL UNIQUE, body TEXT NOT NULL CHECK(json_valid(body)));
                 CREATE TABLE effects(id TEXT PRIMARY KEY REFERENCES records(id), method TEXT NOT NULL, body TEXT NOT NULL CHECK(json_valid(body)), state TEXT NOT NULL CHECK(state IN ('Pending','Dispatched','Completed')), response TEXT CHECK(response IS NULL OR json_valid(response)));
                 PRAGMA user_version=1;"
            )?;
            db.execute("INSERT INTO meta(key,value) VALUES('storeId',?1)", [id()])?;
            db.execute_batch("COMMIT")?;
        }
        let check: String = db.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
        anyhow::ensure!(
            check == "ok",
            "Agent Center store integrity check failed: {check}"
        );
        let violations: i64 =
            db.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })?;
        anyhow::ensure!(
            violations == 0,
            "Agent Center store has broken record references"
        );
        let store_id: String =
            db.query_row("SELECT value FROM meta WHERE key='storeId'", [], |row| {
                row.get(0)
            })?;
        Uuid::parse_str(&store_id).context("Invalid Agent Center store identity")?;
        let records = {
            let mut statement = db.prepare("SELECT id,body FROM records ORDER BY id")?;
            let records = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .map(|row| {
                    let (id, body) = row?;
                    Ok((id, serde_json::from_str(&body)?))
                })
                .collect::<Result<BTreeMap<_, _>>>()?;
            records
        };
        let position: i64 =
            db.query_row("SELECT COALESCE(MAX(sequence),0) FROM events", [], |row| {
                row.get(0)
            })?;
        let mut engine = Self {
            db,
            _owner: owner,
            store_id,
            position: u64::try_from(position)?,
            records,
            pending_events: Vec::new(),
            correlation: id(),
        };
        // Restart admission is deliberately fail-closed, including an intent whose dispatch
        // was not observed. Only explicit runtime/capture reconciliation can settle it.
        let interrupted = {
            let mut statement = engine
                .db
                .prepare("SELECT e.id FROM effects e JOIN records r ON r.id=e.id
                    WHERE e.state!='Completed' OR json_extract(r.body,'$.status') IN ('Pending','Running','RepairRequired')")?;
            let interrupted = statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            interrupted
        };
        if !interrupted.is_empty()
            || engine
                .all("Runtime")
                .iter()
                .any(|runtime| text(runtime, "status") == "Registered")
        {
            engine.db.execute_batch("BEGIN IMMEDIATE")?;
            for mut runtime in engine.all("Runtime") {
                if text(&runtime, "status") == "Registered" {
                    runtime["status"] = json!("Disconnected");
                    engine.put(runtime);
                }
            }
            for operation_id in interrupted {
                if let Some(mut operation) = engine.records.get(&operation_id).cloned() {
                    if text(&operation, "status") == "Succeeded" {
                        continue;
                    }
                    if ["runtime.stop", "runtime.release"]
                        .contains(&text(&operation["effect"], "method"))
                        && engine
                            .records
                            .get(text(&operation["effect"]["params"], "invocationId"))
                            .is_some_and(|invocation| {
                                invocation["releaseKind"] == "CoordinatorAuthorityRevoked"
                            })
                    {
                        continue;
                    }
                    engine.db.execute(
                        "UPDATE effects SET state='Dispatched' WHERE id=?1 AND state='Pending'",
                        [&operation_id],
                    )?;
                    operation["status"] = json!("RepairRequired");
                    operation["failure"] = serde_json::to_value(bad("OUTCOME_UNKNOWN",
                        "Service restarted after dispatch; reconcile this exact effect before replacement").failure)?;
                    let operation = engine.put(operation);
                    engine.emit("OperationChanged", &operation);
                    engine.attention(text(&operation,"workId"),&operation_id,"RestartReconciliation",
                        "Inspect the runtime/capture ledger for this exact operation; do not issue a replacement command");
                }
            }
            engine.persist()?;
            engine.db.execute_batch("COMMIT")?;
        }
        Ok(engine)
    }

    pub fn store_id(&self) -> String {
        self.store_id.clone()
    }

    pub fn cursor(&self) -> String {
        format!("{}:{}", self.store_id, self.position)
    }

    /// SQLite selects a consistent snapshot; an existing destination is never replaced.
    pub fn backup(&self, destination: &Path) -> Result<()> {
        anyhow::ensure!(!destination.exists(), "Backup destination already exists");
        self.db
            .execute("VACUUM INTO ?1", [destination.to_string_lossy().as_ref()])
            .context("Create transaction-consistent Agent Center backup")?;
        Ok(())
    }

    pub fn handle(&mut self, principal: &Principal, request: Request) -> Response {
        let request_id = request.request_id.clone();
        match self.execute(principal, &request) {
            Ok(mut response) => {
                response.request_id = request_id;
                response
            }
            Err(error) => Response::fail(
                request_id,
                "EXECUTION_FAILED",
                format!("Durable store transaction failed: {error:#}"),
            ),
        }
    }

    fn execute(&mut self, principal: &Principal, request: &Request) -> Result<Response> {
        let checked = || -> DomainResult<()> {
            uuid(&request.request_id)?;
            if request.message_type != "request" {
                return Err(bad("INVALID_ARGUMENT", "Expected type request"));
            }
            if !request.params.is_object() {
                return Err(bad("INVALID_ARGUMENT", "params must be an object"));
            }
            if serde_json::to_vec(request)
                .map_err(|e| bad("INVALID_ARGUMENT", e.to_string()))?
                .len()
                > 1_048_576
            {
                return Err(bad("INVALID_ARGUMENT", "Request exceeds frame limit"));
            }
            for subject in &request.if_match {
                uuid(&subject.id)?;
                if subject.version == 0 {
                    return Err(bad("INVALID_ARGUMENT", "Entity versions are positive"));
                }
            }
            Ok(())
        };
        if let Err(response) = checked() {
            return Ok(response);
        }
        let read = is_read_method(&request.method);
        if read {
            if request.command_id.is_some() || !request.if_match.is_empty() {
                return Ok(bad(
                    "INVALID_ARGUMENT",
                    "Reads omit commandId and have empty ifMatch",
                ));
            }
            return Ok(match self.read(principal, request) {
                Ok(data) => {
                    let subjects = if request.method == "delivery.get" {
                        let work = match self.record(text(&data, "workId"), "Work") {
                            Ok(work) => work,
                            Err(response) => return Ok(response),
                        };
                        vec![Self::reference(&work), Self::reference(&data)]
                    } else {
                        Vec::new()
                    };
                    let mut response = Response::ok("", data);
                    response.subjects = subjects;
                    response.cursor = Some(self.cursor());
                    response
                }
                Err(response) => response,
            });
        }
        let Some(command_id) = &request.command_id else {
            return Ok(bad("INVALID_ARGUMENT", "Mutation requires commandId"));
        };
        if let Err(response) = uuid(command_id) {
            return Ok(response);
        }
        let fingerprint = match digest(
            &json!({"method":request.method,"ifMatch":request.if_match,"params":request.params}),
        ) {
            Ok(value) => value,
            Err(response) => return Ok(response),
        };
        self.db.execute_batch("BEGIN IMMEDIATE")?;
        let backup = self.records.clone();
        let original_position = self.position;
        self.pending_events.clear();
        self.correlation = command_id.clone();
        let result = (|| -> Result<Response> {
            let prior: Option<(String,String)> = self.db.query_row(
                "SELECT fingerprint,response FROM commands WHERE principal=?1 AND command_id=?2",
                params![principal.key(), command_id], |row| Ok((row.get(0)?,row.get(1)?))
            ).optional()?;
            if let Some((old, body)) = prior {
                return Ok(if old == fingerprint {
                    serde_json::from_str(&body)?
                } else {
                    bad(
                        "COMMAND_ID_REUSED",
                        "commandId already identifies different semantic content",
                    )
                });
            }
            let mutation = self
                .validate_human_action(principal, request)
                .and_then(|()| self.mutate(principal, request))
                .and_then(|response| {
                    self.record_human_action(request, &response)?;
                    Ok(response)
                });
            let mut response = match mutation {
                Ok(response) => {
                    if let Err(response) = if request.method == "work.open" {
                        Ok(())
                    } else {
                        self.drive()
                    } {
                        self.records = backup.clone();
                        self.pending_events.clear();
                        response
                    } else {
                        response
                    }
                }
                Err(response) => {
                    self.records = backup.clone();
                    self.pending_events.clear();
                    response
                }
            };
            self.persist_changes(&backup)?;
            response.cursor = Some(self.cursor());
            self.db.execute("INSERT INTO commands(principal,command_id,fingerprint,response) VALUES(?1,?2,?3,?4)",
                params![principal.key(),command_id,fingerprint,serde_json::to_string(&response)?])?;
            Ok(response)
        })();
        match result {
            Ok(response) => match self.db.execute_batch("COMMIT") {
                Ok(()) => Ok(response),
                Err(error) => {
                    self.records = backup;
                    self.position = original_position;
                    self.pending_events.clear();
                    let _ = self.db.execute_batch("ROLLBACK");
                    Err(error.into())
                }
            },
            Err(error) => {
                self.records = backup;
                self.position = original_position;
                self.pending_events.clear();
                self.db
                    .execute_batch("ROLLBACK")
                    .context("Rollback failed Agent Center transaction")?;
                Err(error)
            }
        }
    }

    fn persist(&mut self) -> Result<()> {
        self.persist_changes(&BTreeMap::new())
    }

    fn persist_changes(&mut self, before: &BTreeMap<String, Value>) -> Result<()> {
        for (id, record) in &self.records {
            // put() advances the version on every change; streaming one chunk must
            // not serialize and rewrite the entire retained work history.
            if before
                .get(id)
                .is_some_and(|previous| previous["version"] == record["version"])
            {
                continue;
            }
            let parent = record
                .get("workId")
                .and_then(Value::as_str)
                .filter(|parent| *parent != id);
            self.db.execute(
                "INSERT INTO records(id,kind,version,parent_id,body) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(id) DO UPDATE SET version=excluded.version,parent_id=excluded.parent_id,body=excluded.body",
                params![id,text(record,"kind"),i64::try_from(number(record,"version"))?,parent,serde_json::to_string(record)?])?;
            if text(record, "kind") == "Operation" && record.get("effect").is_some() {
                self.db.execute(
                    "INSERT OR IGNORE INTO effects(id,method,body,state) VALUES(?1,?2,?3,'Pending')",
                    params![id,text(&record["effect"],"method"),serde_json::to_string(&record["effect"]["params"])?])?;
            }
        }
        for event in std::mem::take(&mut self.pending_events) {
            let body = serde_json::to_string(&event)?;
            anyhow::ensure!(body.len() <= 1_048_064,
                "Event projection exceeds the v1 frame budget; move large bodies into captured artifacts");
            self.db.execute(
                "INSERT INTO events(event_id,body) VALUES(?1,?2)",
                params![text(&event, "eventId"), body],
            )?;
            self.position = u64::try_from(self.db.last_insert_rowid())?;
        }
        Ok(())
    }

    fn record(&self, id: &str, kind: &str) -> DomainResult<Value> {
        self.records
            .get(id)
            .filter(|value| text(value, "kind") == kind)
            .cloned()
            .ok_or_else(|| {
                bad(
                    "INVALID_REFERENCE",
                    format!("{kind} {id} is not in this store"),
                )
            })
    }

    fn all(&self, kind: &str) -> Vec<Value> {
        self.records
            .values()
            .filter(|value| text(value, "kind") == kind)
            .cloned()
            .collect()
    }

    fn related(&self, kind: &str, field: &str, id: &str) -> Vec<Value> {
        self.all(kind)
            .into_iter()
            .filter(|value| text(value, field) == id)
            .collect()
    }

    fn put(&mut self, mut value: Value) -> Value {
        let record_id = text(&value, "id").to_string();
        let version = self
            .records
            .get(&record_id)
            .map_or(1, |prior| number(prior, "version") + 1);
        value["version"] = json!(version);
        self.records.insert(record_id, value.clone());
        value
    }

    fn create(&mut self, kind: &str, mut body: Value) -> Value {
        body["id"] = json!(id());
        body["kind"] = json!(kind);
        body["createdAt"] = json!(utc_after(0));
        self.put(body)
    }

    fn reference(value: &Value) -> EntityRef {
        EntityRef {
            kind: text(value, "kind").into(),
            id: text(value, "id").into(),
            version: number(value, "version"),
        }
    }

    fn dispatch_view(record: &Value) -> Value {
        let mut dispatch = record.clone();
        dispatch["kind"] = record["dispatchKind"].clone();
        if let Some(fields) = dispatch.as_object_mut() {
            for field in ["dispatchKind", "version", "createdAt"] {
                fields.remove(field);
            }
        }
        dispatch
    }

    fn matched(&self, request: &Request, records: &[&Value]) -> DomainResult<()> {
        let expected: BTreeSet<_> = records
            .iter()
            .map(|record| (text(record, "kind"), text(record, "id")))
            .collect();
        let supplied: BTreeSet<_> = request
            .if_match
            .iter()
            .map(|reference| (reference.kind.as_str(), reference.id.as_str()))
            .collect();
        if expected != supplied || supplied.len() != request.if_match.len() {
            return Err(bad(
                "INVALID_ARGUMENT",
                "ifMatch must contain exactly the operation's mutable subjects",
            ));
        }
        for record in records {
            if !request.if_match.iter().any(|reference| {
                reference.kind == text(record, "kind")
                    && reference.id == text(record, "id")
                    && reference.version == number(record, "version")
            }) {
                let mut response = bad(
                    "STALE_VERSION",
                    "Refresh the current subjects before issuing a new command",
                );
                response.subjects = records
                    .iter()
                    .map(|record| Self::reference(record))
                    .collect();
                if let Some(failure) = &mut response.failure {
                    failure.subjects = response.subjects.clone();
                }
                return Err(response);
            }
        }
        Ok(())
    }

    fn human(principal: &Principal) -> DomainResult<()> {
        if matches!(principal, Principal::Human | Principal::Service) {
            Ok(())
        } else {
            Err(bad("FORBIDDEN", "This operation requires human authority"))
        }
    }

    fn coordinator(&self, principal: &Principal, work_id: Option<&str>) -> DomainResult<Value> {
        if matches!(principal, Principal::Service) {
            return Ok(json!({}));
        }
        let Principal::Invocation { invocation_id } = principal else {
            return Err(bad("FORBIDDEN", "Requires a bound coordinator invocation"));
        };
        let invocation = self.record(invocation_id, "Invocation")?;
        if text(&invocation["subject"], "kind") != "Coordination"
            || text(&invocation, "state") != "Running"
        {
            return Err(bad("FORBIDDEN", "Invocation is not an active coordinator"));
        }
        let turn = self.record(text(&invocation["subject"], "id"), "CoordinationTurn")?;
        if invocation["scope"] == "Global" {
            self.global_coordinator(principal)?;
            if work_id.is_some() {
                return Err(bad(
                    "FORBIDDEN",
                    "Global conversation may propose human actions, not operate a work coordinator",
                ));
            }
        }
        if let Some(work_id) = work_id {
            let work = self.record(work_id, "Work")?;
            if work["projectId"] != invocation["projectId"] {
                return Err(bad(
                    "FORBIDDEN",
                    "Coordinator cannot act outside its approved project",
                ));
            }
            if text(&turn, "workId") != work_id && !text(&turn, "workId").is_empty() {
                return Err(bad("FORBIDDEN", "Coordinator is bound to another work"));
            }
        }
        Ok(turn)
    }

    fn emit(&mut self, kind: &str, record: &Value) -> String {
        let event_id = id();
        let mut event = json!({
            "type":"event","eventId":event_id,"kind":kind,"subject":Self::reference(record),
            "correlationId":self.correlation,"changes":[{"subject":Self::reference(record),"view":self.view(record)}]
        });
        for field in ["workId", "taskId", "attemptId", "invocationId"] {
            if let Some(value) = record.get(field) {
                event[field] = value.clone();
            }
        }
        if text(record, "kind") == "Work" {
            event["workId"] = record["id"].clone();
        }
        self.pending_events.push(event);
        event_id
    }

    fn effect(&mut self, method: &str, params: Value, work_id: Option<&str>) -> Value {
        let mut body = json!({"status":"Pending","steps":[{"state":"Planned","intent":method}],"effect":{"method":method,"params":params},"commandId":self.correlation});
        if let Some(work_id) = work_id {
            body["workId"] = json!(work_id);
        }
        let operation = self.create("Operation", body);
        self.emit("OperationChanged", &operation);
        operation
    }

    fn attention(&mut self, work_id: &str, subject_id: &str, reason: &str, next_action: &str) {
        if self.all("AttentionItem").iter().any(|item| {
            text(item, "subjectId") == subject_id
                && text(item, "reason") == reason
                && text(item, "status") == "Open"
        }) {
            return;
        }
        let mut body = json!({"subjectId":subject_id,"reason":reason,"requiredAction":next_action,"status":"Open"});
        if !work_id.is_empty() {
            body["workId"] = json!(work_id);
        }
        let item = self.create("AttentionItem", body);
        self.emit("AttentionChanged", &item);
    }

    fn resolve_attention(&mut self, subject_id: &str) {
        for mut item in self.related("AttentionItem", "subjectId", subject_id) {
            if text(&item, "status") == "Open" {
                item["status"] = json!("Resolved");
                let item = self.put(item);
                self.emit("AttentionChanged", &item);
            }
        }
    }

    fn view(&self, record: &Value) -> Value {
        let id = text(record, "id");
        match text(record, "kind") {
            "Conversation" => {
                let mut view = record.clone();
                if record.get("workId").is_some() {
                    if let Some(messages) = view["messages"].as_array_mut() {
                        for message in messages {
                            if let Some(current) = self.records.get(text(message, "id")) {
                                if current["kind"] == "ConversationItem" {
                                    *message = current.clone();
                                }
                            }
                        }
                    }
                }
                view["intakeRequests"] = json!(self.related("IntakeRequest", "conversationId", id));
                let mut proposals = self.related("HumanActionProposal", "conversationId", id);
                if let Some(work_id) = record.get("workId").and_then(Value::as_str) {
                    for proposal in self.related("HumanActionProposal", "workId", work_id) {
                        if !proposals
                            .iter()
                            .any(|existing| existing["id"] == proposal["id"])
                        {
                            proposals.push(proposal);
                        }
                    }
                }
                view["actionProposals"] = json!(proposals);
                view
            }
            "Work" => {
                let mut view = json!({"work":record,"spec":record["spec"],
                    "continuation":self.work_continuation(record),
                    "taskSummaries":self.related("Task","workId",id),
                    "changeProposals":self.related("ChangeProposal","workId",id),
                    "humanContributions":self.related("HumanContribution","workId",id),
                    "obligations":self.related("AttentionItem","workId",id).into_iter().filter(|v|text(v,"status")=="Open").collect::<Vec<_>>()});
                view["spec"]["revision"] = record["currentSpecRevision"].clone();
                if let Some(plan) = self.records.get(text(record, "planId")) {
                    view["plan"] = plan.clone();
                }
                if let Some(candidate) = self.records.get(text(record, "currentCandidateId")) {
                    view["candidate"] = candidate.clone();
                }
                if let Some(operation) = self.records.get(text(record, "changeOperationId")) {
                    view["changeOperation"] = self.view(operation);
                }
                if Self::executor_mode(record) {
                    view["executionSummary"] = self.executor_summary(record);
                }
                view
            }
            "Task" => {
                let mut view = json!({"task":record,"contextRequests":self.related("ContextRequest","taskId",id),
                    "evaluations":self.related("EvaluationUnit","taskId",id),
                    "obligations":self.related("AttentionItem","subjectId",id)});
                if let Some(attempt) = self.records.get(text(record, "currentAttemptId")) {
                    view["attempt"] = attempt.clone();
                    if let Some(dispatch) = self.records.get(text(attempt, "dispatchId")) {
                        view["dispatch"] = Self::dispatch_view(dispatch);
                    }
                }
                if let Some(result) = self.records.get(text(record, "currentResultId")) {
                    view["currentResult"] = result.clone();
                }
                view
            }
            "TaskDispatch" => Self::dispatch_view(record),
            "TaskResult" => {
                json!({"result":record,"body":record["body"],"disposition":record["disposition"],
                "evaluationRound":record["evaluationRound"],"gates":self.related("GateResult","resultId",id),
                "reviews":self.related("TaskReview","resultId",id),"rework":self.related("ReworkInstruction","resultId",id)})
            }
            "Operation" => {
                let mut view = json!({"operation":record,"subjects":[]});
                if let Some(failure) = record.get("failure") {
                    view["failure"] = failure.clone();
                }
                if text(record, "status") == "RepairRequired" {
                    view["nextAction"] = json!(
                        "Reconcile the exact recorded effect; do not repeat with a new identity"
                    );
                }
                view
            }
            _ => record.clone(),
        }
    }

    fn authorized_read(&self, principal: &Principal, record: &Value) -> DomainResult<()> {
        if matches!(
            principal,
            Principal::Human | Principal::Service | Principal::Runtime { .. }
        ) {
            return Ok(());
        }
        let Principal::Invocation { invocation_id } = principal else {
            return Err(bad("FORBIDDEN", "No read binding"));
        };
        let invocation = self.record(invocation_id, "Invocation")?;
        if invocation["scope"] == "Global" {
            return self.authorized_global_read(principal, record);
        }
        if text(&invocation, "state") == "Released" {
            return Err(bad("FORBIDDEN", "Invocation binding was released"));
        }
        let work_id = text(&invocation, "workId");
        let project_id = text(&invocation, "projectId");
        if !work_id.is_empty() {
            let pinned_artifact = text(record, "kind") == "Artifact"
                && values(&invocation["dispatch"], "inputs")
                    .iter()
                    .any(|input| text(&input["artifact"], "artifactId") == text(record, "id"));
            if text(record, "workId") == work_id
                || text(record, "id") == work_id
                || (text(record, "kind") == "Project" && text(record, "id") == project_id)
                || pinned_artifact
            {
                return Ok(());
            }
            return Err(bad(
                "FORBIDDEN",
                "Record is outside the bound work and its pinned artifact inputs",
            ));
        }
        if !project_id.is_empty()
            && (text(record, "projectId") == project_id || text(record, "id") == project_id)
        {
            return Ok(());
        }
        Err(bad("FORBIDDEN", "Record is outside the invocation scope"))
    }

    fn read(&self, principal: &Principal, request: &Request) -> DomainResult<Value> {
        if request.method == "work.execution_check" {
            closed(&request.params, &["workId"], &[])?;
            let invocation = self.executor_binding(principal, text(&request.params, "workId"))?;
            return Ok(
                json!({"allowed":true,"invocationId":invocation["id"],"turnId":invocation["currentExecutorTurnId"]}),
            );
        }
        let method = request.method.as_str();
        if method == "memory.list" {
            return self.memory_list(principal, request);
        }
        if method == "console.get" {
            Self::human(principal)?;
            closed(&request.params, &[], &[])?;
            return self.global_policy();
        }
        if method.ends_with(".list") {
            let kind = match method {
                "project.list" => "Project",
                "work.list" => "Work",
                "task.list" => "Task",
                "decision.list" => "DecisionRequest",
                "inbox.list" => "AttentionItem",
                "delivery.list" => "DeliveryCandidate",
                _ => return Err(bad("METHOD_UNSUPPORTED", method)),
            };
            let required = if method == "task.list" {
                vec!["workId", "limit"]
            } else {
                vec!["limit"]
            };
            closed(&request.params, &required, &["afterId", "workId"])?;
            let limit = number(&request.params, "limit");
            if !(1..=100).contains(&limit) {
                return Err(bad("INVALID_ARGUMENT", "limit must be 1..100"));
            }
            let mut items: Vec<_> = self
                .all(kind)
                .into_iter()
                .filter(|record| text(record, "id") > text(&request.params, "afterId"))
                .filter(|record| {
                    text(&request.params, "workId").is_empty()
                        || text(record, "workId") == text(&request.params, "workId")
                })
                .filter(|record| self.authorized_read(principal, record).is_ok())
                .collect();
            let more = items.len() > limit as usize;
            items.truncate(limit as usize);
            let mut data =
                json!({"items":items.iter().map(|record|self.view(record)).collect::<Vec<_>>()});
            if more {
                if let Some(last) = items.last() {
                    data["nextAfterId"] = last["id"].clone();
                }
            }
            return Ok(data);
        }
        if method == "workspace.inspect" {
            closed(&request.params, &["workId"], &["candidateId"])?;
            let work = self.record(text(&request.params, "workId"), "Work")?;
            self.authorized_read(principal, &work)?;
            let workspace_id = text(&work, "workspaceId");
            if workspace_id.is_empty() {
                return Err(bad(
                    "BAD_STATE",
                    "Work has no provisioned workspace to inspect",
                ));
            }
            let workspace = self.record(workspace_id, "Workspace")?;
            if text(&workspace, "localRoot").is_empty() {
                return Err(bad("BAD_STATE", "Workspace provisioning has not completed"));
            }
            let candidate_id = request
                .params
                .get("candidateId")
                .and_then(Value::as_str)
                .unwrap_or(text(&work, "currentCandidateId"));
            if candidate_id.is_empty() && request.params.get("candidateId").is_none() {
                let mut artifacts = Vec::<ArtifactRef>::new();
                let mut results = Vec::new();
                let mut recipes = Vec::new();
                for task in self.related("Task", "workId", text(&work, "id")) {
                    if text(&task["resourceRequirements"], "workspaceId") != workspace_id {
                        continue;
                    }
                    recipes.extend(
                        values(&task["contract"], "gateDefinitions")
                            .into_iter()
                            .filter_map(|gate| gate.get("recipe").cloned()),
                    );
                    let result_id = text(&task, "currentResultId");
                    if result_id.is_empty() {
                        continue;
                    }
                    let result = self.record(result_id, "TaskResult")?;
                    let outputs: Vec<ResultOutput> = parse(&result["body"]["outputs"])?;
                    let refs: Vec<_> = outputs.into_iter().map(|output| output.artifact).collect();
                    self.artifacts(&refs)?;
                    for reference in &refs {
                        if !artifacts.iter().any(|existing| {
                            existing.artifact_id == reference.artifact_id
                                && existing.digest == reference.digest
                        }) {
                            artifacts.push(reference.clone());
                        }
                    }
                    results.push(json!({"taskId":task["id"],"resultId":result["id"],
                        "disposition":result["disposition"],"artifacts":refs}));
                }
                let mut destination = json!({"workspaceId":workspace_id,
                    "localRoot":workspace["localRoot"],"kind":work["spec"]["delivery"]["kind"]});
                for field in ["branch", "commitId"] {
                    if let Some(value) = workspace.get(field) {
                        destination[field] = value.clone();
                    }
                }
                return Ok(json!({"workspaceId":workspace_id,"workspace":workspace,
                    "artifacts":artifacts,"results":results,"destination":destination,"runRecipes":recipes}));
            }
            let candidate = self.record(candidate_id, "DeliveryCandidate")?;
            if text(&candidate, "workId") != text(&work, "id") {
                return Err(bad(
                    "INVALID_REFERENCE",
                    "Candidate belongs to another work",
                ));
            }
            return Ok(
                json!({"workspaceId":work["workspaceId"],"workspace":workspace,"candidateId":candidate["id"],"artifacts":candidate["artifacts"],"destination":candidate["destination"],"runRecipes":candidate["runRecipes"]}),
            );
        }
        let (field, kind) = match method {
            "project.get" => ("projectId", "Project"),
            "work.get" => ("workId", "Work"),
            "task.get" => ("taskId", "Task"),
            "result.get" => ("resultId", "TaskResult"),
            "progress.get" => ("reportId", "ProgressReport"),
            "artifact.get" | "artifact.read" => ("artifactId", "Artifact"),
            "workspace.get" => ("workspaceId", "Workspace"),
            "operation.get" => ("operationId", "Operation"),
            "decision.get" => ("decisionId", "DecisionRequest"),
            "delivery.get" => ("candidateId", "DeliveryCandidate"),
            _ => return Err(bad("METHOD_UNSUPPORTED", method)),
        };
        let optional = if method == "artifact.read" {
            &["relativePath", "offset", "limit"][..]
        } else {
            &[][..]
        };
        closed(&request.params, &[field], optional)?;
        let record = self.record(text(&request.params, field), kind)?;
        self.authorized_read(principal, &record)?;
        if method == "artifact.read" {
            return Self::read_artifact(&record, &request.params);
        }
        Ok(self.view(&record))
    }

    pub fn snapshot(&self, scope: &Value) -> DomainResult<Value> {
        self.validate_scope(scope)?;
        match text(scope, "kind") {
            "WorkList" => Ok(
                json!({"items":self.all("Work").iter().map(|work|self.view(work)).collect::<Vec<_>>()}),
            ),
            "Work" => Ok(self.view(&self.record(text(scope, "id"), "Work")?)),
            "Operation" => Ok(self.view(&self.record(text(scope, "id"), "Operation")?)),
            "Conversation" => Ok(self.view(&self.record(text(scope, "id"), "Conversation")?)),
            _ => Err(bad("INVALID_ARGUMENT", "Invalid subscription scope")),
        }
    }

    fn validate_scope(&self, scope: &Value) -> DomainResult<()> {
        if text(scope, "kind") == "WorkList" {
            closed(scope, &["kind"], &[])?;
        } else {
            closed(scope, &["kind", "id"], &[])?;
            uuid(text(scope, "id"))?;
            match text(scope, "kind") {
                "Work" | "Conversation" | "Operation" => {
                    self.record(text(scope, "id"), text(scope, "kind"))?;
                }
                _ => return Err(bad("INVALID_ARGUMENT", "Unknown scope kind")),
            }
        }
        Ok(())
    }

    pub fn events_after(&self, cursor: Option<&str>, scope: &Value) -> DomainResult<Vec<Value>> {
        self.validate_scope(scope)?;
        let position = if let Some(cursor) = cursor {
            let prefix = format!("{}:", self.store_id);
            cursor
                .strip_prefix(&prefix)
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or_else(|| bad("CURSOR_EXPIRED", "Cursor does not belong to this store"))?
        } else {
            0
        };
        let query = || -> Result<Vec<Value>> {
            let max: i64 =
                self.db
                    .query_row("SELECT COALESCE(MAX(sequence),0) FROM events", [], |row| {
                        row.get(0)
                    })?;
            anyhow::ensure!(
                position <= max as u64,
                "Cursor is beyond the committed stream"
            );
            let mut statement = self.db.prepare(
                "SELECT sequence,body FROM events WHERE sequence>?1 AND (
                    (?2='WorkList' AND (json_type(body,'$.workId')='text' OR json_extract(body,'$.subject.kind')='Work'))
                    OR (?2='Work' AND json_extract(body,'$.workId')=?3)
                    OR (?2 IN ('Conversation','Operation') AND json_extract(body,'$.subject.id')=?3)
                    OR (?2='Conversation' AND json_extract(body,'$.subject.kind')='IntakeRequest'
                        AND json_extract(body,'$.changes[0].view.conversationId')=?3)
                 ) ORDER BY sequence",
            )?;
            let mut events = Vec::new();
            for row in statement.query_map(
                params![
                    i64::try_from(position)?,
                    text(scope, "kind"),
                    text(scope, "id")
                ],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )? {
                let (sequence, body) = row?;
                let mut event: Value = serde_json::from_str(&body)?;
                event["cursor"] = json!(format!("{}:{sequence}", self.store_id));
                events.push(event);
            }
            Ok(events)
        };
        query().map_err(|error| bad("RESYNC_REQUIRED", error.to_string()))
    }

    fn mutate(&mut self, principal: &Principal, request: &Request) -> DomainResult<Response> {
        match request.method.as_str() {
            "work.claim_executor" | "work.request_input" => {
                self.executor_command(principal, request)
            }
            "work.open" | "work.continue" => self.work_continuation_command(principal, request),
            "memory.store" | "memory.forget" => self.memory_command(principal, request),
            "console.configure" => self.configure_global_conversation(principal, request),
            "conversation.propose_action" => self.propose_human_action(principal, request),
            "conversation.resolve_input" => self.resolve_global_input(principal, request),
            "work.propose_change" | "work.apply_change" => self.change_command(principal, request),
            "project.configure" | "runtime.register" | "work.create_draft" | "grant.preview"
            | "work.start" | "plan.propose" | "plan.apply" | "work.control"
            | "workspace.takeover" | "workspace.handback" => {
                self.planning_command(principal, request)
            }
            "task.acknowledge" | "runtime.report" | "artifact.capture" => {
                self.runtime_command(principal, request)
            }
            "result.submit"
            | "gate.submit"
            | "review.submit"
            | "result.accept"
            | "result.request_changes"
            | "result.reject"
            | "task.rework"
            | "delivery.prepare"
            | "delivery.accept"
            | "delivery.request_changes" => self.evaluation_command(principal, request),
            "conversation.submit"
            | "console.open"
            | "conversation.resolve_intents"
            | "conversation.request_input"
            | "conversation.answer_input"
            | "task.report_progress"
            | "task.request_context"
            | "task.answer_context"
            | "coordination.finish"
            | "decision.request"
            | "decision.answer" => self.collaboration_command(principal, request),
            _ => Err(bad(
                "METHOD_UNSUPPORTED",
                format!(
                    "{} is not implemented by the experimental work engine",
                    request.method
                ),
            )),
        }
    }

    fn drive(&mut self) -> DomainResult<()> {
        // Controllers only admit recorded work; effects are dispatched after the enclosing commit.
        self.settle_workspace_operations()?;
        self.settle_spec_changes()?;
        self.settle_work_continuations()?;
        self.drive_executors()?;
        self.evaluate_ready()?;
        self.schedule()?;
        self.coordinate()?;
        self.finalize_works()?;
        Ok(())
    }
}
