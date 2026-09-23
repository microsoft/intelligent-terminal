// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

use super::*;

const SNAPSHOT_BYTES: usize = 8192;
const MAX_SCOPE_RECORDS: usize = 512;

impl Engine {
    fn memory_forget_version(&self) -> u64 {
        self.records
            .values()
            .find(|record| record["kind"] == "PreferenceEpoch")
            .map_or(0, |record| number(record, "version"))
    }

    fn memory_authorize(
        &self,
        principal: &Principal,
        project_id: Option<&str>,
    ) -> DomainResult<Option<Value>> {
        if let Some(project_id) = project_id {
            uuid(project_id)?;
            self.record(project_id, "Project")?;
        }
        if matches!(principal, Principal::Human | Principal::Service) {
            return Ok(None);
        }
        let turn = self.coordinator(principal, None)?;
        let Principal::Invocation { invocation_id } = principal else {
            return Err(bad("FORBIDDEN", "Memory requires a bound coordinator"));
        };
        let invocation = self.record(invocation_id, "Invocation")?;
        if let Some(project_id) = project_id {
            if invocation["scope"] == "Global" {
                self.authorized_global_read(principal, &self.record(project_id, "Project")?)?;
            } else if text(&invocation, "projectId") != project_id {
                return Err(bad("FORBIDDEN", "Preference belongs to another project"));
            }
        }
        if turn["invocationId"] != invocation["id"] {
            return Err(bad("FORBIDDEN", "Memory coordinator binding is obsolete"));
        }
        Ok(Some(invocation))
    }

    fn memory_source(
        &self,
        invocation: Option<&Value>,
        source_id: Option<&str>,
        storing: bool,
    ) -> DomainResult<()> {
        let Some(invocation) = invocation else {
            if let Some(source_id) = source_id {
                uuid(source_id)?;
                let message = self.record(source_id, "ConversationItem")?;
                if message["role"] != "human" {
                    return Err(bad("FORBIDDEN", "Preference source must be human input"));
                }
            }
            return Ok(());
        };
        if storing
            && invocation
                .pointer("/coordinationInput/snapshot/preferences/forgetVersion")
                .and_then(Value::as_u64)
                != Some(self.memory_forget_version())
        {
            return Err(bad(
                "STALE_VERSION",
                "Preferences were forgotten after this invocation was captured; wait for fresh human input",
            ));
        }
        let source_id = source_id.ok_or_else(|| {
            bad(
                "INVALID_ARGUMENT",
                "Agent memory writes require sourceMessageId",
            )
        })?;
        uuid(source_id)?;
        let message = self.record(source_id, "ConversationItem")?;
        let snapshot = &invocation["coordinationInput"]["snapshot"];
        let captured = values(snapshot, "messages")
            .into_iter()
            .rev()
            .find(|message| message["role"] == "human")
            .ok_or_else(|| bad("FORBIDDEN", "No captured human input for memory writing"))?;
        let conversation = self.record(text(&message, "conversationId"), "Conversation")?;
        let latest = values(&conversation, "messages")
            .into_iter()
            .rev()
            .find(|message| message["role"] == "human");
        let source_matches = [
            "id",
            "kind",
            "role",
            "conversationId",
            "clientMessageId",
            "text",
            "attachments",
            "context",
            "declaredIntent",
        ]
        .iter()
        .all(|field| captured.get(*field) == message.get(*field));
        let trigger_matches = values(&invocation["coordinationInput"], "triggerEvents")
            .iter()
            .any(|trigger| {
                trigger["kind"] == "IntakeMessage"
                    && trigger["subject"]["kind"] == "ConversationItem"
                    && trigger["subject"]["id"] == message["id"]
            });
        if message["role"] != "human"
            || !source_matches
            || !trigger_matches
            || latest
                .as_ref()
                .is_none_or(|latest| latest["id"] != message["id"])
            || invocation["coordinationInput"]["scope"]["conversationId"]
                != message["conversationId"]
        {
            return Err(bad(
                "FORBIDDEN",
                "Memory source must be the latest human input captured by this intake invocation",
            ));
        }
        Ok(())
    }

    fn memory_items(&self, project_id: Option<&str>, active_only: bool) -> Vec<Value> {
        self.records
            .values()
            .filter(|record| {
                record["kind"] == "Preference"
                    && (!active_only || record["status"] == "Active")
                    && (record["scope"] == "User"
                        || (project_id.is_some()
                            && record.get("projectId").and_then(Value::as_str) == project_id))
            })
            .cloned()
            .collect()
    }

    pub(super) fn memory_snapshot(&self, project_id: Option<&str>) -> Value {
        let mut items = Vec::new();
        let mut bytes = 2;
        let mut truncated = false;
        let mut preferences = self.memory_items(project_id, true);
        preferences.sort_by_key(|record| {
            (
                record["key"] != "workspace.code_root",
                text(record, "id").to_owned(),
            )
        });
        for record in preferences {
            // Values are already serializable JSON; count actual escaped UTF-8 bytes.
            let size = record.to_string().len() + usize::from(!items.is_empty());
            if bytes + size > SNAPSHOT_BYTES {
                truncated = true;
                break;
            }
            bytes += size;
            items.push(record);
        }
        json!({
            "items":items,"truncated":truncated,"forgetVersion":self.memory_forget_version()
        })
    }

