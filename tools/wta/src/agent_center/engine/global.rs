// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

use super::*;

impl Engine {
    pub(super) fn global_policy(&self) -> DomainResult<Value> {
        let policies = self.all("GlobalConversationPolicy");
        if policies.is_empty() {
            return Err(bad(
                "ASSISTANT_UNAVAILABLE",
                "No approved assistant is configured for this conversation. Choose an assistant and confirm its data destination before sending.",
            ));
        }
        if policies.len() != 1 {
            return Err(bad(
                "INVALID_REFERENCE",
                "Global assistant policy is ambiguous",
            ));
        }
        Ok(policies[0].clone())
    }

    pub(in crate::agent_center) fn validate_limits(limits: &Limits) -> DomainResult<()> {
        for (key, value) in encode(limits)?.as_object().into_iter().flatten() {
            let value = value.as_u64().unwrap_or(0);
            if value == 0 || value > 86_400 {
                return Err(bad(
                    "INVALID_ARGUMENT",
                    format!("Finite nonzero limit required: {key}"),
                ));
            }
        }
        if limits.concurrency > 64 || limits.context_rounds > 64 {
            return Err(bad(
                "INVALID_ARGUMENT",
                "Experiment supports at most 64 execution slots/context rounds",
            ));
        }
        Ok(())
    }

    pub(super) fn configure_global_conversation(
        &mut self,
        principal: &Principal,
        request: &Request,
    ) -> DomainResult<Response> {
        Self::human(principal)?;
        let input: ConsoleConfigure = parse(&request.params)?;
        nonempty(
            &input.approved_model_destination,
            "approvedModelDestination",
        )?;
        if input.approved_model_destination.len() > 1024
            || input
                .approved_model_destination
                .chars()
                .any(char::is_control)
        {
            return Err(bad(
                "INVALID_ARGUMENT",
                "Invalid approved model destination",
            ));
        }
        Self::validate_limits(&input.limits)?;
        for (capability, kind) in [
            (&input.capability_id, "Coordinate"),
            (&input.worker_capability_id, "ProduceResult"),
            (&input.check_capability_id, "EvaluateGate"),
        ] {
            if self.runtime_for(capability, kind).is_none() {
                return Err(bad(
                    "CAPABILITY_UNAVAILABLE",
                    "The approved assistant capability is not available",
                ));
            }
        }
        let body = encode(&input)?;
        let existing = self.all("GlobalConversationPolicy");
        if existing.len() > 1 {
            return Err(bad(
                "INVALID_REFERENCE",
                "Global assistant policy is ambiguous",
            ));
        }
        let policy = if let Some(existing) = existing.first() {
            let unchanged = body.as_object().is_some_and(|fields| {
                fields
                    .iter()
                    .all(|(key, value)| existing.get(key) == Some(value))
            });
            if unchanged {
                if !request.if_match.is_empty() {
                    self.matched(request, &[existing])?;
                }
                return Ok(Response::ok("", existing.clone()));
            }
            if matches!(principal, Principal::Service) && request.if_match.is_empty() {
                // Startup may refresh human-installed approval metadata only
                // after the old global invocations have fully released.
            } else {
                self.matched(request, &[existing])?;
            }
            if self.all("Invocation").iter().any(|invocation| {
                invocation["scope"] == "Global" && invocation["state"] != "Released"
            }) {
                return Err(bad(
                    "BAD_STATE",
                    "The current assistant must settle before changing its approved configuration",
                ));
            }
            let mut updated = existing.clone();
            for (key, value) in body.as_object().into_iter().flatten() {
                updated[key] = value.clone();
            }
            self.put(updated)
        } else {
            self.matched(request, &[])?;
            self.create("GlobalConversationPolicy", body)
        };
        Ok(Response::ok("", policy))
    }

    pub(super) fn global_coordinator(&self, principal: &Principal) -> DomainResult<Value> {
        let Principal::Invocation { invocation_id } = principal else {
            return Err(bad("FORBIDDEN", "Requires a bound global conversation"));
        };
        let invocation = self.record(invocation_id, "Invocation")?;
        if invocation["scope"] != "Global"
            || invocation["subject"]["kind"] != "Coordination"
            || invocation["state"] != "Running"
        {
            return Err(bad(
                "FORBIDDEN",
                "Global conversation binding is not active",
            ));
        }
        let policy = self.global_policy()?;
        if invocation["globalPolicyId"] != policy["id"]
            || number(&invocation, "globalPolicyVersion") != number(&policy, "version")
        {
            return Err(bad("FORBIDDEN", "Global conversation approval is obsolete"));
        }
        let conversation = self.record(text(&invocation, "conversationId"), "Conversation")?;
        if conversation["scope"] != "Global" {
            return Err(bad("FORBIDDEN", "Conversation is not globally bound"));
        }
        Ok(invocation)
    }

