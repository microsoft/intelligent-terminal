// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

//! Agent Center v1 values shared by the service and its adapters.

use serde::{Deserialize, Serialize};
use serde_json::Value;

fn request_type() -> String {
    "request".into()
}

pub fn is_read_method(method: &str) -> bool {
    super::schemas::is_read(method)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Request {
    #[serde(rename = "type", default = "request_type")]
    pub message_type: String,
    pub request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
    pub method: String,
    pub if_match: Vec<EntityRef>,
    pub params: Value,
}

impl Request {
    pub fn new(method: impl Into<String>, params: Value) -> Self {
        Self {
            message_type: request_type(),
            request_id: uuid::Uuid::new_v4().to_string(),
            command_id: None,
            method: method.into(),
            if_match: Vec::new(),
            params,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EntityRef {
    pub kind: String,
    pub id: String,
    pub version: u64,
}

/// This binding is supplied by the authenticated transport, never request JSON.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Principal {
    Human,
    Runtime { runtime_id: String },
    Invocation { invocation_id: String },
    Service,
}

impl Principal {
    pub fn key(&self) -> String {
        match self {
            Self::Human => "human".into(),
            Self::Service => "service".into(),
            Self::Runtime { runtime_id } => format!("runtime:{runtime_id}"),
            Self::Invocation { invocation_id } => format!("invocation:{invocation_id}"),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FieldError {
    pub path: String,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Failure {
    pub code: String,
    pub message: String,
    pub field_errors: Vec<FieldError>,
    pub recovery: String,
    pub subjects: Vec<EntityRef>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InputRequest {
    pub kind: String,
    pub id: String,
    pub version: u64,
    pub response_schema: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Response {
    #[serde(rename = "type")]
    pub message_type: String,
    pub request_id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<Failure>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_request: Option<InputRequest>,
    pub subjects: Vec<EntityRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

impl Response {
    pub fn ok(request_id: impl Into<String>, data: Value) -> Self {
        Self {
            message_type: "response".into(),
            request_id: request_id.into(),
            status: "ok".into(),
            data: Some(data),
            failure: None,
            operation_id: None,
            input_request: None,
            subjects: Vec::new(),
            cursor: None,
        }
    }

    pub fn pending(request_id: impl Into<String>, operation_id: String, data: Value) -> Self {
        let mut response = Self::ok(request_id, data);
        response.status = "pending".into();
        response.operation_id = Some(operation_id);
        response
    }

    pub fn needs_input(request_id: impl Into<String>, input: InputRequest) -> Self {
        let mut response = Self::ok(request_id, Value::Null);
        response.status = "needs_input".into();
        response.data = None;
        response.input_request = Some(input);
        response
    }

    pub fn fail(request_id: impl Into<String>, code: &str, message: impl Into<String>) -> Self {
        let mut response = Self::ok(request_id, Value::Null);
        response.status = match code {
            "STALE_VERSION" | "STALE_DISPATCH" | "STALE_EVALUATION" => "conflict",
            "METHOD_UNSUPPORTED" | "CAPABILITY_UNAVAILABLE" => "unsupported",
            _ => "error",
        }
        .into();
        response.data = None;
        response.failure = Some(Failure {
            code: code.into(),
            message: message.into(),
            field_errors: Vec::new(),
            recovery: match code {
                "STALE_VERSION" | "STALE_DISPATCH" | "STALE_EVALUATION" | "RESYNC_REQUIRED"
                | "CURSOR_EXPIRED" => "refresh",
                "OUTCOME_UNKNOWN" => "repair",
                _ => "none",
            }
            .into(),
            subjects: Vec::new(),
        });
        response
    }
}

macro_rules! payload {
    (@fields $name:ident [$($fields:tt)*]) => {
        #[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        pub struct $name { $($fields)* }
    };
    (@fields $name:ident [$($fields:tt)*] $field:ident: Option<$ty:ty> $(, $($rest:tt)*)?) => {
        payload!(@fields $name [
            $($fields)*
            #[serde(skip_serializing_if = "Option::is_none")]
            pub $field: Option<$ty>,
        ] $($($rest)*)?);
    };
    (@fields $name:ident [$($fields:tt)*] $field:ident: $ty:ty $(, $($rest:tt)*)?) => {
        payload!(@fields $name [$($fields)* pub $field: $ty,] $($($rest)*)?);
    };
    ($name:ident { $($fields:tt)* }) => {
        payload!(@fields $name [] $($fields)*);
    };
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub enum PreferenceScope {
    User,
    Project,
}

payload!(MemoryList {
    project_id: Option<String>, after_id: Option<String>, limit: Option<u64>
});
payload!(MemoryStore {
    key: String, scope: PreferenceScope, project_id: Option<String>,
    content: String, source_message_id: Option<String>
});
payload!(MemoryForget {
    preference_id: String, source_message_id: Option<String>
});

payload!(ArtifactRef {
    artifact_id: String,
    digest: String
});
payload!(InputRef {
    slot: String, artifact: ArtifactRef, source_result_id: Option<String>, source_gate_ids: Vec<String>
});
payload!(IntegrationBase { head_generation: u64, parent_result_id: Option<String> });
payload!(TaskDispatch {
    id: String, work_id: String, task_id: String, task_revision: u64, attempt_id: String,
    invocation_id: String, kind: String, role: String, subject_result_id: Option<String>,
    evaluation_round: Option<u64>, evaluation_unit_id: Option<String>, gate_definition_id: Option<String>,
    evidence_manifest_digest: Option<String>, integration_base: Option<IntegrationBase>,
    objective: String, scope: Vec<String>, exclusions: Vec<String>, inputs: Vec<InputRef>,
    input_manifest_digest: String, context_snapshot_id: String, workspace_id: String,
    outputs: Vec<OutputContract>, criteria: Vec<Criterion>, gate_definitions: Vec<GateDefinition>,
    review_policy: ReviewPolicy, effective_grant_id: String, contract_digest: String,
    rework: Option<ReworkInstruction>, reporting_contract_version: u64
});
payload!(InvocationSubject {
    kind: String,
    id: String
});
payload!(InvocationLimits {
    deadline_utc: String,
    remaining_execution_allowance: u64,
    remaining_context_rounds: u64
});
payload!(CoordinationScope { work_id: Option<String>, conversation_id: String });
payload!(TriggerEvent {
    event_id: String,
    kind: String,
    subject: EntityRef
});
payload!(CoordinationInput {
    turn_id: String, scope: CoordinationScope, reply_message_id: String, snapshot_version: u64,
    trigger_events: Vec<TriggerEvent>, snapshot: Value, remaining_allowance: u64
});
payload!(Invocation {
    id: String, subject: InvocationSubject, runtime_id: String, capability_id: String,
    session_reuse_ref: Option<String>, dispatch: Option<TaskDispatch>,
    coordination_input: Option<CoordinationInput>, reply_message_id: Option<String>,
    executor_input: Option<Value>,
    transcript_message_id: Option<String>, binding_generation: u64, limits: InvocationLimits,
    available_tool_names: Vec<String>
});
payload!(Continuation {
    id: String, request_id: String, answer_id: String, dispatch_id: String, task_revision: u64,
    binding_generation: u64, answer: String, evidence: Vec<ArtifactRef>,
    compatible_input_manifest_digest: String
});
payload!(OutputContract {
    slot: String,
    kind: String,
    required: bool
});
impl OutputContract {
    pub const KINDS: &'static [&'static str] =
        &["File", "Tree", "GitCommit", "Report", "Code", "Evidence"];
}
payload!(Criterion {
    id: String, description: String, evidence_rule: Option<String>,
    required_evidence: Vec<String>
});
payload!(WorkCriterion {
    id: String,
    description: String,
    evidence_rule: String
});
payload!(Recipe {
    executable: String, args: Vec<String>, cwd_relative: String, environment_ref: String,
    timeout_seconds: u64, evidence_parser_id: String
});
payload!(GateDefinition {
    id: String, revision: u64, criterion_ids: Vec<String>, kind: String, required: bool,
    recipe: Option<Recipe>, decision_schema_ref: Option<String>
});
payload!(ReviewPolicy {
    revision: u64, required: bool, reviewer_capability_id: Option<String>, rule: String
});

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum InputSource {
    Artifact {
        artifact: ArtifactRef,
    },
    Dependency {
        #[serde(rename = "sourceTaskKey")]
        source_task_key: String,
        #[serde(rename = "outputSlot")]
        output_slot: String,
    },
}
payload!(InputSlot {
    slot: String,
    source: InputSource
});
payload!(Resources {
    workspace_id: String,
    mode: String
});
payload!(TaskContract {
    client_key: String, existing_task_id: Option<String>, role: String, objective: String,
    scope: Vec<String>, exclusions: Vec<String>, input_slots: Vec<InputSlot>,
    outputs: Vec<OutputContract>, criteria: Vec<Criterion>, gate_definitions: Vec<GateDefinition>,
    review_policy: ReviewPolicy, capability_id: String, required_for_delivery: bool,
    resource_requirements: Resources
});
payload!(DependencyContract {
    source_task_key: String, output_slot: String, consumer_task_key: String,
    condition: String, required_gate_ids: Vec<String>
});
payload!(Delivery { kind: String, destination_workspace_id: Option<String> });
payload!(CreateDraft {
    project_id: String, goal: String, scope: Vec<String>, exclusions: Vec<String>,
    criteria: Vec<WorkCriterion>, context: Vec<ArtifactRef>, delivery: Delivery,
    source_message_ids: Vec<String>, execution_mode: Option<String>
});
payload!(ReplacementSpec {
    revision: u64, project_id: String, goal: String, scope: Vec<String>, exclusions: Vec<String>,
    criteria: Vec<WorkCriterion>, context: Vec<ArtifactRef>, delivery: Delivery,
    source_message_ids: Vec<String>
});
payload!(WorkProposeChange {
    work_id: String, replacement_spec: ReplacementSpec, affected_task_ids: Vec<String>, reason: String
});
payload!(WorkApplyChange {
    proposal_id: String,
    grant_proposal_id: String
});
payload!(Limits {
    concurrency: u64,
    execution_attempts: u64,
    evaluation_attempts: u64,
    coordination_turns: u64,
    context_rounds: u64,
    execution_seconds: u64,
    coordination_seconds: u64
});
payload!(ProjectConfigure {
    project_id: Option<String>, name: String, root: String,
    create_directory: Option<bool>, root_preference: Option<EntityRef>,
    coordinator_capability_id: String, worker_capability_id: String, check_capability_id: String,
    limits: Limits, capability_ids: Option<Vec<String>>, environment_refs: Option<Vec<String>>
});
payload!(Capability {
    id: String, kinds: Vec<String>, supports_continuation: bool, supports_scoped_stop: bool
});
payload!(RuntimeRegister {
    runtime_instance_id: String, protocol_versions: Vec<u64>, capabilities: Vec<Capability>
});
payload!(GrantPreview {
    work_id: String,
    spec_revision: u64,
    policy_revision: u64
});
payload!(WorkStart {
    work_id: String,
    spec_revision: u64,
    project_policy_revision: u64,
    grant_proposal_id: String
});
payload!(PlanPropose {
    work_id: String, based_on_plan_revision: Option<u64>, tasks: Vec<TaskContract>,
    edges: Vec<DependencyContract>, integration_task_key: String, reason: String
});
payload!(PlanApply {
    proposal_id: String
});
payload!(Acknowledge {
    dispatch_id: String, task_revision: u64, continuation_id: Option<String>,
    disposition: String, reason: Option<String>
});
payload!(CoordinationRequest { reason: String, affected_task_ids: Vec<String>, proposed_action: String });
payload!(Progress {
    dispatch_id: String, task_revision: u64, activity: String, findings: Vec<String>,
    artifacts: Vec<ArtifactRef>, next_step: String, blocker_request_id: Option<String>,
    coordination_request: Option<CoordinationRequest>
});
payload!(ContextTarget { kind: String, task_id: Option<String> });
payload!(RequestContext {
    dispatch_id: String, task_revision: u64, question: String, target: ContextTarget,
    inputs: Vec<ArtifactRef>, blocking: bool
});
payload!(AnswerContext {
    request_id: String, answer: String, evidence: Vec<ArtifactRef>, compatibility: String
});
payload!(CaptureSource { kind: String, relative_path: Option<String>, commit_id: Option<String> });
payload!(Capture { workspace_id: String, sources: Vec<CaptureSource>, purpose: String });
payload!(ResultOutput {
    slot: String,
    artifact: ArtifactRef
});
payload!(CriterionEvidence { criterion_id: String, evidence: Vec<ArtifactRef>, claim: String });
payload!(TaskResultBody {
    dispatch_id: String, task_revision: u64, input_manifest_digest: String,
    outputs: Vec<ResultOutput>, criterion_evidence: Vec<CriterionEvidence>, summary: String,
    known_gaps: Vec<String>, supersedes_result_id: Option<String>
});
payload!(GateSubmission {
    evaluation_unit_id: String, dispatch_id: Option<String>, decision_application_id: Option<String>,
    task_revision: u64, subject_result_id: String, evaluation_round: u64,
    gate_definition_id: String, gate_definition_revision: u64, input_manifest_digest: String,
    outcome: String, evidence: Vec<ArtifactRef>, explanation: String
});
payload!(ReviewFinding {
    criterion_id: String, evidence: Vec<ArtifactRef>, explanation: String, requested_change: String
});
payload!(ReviewSubmission {
    dispatch_id: String, evaluation_unit_id: String, task_revision: u64, subject_result_id: String,
    evaluation_round: u64, evidence_manifest_digest: String, gate_result_ids: Vec<String>,
    recommendation: String, findings: Vec<ReviewFinding>, preserve_artifacts: Vec<ArtifactRef>
});
payload!(ReworkFinding { criterion_id: String, evidence: Vec<ArtifactRef>, requested_change: String });
payload!(ReworkInstruction {
    result_id: String, review_ids: Vec<String>, gate_result_ids: Vec<String>, reason: String,
    findings: Vec<ReworkFinding>, preserve_artifacts: Vec<ArtifactRef>, action: String
});
payload!(Rework {
    task_id: String, result_id: String, rework_id: String, action: String, capability_id: Option<String>
});
payload!(RuntimeReport {
    observation_id: String,
    invocation_id: String,
    binding_generation: u64,
    sequence: u64,
    kind: String,
    data: Value
});
payload!(Started {
    adapter_kind: String, execution_identity: String, provider_session_id: Option<String>,
    provider_configuration_digest: Option<String>, session_cwd: Option<String>,
    session_loaded: Option<bool>
});
payload!(TextDelta {
    message_id: String,
    part_id: String,
    chunk_index: u64,
    text: String
});
payload!(ToolActivity {
    call_id: String,
    tool_name: String,
    phase: String,
    summary: String
});
payload!(TurnEnded { turn_number: u64, finish: String, quiescent: bool, error_text: Option<String> });
payload!(Settled { quiescent: bool, execution_identity: String, completed_operation_ids: Vec<String> });
payload!(CoordinationFinish {
    turn_id: String, outcome: String, command_ids: Vec<String>, operation_ids: Vec<String>,
    message_id: Option<String>, waiting_subject: Option<EntityRef>, explanation: String
});
payload!(ConversationContext {
    console_session_id: String, context_version: u64, selected_work_id: Option<String>,
    project_id: Option<String>, scope: Option<String>
});
payload!(ConsoleOpen {
    console_session_id: String,
    project_id: Option<String>,
    conversation_id: String,
    scope: Option<String>
});
payload!(ConsoleConfigure {
    capability_id: String,
    worker_capability_id: String,
    check_capability_id: String,
    approved_model_destination: String,
    limits: Limits
});
payload!(ProposeHumanAction {
    conversation_id: String, message_id: String, method: String, params: Value,
    if_match: Vec<EntityRef>, summary: String
});
payload!(ResolveGlobalInput {
    request_id: String,
    message_id: String,
    value: Value
});
payload!(ConversationSubmit {
    conversation_id: String, client_message_id: String, text: String,
    attachments: Vec<ArtifactRef>, declared_intent: Option<String>, context: ConversationContext
});
payload!(ResolvedIntent {
    kind: String, work_id: Option<String>, draft_goal: Option<String>,
    referenced_message_ids: Vec<String>, change_proposal_id: Option<String>
});
payload!(ResolveIntents { turn_id: String, message_id: String, intents: Vec<ResolvedIntent> });
payload!(IntakeInput {
    turn_id: String,
    conversation_id: String,
    message_id: String,
    question: String,
    response_schema: Value
});
payload!(AnswerIntake { request_id: String, action: String, value: Option<Value> });
payload!(DecisionOption {
    id: String,
    label: String,
    impact: String
});
payload!(DecisionRequestBody {
    work_id: String, purpose: String, subject: EntityRef, application: Value, question: String,
    options: Vec<DecisionOption>, response_schema: Value, context_request_id: Option<String>
});
payload!(DecisionAnswer {
    decision_id: String,
    value: Value
});
payload!(DeliveryPrepare {
    work_id: String,
    integration_result_id: String
});
payload!(DeliveryAccept {
    candidate_id: String
});
payload!(DeliveryFinding {
    criterion_id: String,
    requested_change: String
});
payload!(DeliveryChanges {
    candidate_id: String, findings: Vec<DeliveryFinding>, preserve_artifacts: Vec<ArtifactRef>, advance: bool
});
payload!(WorkControl {
    work_id: String,
    action: String
});
payload!(WorkContinue {
    work_id: String,
    restart_session: Option<bool>
});
payload!(ClaimExecutor { work_id: String, restart_session: Option<bool> });
payload!(ExecutorInputRequest {
    work_id: String,
    question: String,
    response_schema: Value
});
payload!(WorkspaceTakeover {
    workspace_id: String
});
payload!(WorkspaceHandback {
    workspace_id: String,
    summary: String,
    resume_affected: bool
});
payload!(ResultVerdict {
    result_id: String,
    evaluation_round: u64
});
payload!(ResultChanges {
    result_id: String,
    evaluation_round: u64,
    instruction: ReworkInstruction
});

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    payload!(OptionalProbe { required: Value, optional: Option<Value>, trailing: Vec<String>, });

    #[test]
    fn required_json_null_is_preserved() {
        let answer = DecisionAnswer {
            decision_id: "36f6003a-f7eb-479e-a86a-e58ef29beaa5".into(),
            value: Value::Null,
        };
        let value = serde_json::to_value(&answer).unwrap();
        assert_eq!(value, json!({"decisionId":answer.decision_id,"value":null}));
        let decoded: DecisionAnswer = serde_json::from_value(value).unwrap();
        assert_eq!(decoded.decision_id, answer.decision_id);
        assert!(decoded.value.is_null());
    }

    #[test]
    fn only_absent_option_fields_are_omitted() {
        let mut probe = OptionalProbe {
            required: Value::Null,
            optional: None,
            trailing: vec![],
        };
        assert_eq!(
            serde_json::to_value(&probe).unwrap(),
            json!({"required":null,"trailing":[]})
        );
        probe.optional = Some(json!({"answer":42}));
        assert_eq!(
            serde_json::to_value(&probe).unwrap(),
            json!({"required":null,"optional":{"answer":42},"trailing":[]})
        );
        probe.optional = Some(Value::Null);
        assert!(serde_json::to_value(&probe)
            .unwrap()
            .get("optional")
            .is_some());
    }

    static SERIALIZATIONS: AtomicUsize = AtomicUsize::new(0);

    #[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
    pub struct Counted(u64);

    impl Serialize for Counted {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            SERIALIZATIONS.fetch_add(1, Ordering::Relaxed);
            serializer.serialize_u64(self.0)
        }
    }

    payload!(CountingProbe { required: Counted, optional: Option<Counted> });

    #[test]
    fn payload_values_are_serialized_exactly_once() {
        SERIALIZATIONS.store(0, Ordering::Relaxed);
        let probe = CountingProbe {
            required: Counted(1),
            optional: Some(Counted(2)),
        };
        assert_eq!(
            serde_json::to_value(&probe).unwrap(),
            json!({"required":1,"optional":2})
        );
        assert_eq!(SERIALIZATIONS.load(Ordering::Relaxed), 2);
    }
}