    pub(super) fn memory_list(
        &self,
        principal: &Principal,
        request: &Request,
    ) -> DomainResult<Value> {
        let input: MemoryList = parse(&request.params)?;
        self.memory_authorize(principal, input.project_id.as_deref())?;
        let limit = input.limit.unwrap_or(50);
        if !(1..=100).contains(&limit) {
            return Err(bad("INVALID_ARGUMENT", "Memory list limit must be 1..100"));
        }
        if let Some(after) = &input.after_id {
            uuid(after)?;
        }
        let mut items = self.memory_items(input.project_id.as_deref(), false);
        items.retain(|item| {
            input
                .after_id
                .as_deref()
                .is_none_or(|after| text(item, "id") > after)
        });
        let more = items.len() > limit as usize;
        items.truncate(limit as usize);
        let mut result = json!({"items":items});
        if more {
            result["nextAfterId"] = result["items"][limit as usize - 1]["id"].clone();
        }
        Ok(result)
    }

    pub(super) fn memory_command(
        &mut self,
        principal: &Principal,
        request: &Request,
    ) -> DomainResult<Response> {
        if request.method == "memory.forget" {
            let input: MemoryForget = parse(&request.params)?;
            uuid(&input.preference_id)?;
            let mut record = self.record(&input.preference_id, "Preference")?;
            let invocation =
                self.memory_authorize(principal, record.get("projectId").and_then(Value::as_str))?;
            self.memory_source(
                invocation.as_ref(),
                input.source_message_id.as_deref(),
                false,
            )?;
            self.matched(request, &[&record])?;
            if record["status"] == "Forgotten" {
                return Ok(Response::ok("", record));
            }
            record["status"] = json!("Forgotten");
            record
                .as_object_mut()
                .ok_or_else(|| bad("INVALID_REFERENCE", "Invalid preference"))?
                .remove("content");
            record["forgottenSourceMessageId"] = json!(input.source_message_id);
            record["updatedAt"] = json!(utc_after(0));
            let record = self.put(record);
            if let Some(epoch) = self.all("PreferenceEpoch").into_iter().next() {
                self.put(epoch);
            } else {
                self.create("PreferenceEpoch", json!({}));
            }
            self.emit("PreferenceForgotten", &record);
            return Ok(Response::ok("", record));
        }

        let input: MemoryStore = parse(&request.params)?;
        if input.key.is_empty()
            || input.key.len() > 80
            || !input.key.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'.' | b'-' | b'_')
            })
        {
            return Err(bad("INVALID_ARGUMENT", "Preference key must be 1..80 lowercase ASCII letters, digits, dots, dashes or underscores"));
        }
        nonempty(&input.content, "content")?;
        if input.content.chars().count() > 512 {
            return Err(bad(
                "INVALID_ARGUMENT",
                "Preference content exceeds 512 characters",
            ));
        }
        if matches!(input.scope, PreferenceScope::Project) != input.project_id.is_some() {
            return Err(bad(
                "INVALID_ARGUMENT",
                "Only Project scope requires projectId",
            ));
        }
        let invocation = self.memory_authorize(principal, input.project_id.as_deref())?;
        self.memory_source(
            invocation.as_ref(),
            input.source_message_id.as_deref(),
            true,
        )?;
        let prior = self.all("Preference").into_iter().find(|record| {
            text(record, "key") == input.key
                && record.get("projectId").and_then(Value::as_str) == input.project_id.as_deref()
        });
        self.matched(request, &prior.iter().collect::<Vec<_>>())?;
        if let Some(prior) = &prior {
            if invocation.is_some()
                && prior["status"] == "Forgotten"
                && (prior.get("sourceMessageId").and_then(Value::as_str)
                    == input.source_message_id.as_deref()
                    || prior
                        .get("forgottenSourceMessageId")
                        .and_then(Value::as_str)
                        == input.source_message_id.as_deref())
            {
                return Err(bad(
                    "FORBIDDEN",
                    "Restoring a forgotten preference requires fresh human input",
                ));
            }
        } else if self
            .all("Preference")
            .iter()
            .filter(|record| {
                record.get("projectId").and_then(Value::as_str) == input.project_id.as_deref()
            })
            .count()
            >= MAX_SCOPE_RECORDS
        {
            return Err(bad(
                "CAPACITY_EXCEEDED",
                "Preference scope has reached its record limit; reuse existing keys",
            ));
        }
        let mut record = encode(&input)?;
        record["status"] = json!("Active");
        record["updatedAt"] = json!(utc_after(0));
        let record = if let Some(prior) = prior {
            record["id"] = prior["id"].clone();
            record["kind"] = json!("Preference");
            record["createdAt"] = prior["createdAt"].clone();
            self.put(record)
        } else {
            self.create("Preference", record)
        };
        self.emit("PreferenceStored", &record);
        Ok(Response::ok("", record))
    }
}