    pub(super) fn authorized_global_read(
        &self,
        principal: &Principal,
        record: &Value,
    ) -> DomainResult<()> {
        let invocation = self.global_coordinator(principal)?;
        let conversation = text(&invocation, "conversationId");
        if (record["kind"] == "Conversation" && text(record, "id") == conversation)
            || (!conversation.is_empty() && text(record, "conversationId") == conversation)
        {
            return Ok(());
        }
        // A different window's conversation is not project data, even when it
        // carries the same project hint.
        if matches!(
            text(record, "kind"),
            "Conversation" | "ConsoleSession" | "IntakeRequest" | "HumanActionProposal"
        ) || (record["kind"] == "ConversationItem" && !text(record, "conversationId").is_empty())
        {
            return Err(bad("FORBIDDEN", "Record belongs to another conversation"));
        }
        let project = if record["kind"] == "Project" {
            text(record, "id")
        } else if !text(record, "workId").is_empty() {
            self.records
                .get(text(record, "workId"))
                .filter(|work| work["kind"] == "Work")
                .map(|work| text(work, "projectId"))
                .unwrap_or("")
        } else {
            text(record, "projectId")
        };
        if !project.is_empty()
            && values(&invocation, "authorizedProjectIds").contains(&json!(project))
        {
            return Ok(());
        }
        Err(bad(
            "FORBIDDEN",
            "Record is outside captured conversation access",
        ))
    }

    pub(super) fn proposal_coordinator(
        &self,
        principal: &Principal,
        work_id: &str,
    ) -> DomainResult<()> {
        if let Principal::Invocation { invocation_id } = principal {
            if self.record(invocation_id, "Invocation")?["scope"] == "Global" {
                self.global_coordinator(principal)?;
                return self.authorized_read(principal, &self.record(work_id, "Work")?);
            }
        }
        self.coordinator(principal, Some(work_id)).map(|_| ())
    }

    pub(super) fn coordinate_global(&mut self, trigger: &Value) -> DomainResult<()> {
        let conversation_id = text(trigger, "conversationId");
        let mut conversation = self.record(conversation_id, "Conversation")?;
        let policy = self.global_policy()?;
        let remaining = number(&policy["limits"], "coordinationTurns")
            .saturating_sub(number(&conversation, "coordinationUsed"));
        if remaining == 0 {
            self.attention(
                "",
                conversation_id,
                "CoordinationAllowanceExhausted",
                "Review the assistant allowance before continuing this conversation",
            );
            return Ok(());
        }
        let Some(runtime) = self.runtime_for(text(&policy, "capabilityId"), "Coordinate") else {
            self.attention(
                "",
                conversation_id,
                "CoordinatorUnavailable",
                "The approved assistant is unavailable; no execution was started",
            );
            return Ok(());
        };
        let triggers: Vec<_> = self
            .related("CoordinationTrigger", "scopeId", conversation_id)
            .into_iter()
            .filter(|trigger| trigger["status"] == "Queued")
            .collect();
        if triggers.is_empty() {
            return Ok(());
        }
        let turn_id = self
            .related("CoordinationTurn", "scopeId", conversation_id)
            .into_iter()
            .find(|turn| turn["state"] == "Queued" && turn.get("invocationId").is_none())
            .map(|turn| text(&turn, "id").to_owned())
            .unwrap_or_else(id);
        let invocation_id = id();
        let message = self.create(
            "ConversationItem",
            json!({
                "conversationId":conversation_id,"role":"assistant","parts":[],
                "status":"Streaming","invocationId":invocation_id
            }),
        );
        let projects: Vec<_> = self
            .all("Project")
            .into_iter()
            .filter(|project| {
                project["coordinatorCapabilityId"] == policy["capabilityId"]
                    || project["workerCapabilityId"] == policy["capabilityId"]
                    || values(project, "capabilityIds").contains(&policy["capabilityId"])
            })
            .collect();
        let authorized: Vec<_> = projects
            .iter()
            .map(|project| project["id"].clone())
            .collect();
        let mut snapshot = self.view(&conversation);
        let preference_project = values(&snapshot, "messages")
            .iter()
            .rev()
            .find(|message| message["role"] == "human")
            .and_then(|message| message["context"]["projectId"].as_str())
            .filter(|project| authorized.contains(&json!(project)))
            .map(str::to_owned);
        snapshot["preferences"] = self.memory_snapshot(preference_project.as_deref());
        snapshot["scope"] = json!("Global");
        snapshot["policy"] = json!({
            "capabilityId":policy["capabilityId"],"workerCapabilityId":policy["workerCapabilityId"],
            "checkCapabilityId":policy["checkCapabilityId"],"limits":policy["limits"]
        });
        snapshot["projects"] = json!(projects.iter().take(100).map(|project| json!({
            "id":project["id"],"version":project["version"],"name":project["name"],"root":project["root"]
        })).collect::<Vec<_>>());
        let works: Vec<_> = self
            .all("Work")
            .into_iter()
            .filter(|work| authorized.contains(&work["projectId"]))
            .collect();
        snapshot["works"] = json!(works
            .iter()
            .take(100)
            .map(|work| json!({
                "id":work["id"],"version":work["version"],"projectId":work["projectId"],
                "goal":work["spec"]["goal"],"lifecycle":work["lifecycle"],
                "desiredAdvancement":work["desiredAdvancement"]
            }))
            .collect::<Vec<_>>());
        snapshot["projectSummaryTruncated"] = json!(projects.len() > 100);
        snapshot["workSummaryTruncated"] = json!(works.len() > 100);
        snapshot["projectAccessIsScoped"] = json!(true);
        let input = json!({
            "turnId":turn_id,"scope":{"conversationId":conversation_id},
            "replyMessageId":message["id"],"snapshotVersion":number(&conversation,"version"),
            "triggerEvents":triggers.iter().map(|trigger|json!({
                "eventId":trigger["eventId"],"kind":trigger["reason"],
                "subject":{"kind":trigger["subjectKind"],"id":trigger["subjectId"],"version":trigger["subjectVersion"]}
            })).collect::<Vec<_>>(),"snapshot":snapshot,"remainingAllowance":remaining
        });
        self.put(json!({"id":turn_id,"kind":"CoordinationTurn","scopeId":conversation_id,
            "conversationId":conversation_id,"invocationId":invocation_id,
            "replyMessageId":message["id"],"input":input,"state":"Queued","createdAt":utc_after(0)}));
        let invocation = self.put(json!({
            "id":invocation_id,"kind":"Invocation","scope":"Global","conversationId":conversation_id,
            "globalPolicyId":policy["id"],"globalPolicyVersion":policy["version"],
            "authorizedProjectIds":authorized,"subject":{"kind":"Coordination","id":turn_id},
            "runtimeId":runtime["id"],"capabilityId":policy["capabilityId"],
            "coordinationInput":input,"replyMessageId":message["id"],"bindingGeneration":1,
            "limits":{"deadlineUtc":utc_after(number(&policy["limits"],"coordinationSeconds")),
                "remainingExecutionAllowance":remaining,"remainingContextRounds":policy["limits"]["contextRounds"]},
            "availableToolNames":["memory_list","memory_store","memory_forget",
                "project_list","project_get","work_list","work_get","task_list",
                "task_get","progress_get","result_get","artifact_get","artifact_read","delivery_list",
                "delivery_get","decision_list","decision_get","inbox_list","operation_get","work_create_draft",
                "work_propose_change","grant_preview","conversation_resolve_intents",
                "conversation_request_input","conversation_resolve_input",
                "conversation_propose_action","coordination_finish"],
            "state":"Dispatching","lastSequence":0,"createdAt":utc_after(0)
        }));
        for mut trigger in triggers {
            trigger["status"] = json!("Captured");
            trigger["turnId"] = json!(turn_id);
            self.put(trigger);
        }
        conversation["coordinationUsed"] = json!(number(&conversation, "coordinationUsed") + 1);
        self.put(conversation);
        self.emit_message("MessageRecorded", &message);
        self.resolve_attention(conversation_id);
        self.effect(
            "runtime.invoke",
            json!({"invocation":Self::wire_invocation(&invocation)?}),
            None,
        );
        Ok(())
    }
}
