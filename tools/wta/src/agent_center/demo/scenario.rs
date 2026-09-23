// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

//! Replayable English demo state. Optional local check receipts are supplied by the UI adapter.

use anyhow::{bail, ensure, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub(in crate::agent_center) mod release_journey;
pub(in crate::agent_center) mod work_board;
pub(in crate::agent_center) mod work_graph;
mod work_model;
pub(in crate::agent_center) use work_model::Definition;

const FIX: &str = "fix-4821";
const MIGRATION: &str = "api-v2";
const DOCS: &str = "documentation";
const SOURCE: &str = "scripted-fixture";
const ACCEPTANCE: &str = "Pending; scripted demo, not formal WorkExecutor acceptance";
pub(in crate::agent_center) const ATTENTION_DELAY_MS: u64 = 10_000;
pub(in crate::agent_center) const BUDGET_STEP_MS: u64 = 3_000;
const BUDGET_STEP_TOKENS: u64 = 5_000;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::agent_center) struct DemoClock {
    pub elapsed_ms: u64,
    pub attention_due_ms: Option<u64>,
    pub attention_raised: bool,
    pub budget_due_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::agent_center) struct Scenario {
    pub format_version: u32,
    pub revision: u64,
    pub scene: u8,
    pub works: Vec<Work>,
    pub selected_work_id: String,
    pub events: Vec<Event>,
    pub exchange: Option<Exchange>,
    pub decision: Option<Decision>,
    pub budget: Budget,
    pub resumed: bool,
    pub branch_confirmation_pending: bool,
    pub evidence_visible: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clock: Option<DemoClock>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cap_configured: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_graph: Option<work_graph::Graph>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_board: Option<work_board::Board>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_journey: Option<release_journey::Journey>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::agent_center) struct Work {
    pub id: String,
    pub title: String,
    pub goal: String,
    pub status: String,
    pub repository: String,
    pub branch: String,
    pub steps: Vec<Step>,
    pub evidence: Vec<Evidence>,
    pub findings: Vec<String>,
    pub parent_work_id: Option<String>,
    pub context: Context,
    pub executor_session_id: String,
    pub progress: String,
    pub blockers: Vec<String>,
    pub decisions: Vec<String>,
    pub deliverables: Vec<String>,
    pub acceptance: String,
    pub next_step: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition: Option<Definition>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::agent_center) struct Context {
    pub repository: String,
    pub issue: String,
    pub background: Vec<String>,
    pub findings: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(in crate::agent_center) enum StepStatus {
    Completed,
    Blocked,
    Pending,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::agent_center) struct Step {
    pub id: String,
    pub title: String,
    pub status: StepStatus,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::agent_center) struct Evidence {
    pub id: String,
    pub step_id: String,
    pub command: String,
    pub exit_code: i32,
    pub source: String,
    pub summary: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::agent_center) struct Event {
    pub sequence: u64,
    pub revision: u64,
    pub kind: EventKind,
    pub work_id: String,
    pub input: Option<String>,
    pub source: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executor_receipt: Option<work_graph::Receipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_receipt: Option<release_journey::Receipt>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(in crate::agent_center) enum EventKind {
    UserRequest,
    WorkCreated,
    SchemaRequest,
    SchemaResponse,
    DecisionRecorded,
    BudgetReached,
    BudgetAdded,
    SystemStarted,
    SystemAdvanced,
    AttentionRaised,
    BudgetConsumed,
    CapSetupEnabled,
    TokenCapSet,
    DemoExecutorUpdate,
    ReleaseExecutorUpdate,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::agent_center) struct Exchange {
    pub request: SchemaRequest,
    pub response: SchemaResponse,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::agent_center) struct SchemaRequest {
    pub need: String,
    pub reason: String,
    pub blocking: bool,
    pub expected_result: String,
    pub consumer: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::agent_center) struct SchemaResponse {
    pub version: String,
    pub breaking_changes: u8,
    pub compatibility_notes: Vec<String>,
    pub recommendation: String,
    pub source: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::agent_center) struct Decision {
    pub work_id: String,
    pub choice: String,
    pub resolved: bool,
    pub outcome: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::agent_center) struct Budget {
    pub work_id: String,
    pub limit: u64,
    pub consumed: u64,
    pub priority: String,
    pub max_concurrency: u8,
    pub reached: bool,
    pub paused: bool,
    pub resume_condition: String,
}

type AutomaticEvent = (EventKind, &'static str, &'static str);

impl Work {
    fn fixture(id: &str, title: &str, goal: &str, steps: &[(&str, &str)]) -> Self {
        let mut work = Self {
            id: id.into(),
            title: title.into(),
            goal: goal.into(),
            status: "Pending".into(),
            repository: "microsoft/foo".into(),
            branch: if id == FIX {
                FIX.into()
            } else {
                format!("demo-{id}")
            },
            steps: steps
                .iter()
                .map(|(id, title)| Step {
                    id: (*id).into(),
                    title: (*title).into(),
                    status: StepStatus::Pending,
                })
                .collect(),
            evidence: vec![],
            findings: vec![],
            parent_work_id: (id != FIX).then(|| FIX.into()),
            context: Context {
                repository: "microsoft/foo".into(),
                issue: "#4821".into(),
                background: vec!["Shell integration consumes the affected API.".into()],
                findings: vec!["Compatibility fails for the legacy shell adapter.".into()],
            },
            executor_session_id: format!("simulated-{id}-executor-1"),
            progress: String::new(),
            blockers: vec![],
            decisions: vec![],
            deliverables: vec![],
            acceptance: ACCEPTANCE.into(),
            next_step: String::new(),
            definition: None,
        };
        work.refresh_progress();
        work
    }

    fn record(&mut self, step: &str, command: &str, exit_code: i32, summary: &str) {
        self.evidence.push(Evidence {
            id: format!("sample-{}-{step}", self.id),
            step_id: step.into(),
            command: command.into(),
            exit_code,
            source: SOURCE.into(),
            summary: summary.into(),
        });
        self.refresh_progress();
    }

    fn refresh_progress(&mut self) {
        for step in &mut self.steps {
            step.status = match self.evidence.iter().rev().find(|e| e.step_id == step.id) {
                Some(e) if e.exit_code == 0 => StepStatus::Completed,
                Some(_) => StepStatus::Blocked,
                None => StepStatus::Pending,
            };
        }
        let completed = self
            .steps
            .iter()
            .filter(|s| s.status == StepStatus::Completed)
            .count();
        self.progress = format!(
            "{completed}/{} checks completed from recorded evidence",
            self.steps.len()
        );
    }
}

impl Scenario {
    pub(in crate::agent_center) fn new() -> Self {
        let mut fix = Work::fixture(
            FIX,
            "Fix Issue #4821",
            "Fix issue #4821 without breaking shell integration",
            &[
                ("reproduce", "Reproduce issue"),
                ("implement", "Implement fix"),
                ("unit-tests", "Unit tests"),
                ("compatibility", "Compatibility test"),
                ("documentation", "Documentation"),
                ("pull-request", "Pull Request"),
            ],
        );
        fix.record(
            "reproduce",
            "fixture-test issue-4821 --reproduce",
            0,
            "1/1 reproduction check passed: original shell failure reproduced.",
        );
        fix.record(
            "implement",
            "fixture-test issue-4821 --patch",
            0,
            "1/1 patch regression check passed.",
        );
        fix.record(
            "unit-tests",
            "fixture-test unit",
            0,
            "42/42 unit tests passed.",
        );
        fix.record("compatibility", "fixture-test shell-compatibility", 1, "7/8 compatibility tests passed; legacy shell adapter expected exit status 0, observed 1.");
        fix.status = "Compatibility test failed".into();
        fix.blockers = vec!["Compatibility test failed: legacy shell adapter".into()];
        fix.findings =
            vec!["The fix passes unit tests but breaks legacy shell integration.".into()];
        fix.deliverables = vec!["Recorded sample patch regression and unit-test results".into()];
        fix.next_step = "Investigate shell integration behavior".into();
        Self {
            format_version: 1,
            revision: 0,
            scene: 1,
            works: vec![fix],
            selected_work_id: FIX.into(),
            events: vec![],
            exchange: None,
            decision: None,
            budget: Budget {
                work_id: MIGRATION.into(),
                limit: 20_000,
                consumed: 0,
                priority: "Low".into(),
                max_concurrency: 1,
                reached: false,
                paused: false,
                resume_condition: "More budget available".into(),
            },
            resumed: false,
            branch_confirmation_pending: false,
            evidence_visible: false,
            clock: None,
            cap_configured: None,
            work_graph: None,
            work_board: None,
            release_journey: None,
        }
    }

    #[cfg(test)]
    pub(in crate::agent_center) fn scene(&self) -> u8 {
        self.scene
    }
    #[cfg(test)]
    pub(in crate::agent_center) fn revision(&self) -> u64 {
        self.revision
    }

    pub(in crate::agent_center) fn apply(&mut self, input: &str) -> Result<()> {
        self.validate()?;
        self.apply_recorded(command(input)?)
    }

    // Old v1 snapshots replay unchanged. This explicit, persisted system event opts
    // them into the natural demo clock without rewriting their historical events.
    pub(in crate::agent_center) fn enable_clock(&mut self) -> Result<()> {
        self.validate()?;
        self.enable_clock_recorded()
    }

    pub(in crate::agent_center) fn require_cap_setup(&mut self) -> Result<()> {
        self.validate()?;
        self.require_cap_setup_recorded()
    }

    fn require_cap_setup_recorded(&mut self) -> Result<()> {
        ensure!(
            self.clock.is_some()
                && self.cap_configured.is_none()
                && !self.resumed
                && self.exchange.is_none(),
            "Explicit token cap setup can only be enabled for a fresh natural demo"
        );
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("Demo revision overflow"))?;
        self.cap_configured = Some(false);
        self.event(
            EventKind::CapSetupEnabled,
            MIGRATION.into(),
            None,
            "Migration requires an explicitly confirmed token cap before consuming budget.",
        );
        Ok(())
    }

    pub(in crate::agent_center) fn awaiting_cap(&self) -> bool {
        self.cap_configured == Some(false)
    }

    fn enable_clock_recorded(&mut self) -> Result<()> {
        ensure!(self.clock.is_none(), "Demo clock is already enabled");
        let attention_raised = self.decision.is_some() || self.scene >= 6;
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("Demo revision overflow"))?;
        self.clock = Some(DemoClock {
            elapsed_ms: 0,
            attention_due_ms: (self.exchange.is_some() && !attention_raised)
                .then_some(ATTENTION_DELAY_MS),
            attention_raised,
            budget_due_ms: (self.decision.is_some() && !self.budget.reached)
                .then_some(BUDGET_STEP_MS),
        });
        self.event(
            EventKind::SystemStarted,
            FIX.into(),
            None,
            "Natural demo clock enabled; historical v1 events preserved.",
        );
        Ok(())
    }

    pub(in crate::agent_center) fn attention_pending(&self) -> bool {
        self.clock
            .as_ref()
            .is_some_and(|clock| clock.attention_raised)
            && self.decision.is_none()
    }

    pub(in crate::agent_center) fn advance(&mut self, elapsed: Duration) -> Result<()> {
        self.validate()?;
        let elapsed_ms = u64::try_from(elapsed.as_millis())
            .map_err(|_| anyhow::anyhow!("Demo elapsed time overflow"))?;
        let mut next = self.clone();
        next.advance_recorded(elapsed_ms)?;
        *self = next;
        Ok(())
    }

    fn advance_recorded(&mut self, elapsed_ms: u64) -> Result<()> {
        if self.work_board.is_some() || self.release_journey.is_some() {
            return Ok(());
        }
        if self.work_graph.is_some() {
            return self.advance_work_graph(elapsed_ms);
        }
        if self.is_work_model() {
            return self.advance_work_model(elapsed_ms);
        }
        let clock = self
            .clock
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Demo clock is not enabled"))?;
        if elapsed_ms == 0 || (clock.attention_due_ms.is_none() && clock.budget_due_ms.is_none()) {
            return Ok(());
        }
        let now = clock
            .elapsed_ms
            .checked_add(elapsed_ms)
            .ok_or_else(|| anyhow::anyhow!("Demo elapsed time overflow"))?;
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("Demo revision overflow"))?;
        self.event(
            EventKind::SystemAdvanced,
            MIGRATION.into(),
            None,
            "Deterministic demo clock advanced; no real agent or token usage.",
        );
        if let Some(event) = self.events.last_mut() {
            event.elapsed_ms = Some(elapsed_ms);
        }
        let clock = self
            .clock
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("Missing demo clock"))?;
        clock.elapsed_ms = now;
        if clock.attention_due_ms.is_some_and(|due| due <= now) {
            clock.attention_due_ms = None;
            clock.attention_raised = true;
            self.scene = 6;
            self.event(EventKind::AttentionRaised, FIX.into(), None,
                "Fix Issue #4821 needs a human compatibility decision. B: fix compatibility, then recheck; failed evidence remains unchanged.");
        }
        while self
            .clock
            .as_ref()
            .and_then(|clock| clock.budget_due_ms)
            .is_some_and(|due| due <= now)
        {
            let cap = if self.cap_configured.is_some() {
                self.budget.limit
            } else {
                20_000
            };
            self.budget.consumed = (self.budget.consumed + BUDGET_STEP_TOKENS).min(cap);
            self.scene = 7;
            if self.budget.consumed == cap {
                self.budget.reached = true;
                self.budget.paused = true;
                self.clock
                    .as_mut()
                    .ok_or_else(|| anyhow::anyhow!("Missing demo clock"))?
                    .budget_due_ms = None;
                let migration = self.work_mut(MIGRATION)?;
                migration.status = "Paused".into();
                migration.blockers = vec!["Budget reached; More budget available required".into()];
                let summary = if self.cap_configured.is_some() {
                    format!("Simulated usage reached {cap} / {cap} tokens; Work paused, findings preserved.")
                } else {
                    "Simulated usage reached exactly 20,000 / 20,000 tokens; Work paused, findings preserved; no process killed.".into()
                };
                self.event(EventKind::BudgetReached, MIGRATION.into(), None, &summary);
            } else {
                let clock = self
                    .clock
                    .as_mut()
                    .ok_or_else(|| anyhow::anyhow!("Missing demo clock"))?;
                clock.budget_due_ms = Some(
                    clock
                        .budget_due_ms
                        .ok_or_else(|| anyhow::anyhow!("Missing budget deadline"))?
                        .checked_add(BUDGET_STEP_MS)
                        .ok_or_else(|| anyhow::anyhow!("Demo budget deadline overflow"))?,
                );
                self.event(EventKind::BudgetConsumed, MIGRATION.into(), None,
                    &format!("Migration Investigation: {} / {cap} simulated tokens consumed; findings preserved.", self.budget.consumed));
            }
        }
        Ok(())
    }

    pub(in crate::agent_center) fn replay_event(&mut self, event: &Event) -> Result<()> {
        match event.kind {
            EventKind::UserRequest => self.apply_recorded(command(
                event
                    .input
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("User event has no input"))?,
            )?),
            EventKind::SystemStarted => self.enable_clock_recorded(),
            EventKind::CapSetupEnabled => self.require_cap_setup_recorded(),
            EventKind::SystemAdvanced => self.advance_recorded(
                event
                    .elapsed_ms
                    .ok_or_else(|| anyhow::anyhow!("System clock event has no elapsed time"))?,
            ),
            EventKind::DemoExecutorUpdate => self.receive_graph_receipt_recorded(
                event
                    .executor_receipt
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("Missing executor receipt"))?,
            ),
            EventKind::ReleaseExecutorUpdate => self.receive_release_recorded(
                event
                    .release_receipt
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("Missing release check receipt"))?,
            ),
            _ => Ok(()),
        }
    }

    fn apply_recorded(&mut self, input: &str) -> Result<()> {
        // Reduce a copy so every rejection is atomic, including prerequisite failures.
        let mut next = self.clone();
        let automatic = next.reduce(input)?;
        let conversation_turn = next.work_graph.is_some()
            || next.release_journey.is_some() && input == release_journey::PROGRESS
            || next.is_work_model()
                && matches!(
                    input,
                    "Review progress"
                        | "Why are you paused?"
                        | "Keep the legacy API unchanged."
                        | "Is yesterday's fix ready to merge?"
                        | "Why did compatibility fail?"
                );
        if next == *self && !conversation_turn {
            return Ok(());
        }
        next.revision = self
            .revision
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("Demo revision overflow"))?;
        next.event(
            EventKind::UserRequest,
            next.selected_work_id.clone(),
            Some(input.into()),
            input,
        );
        for (kind, work, summary) in automatic {
            next.event(kind, work.into(), None, summary);
        }
        *self = next;
        Ok(())
    }

    fn event(&mut self, kind: EventKind, work_id: String, input: Option<String>, summary: &str) {
        self.events.push(Event {
            sequence: self.events.len() as u64 + 1,
            revision: self.revision,
            kind,
            work_id,
            input,
            source: SOURCE.into(),
            summary: summary.into(),
            elapsed_ms: None,
            executor_receipt: None,
            release_receipt: None,
        });
    }

    fn work_mut(&mut self, id: &str) -> Result<&mut Work> {
        self.works
            .iter_mut()
            .find(|w| w.id == id)
            .ok_or_else(|| anyhow::anyhow!("Work {id} is not available yet"))
    }

    fn reduce(&mut self, input: &str) -> Result<Vec<AutomaticEvent>> {
        if self.release_journey.is_some() {
            return self.reduce_release(input);
        }
        if matches!(
            input,
            release_journey::OPEN | release_journey::OPEN_PARALLEL
        ) {
            return self.enable_release(input == release_journey::OPEN_PARALLEL);
        }
        if self.work_board.is_some() {
            return self.reduce_work_board(input);
        }
        if input == "Open Work board" {
            return self.enable_work_board();
        }
        if input == "Open Work graph" {
            return self.enable_work_graph();
        }
        if self.work_graph.is_some() {
            return self.reduce_work_graph(input);
        }
        if input == "Open merge story" {
            return self.enable_merge_story();
        }
        if input == "Open Work model" {
            return self.enable_work_model();
        }
        if self.is_work_model() {
            return self.reduce_work_model(input);
        }
        let mut automatic = vec![];
        if input != "Continue fixing issue #4821" {
            ensure!(self.resumed, "First: Continue fixing issue #4821");
        }
        match input {
            "Continue fixing issue #4821" => {
                self.resumed = true;
                self.scene = 2;
                self.selected_work_id = FIX.into();
            }
            "Show work state" | "Show evidence" => {
                self.scene = 3;
                self.selected_work_id = FIX.into();
                self.evidence_visible = input == "Show evidence";
            }
            "Should we migrate to API v2?" => {
                self.scene = 4;
                self.selected_work_id = FIX.into();
                self.branch_confirmation_pending = self.exchange.is_none();
            }
            "Cancel related work" => {
                ensure!(
                    self.branch_confirmation_pending,
                    "No related-work confirmation is pending"
                );
                self.branch_confirmation_pending = false;
                self.scene = 3;
            }
            "Create related work" => {
                if self.exchange.is_some() {
                    return Ok(automatic);
                }
                ensure!(
                    self.branch_confirmation_pending,
                    "First ask: Should we migrate to API v2?"
                );
                let mut migration = Work::fixture(
                    MIGRATION,
                    "Investigate API v2",
                    "Evaluate API v2 migration independently of the bug fix",
                    &[
                        ("schema", "Versioned schema review"),
                        ("performance", "Performance Validation"),
                    ],
                );
                migration.status = "Investigating".into();
                migration.record("schema", "fixture-test schema-contract --version 2.3", 0, "Versioned schema 2.3 recorded; 3 breaking changes and compatibility notes included.");
                migration.findings = vec!["Schema 2.3 saved; 3 breaking changes; compatibility notes included; Proceed with caution.".into()];
                migration.deliverables =
                    vec!["Versioned schema 2.3 and compatibility notes (sample)".into()];
                migration.next_step = "Performance Validation".into();
                self.works.push(migration);
                self.exchange = Some(Exchange {
                    request: SchemaRequest {
                        need: "API v2 schema".into(),
                        reason: "Migration evaluation".into(),
                        blocking: false,
                        expected_result: "Versioned schema".into(),
                        consumer: MIGRATION.into(),
                    },
                    response: SchemaResponse {
                        version: "2.3".into(),
                        breaking_changes: 3,
                        compatibility_notes: vec![
                            "Legacy shell adapters need an explicit v2 compatibility layer.".into(),
                        ],
                        recommendation: "Proceed with caution".into(),
                        source: SOURCE.into(),
                    },
                });
                self.branch_confirmation_pending = false;
                self.scene = 4;
                self.selected_work_id = MIGRATION.into();
                if let Some(clock) = &mut self.clock {
                    clock.attention_due_ms = Some(
                        clock
                            .elapsed_ms
                            .checked_add(ATTENTION_DELAY_MS)
                            .ok_or_else(|| anyhow::anyhow!("Demo attention deadline overflow"))?,
                    );
                    self.scene = 5;
                }
                automatic.extend([
                    (EventKind::WorkCreated, MIGRATION, "Independent related Work created; selective background only, no transcript copied."),
                    (EventKind::SchemaRequest, MIGRATION, "Automatic nonblocking request for API v2 schema; no human handoff."),
                    (EventKind::SchemaResponse, MIGRATION, "Automatic structured result: schema 2.3, 3 breaking changes, compatibility notes included."),
                ]);
            }
            "Show schema exchange" => {
                ensure!(self.exchange.is_some(), "Confirm the related Work first");
                self.scene = 5;
                self.selected_work_id = MIGRATION.into();
            }
            "Show attention" => {
                ensure!(self.exchange.is_some(), "Confirm the related Work first");
                if self.clock.is_some() {
                    ensure!(
                        self.attention_pending() || self.decision.is_some(),
                        "No attention decision is pending yet"
                    );
                }
                self.scene = 6;
                self.selected_work_id = FIX.into();
            }
            "A" | "B" | "C" => {
                if let Some(decision) = &self.decision {
                    ensure!(
                        decision.choice == input,
                        "The decision is already resolved; a different choice is unsupported"
                    );
                    return Ok(automatic);
                }
                ensure!(
                    if self.clock.is_some() {
                        self.attention_pending()
                    } else {
                        self.scene == 6
                    },
                    "No compatibility decision is pending"
                );
                self.selected_work_id = FIX.into();
                let (outcome, status) = match input {
                    "A" => (
                        "Compatibility failure acknowledged; acceptance remains pending",
                        "Compatibility failure acknowledged",
                    ),
                    "B" => (
                        "Queue fix and recheck compatibility; no new verification result",
                        "Compatibility recheck queued",
                    ),
                    _ => (
                        "Queue rollback review; no repository changes performed",
                        "Rollback review queued",
                    ),
                };
                let fix = self.work_mut(FIX)?;
                fix.status = status.into();
                fix.next_step = outcome.into();
                fix.decisions.push(format!("{input}: {outcome}"));
                self.decision = Some(Decision {
                    work_id: FIX.into(),
                    choice: input.into(),
                    resolved: true,
                    outcome: outcome.into(),
                });
                let mut docs = Work::fixture(
                    DOCS,
                    "Prepare Documentation",
                    "Document issue #4821 and outstanding compatibility limitations",
                    &[
                        ("draft", "Draft documentation"),
                        ("review", "Review documentation"),
                    ],
                );
                docs.next_step =
                    "Draft documentation; compatibility acceptance is still pending".into();
                self.works.push(docs);
                let start_budget = !self.awaiting_cap();
                if !start_budget {
                    self.scene = 7;
                }
                if let Some(clock) = self.clock.as_mut().filter(|_| start_budget) {
                    clock.budget_due_ms = Some(
                        clock
                            .elapsed_ms
                            .checked_add(BUDGET_STEP_MS)
                            .ok_or_else(|| anyhow::anyhow!("Demo budget deadline overflow"))?,
                    );
                }
                automatic.extend([
                    (EventKind::DecisionRecorded, FIX, "Decision recorded; failed compatibility evidence remains unchanged."),
                    (EventKind::WorkCreated, DOCS, "Independent documentation follow-up created; no execution or acceptance claimed."),
                ]);
            }
            "Show budget" => {
                ensure!(
                    self.decision.is_some(),
                    "Resolve the compatibility decision first"
                );
                if self.clock.is_none() && !self.budget.reached {
                    self.budget.consumed = 20_000;
                    self.budget.reached = true;
                    self.budget.paused = true;
                    let migration = self.work_mut(MIGRATION)?;
                    migration.status = "Paused".into();
                    migration.blockers =
                        vec!["Budget reached; More budget available required".into()];
                    automatic.push((EventKind::BudgetReached, MIGRATION, "Simulated usage reached exactly 20,000 / 20,000 tokens; Work paused, findings preserved; no process killed."));
                }
                self.scene = 7;
                self.selected_work_id = MIGRATION.into();
            }
            "Add 10K budget" => {
                ensure!(
                    self.budget.reached,
                    "Wait for the migration budget to be reached"
                );
                if (self.cap_configured.is_none() && self.budget.limit == 30_000)
                    || (self.cap_configured.is_some() && !self.budget.paused)
                {
                    return Ok(automatic);
                }
                self.budget.limit = if self.cap_configured.is_none() {
                    30_000
                } else {
                    self.budget
                        .limit
                        .checked_add(10_000)
                        .ok_or_else(|| anyhow::anyhow!("Demo token cap overflow"))?
                };
                self.budget.paused = false;
                let migration = self.work_mut(MIGRATION)?;
                migration.status = "Investigating".into();
                migration.blockers.clear();
                self.scene = 7;
                self.selected_work_id = MIGRATION.into();
                automatic.push((EventKind::BudgetAdded, MIGRATION,
                    if self.cap_configured.is_none() {
                        "Simulated budget increased to 30,000; same Work and executor resumed with saved findings."
                    } else { "Token cap increased by 10,000; same Work and executor resumed with saved findings." }));
            }
            "Set token cap to 10K" | "Set token cap to 20K" | "Set token cap to 30K" => {
                ensure!(
                    self.exchange.is_some() && self.clock.is_some(),
                    "Create the migration Work before setting its token cap"
                );
                let cap = match input {
                    "Set token cap to 10K" => 10_000,
                    "Set token cap to 20K" => 20_000,
                    _ => 30_000,
                };
                ensure!(
                    cap >= self.budget.consumed,
                    "Token cap cannot be lower than recorded usage; usage is never discarded"
                );
                if self.cap_configured == Some(true) && self.budget.limit == cap {
                    return Ok(automatic);
                }
                self.cap_configured = Some(true);
                self.budget.limit = cap;
                self.budget.reached = self.budget.consumed == cap;
                self.budget.paused = self.budget.reached;
                self.selected_work_id = MIGRATION.into();
                if self.decision.is_some() {
                    self.scene = 7;
                }
                let paused = self.budget.paused;
                let migration = self.work_mut(MIGRATION)?;
                migration.status = if paused { "Paused" } else { "Investigating" }.into();
                migration.blockers = if paused {
                    vec!["Budget reached; More budget available required".into()]
                } else {
                    vec![]
                };
                let clock = self
                    .clock
                    .as_mut()
                    .ok_or_else(|| anyhow::anyhow!("Missing demo clock"))?;
                if paused || self.decision.is_none() {
                    clock.budget_due_ms = None;
                } else if clock.budget_due_ms.is_none() {
                    clock.budget_due_ms = Some(
                        clock
                            .elapsed_ms
                            .checked_add(BUDGET_STEP_MS)
                            .ok_or_else(|| anyhow::anyhow!("Demo budget deadline overflow"))?,
                    );
                }
                automatic.push((EventKind::TokenCapSet, MIGRATION,
                    "Human confirmed the migration token cap; recorded usage and findings preserved."));
            }
            "Show work overview" => {
                ensure!(
                    self.budget.reached && self.works.iter().any(|w| w.id == DOCS),
                    "Resolve the decision and show the budget before the overview"
                );
                self.scene = 8;
                self.selected_work_id = FIX.into();
            }
            _ => bail!("Unsupported scripted demo input"),
        }
        Ok(automatic)
    }

    pub(in crate::agent_center) fn validate(&self) -> Result<()> {
        ensure!(self.format_version == 1, "Unsupported demo formatVersion");
        ensure!((1..=8).contains(&self.scene), "Invalid demo scene");
        // Canonical replay verifies all denormalized fields, evidence provenance,
        // event ordering and IDs without trusting the serialized step statuses.
        let mut expected = Self::new();
        for event in &self.events {
            expected.replay_event(event)?;
        }
        ensure!(
            *self == expected,
            "Corrupt demo snapshot: state or evidence differs from deterministic event replay"
        );
        Ok(())
    }

    pub(in crate::agent_center) fn next_input(&self) -> &'static str {
        if let Some(journey) = &self.release_journey {
            return journey.next_input();
        }
        if self.work_board.is_some() {
            return "Show work overview";
        }
        if self.work_graph.is_some() {
            return self.graph_next_input();
        }
        if self.is_work_model() {
            return if !self.resumed {
                if self.is_merge_story() {
                    "Is yesterday's fix ready to merge?"
                } else {
                    "Continue fixing issue #4821"
                }
            } else if self.awaiting_cap() {
                if self.is_merge_story() {
                    "Proceed. Keep the legacy API unchanged."
                } else {
                    "Set token cap to 20K"
                }
            } else if self.budget.paused {
                "Add 10K budget"
            } else {
                "Investigate compatibility"
            };
        }
        if self.clock.is_some() {
            return if !self.resumed {
                "Continue fixing issue #4821"
            } else if self.branch_confirmation_pending {
                "Create related work"
            } else if self.exchange.is_none() {
                "Should we migrate to API v2?"
            } else if self.attention_pending() {
                "B"
            } else if self.budget.paused {
                "Add 10K budget"
            } else {
                "Show evidence"
            };
        }
        match self.scene {
            1 => "Continue fixing issue #4821",
            2 => "Show work state",
            3 => "Should we migrate to API v2?",
            4 if self.branch_confirmation_pending => "Create related work",
            4 => "Show schema exchange",
            5 => "Show attention",
            6 if self.decision.is_none() => "B",
            6 => "Show budget",
            7 => "Show work overview",
            _ => "Show evidence",
        }
    }

    #[cfg(test)]
    fn content(&self) -> String {
        let mut text = format!("SIMULATED STORY FIXTURE — Scene {}\n", self.scene);
        match self.scene {
            1 => text.push_str("Three days ago I was working with multiple agents.\nTab 1: Fix bug\nTab 2: Code review\nTab 3: Migration\nTab 4: Research\nWhich tab owns the bug? Which session has context? What is done?\nToday: Human -> Agent -> Work. Work is buried inside agent sessions.\n"),
            2 => {
                text.push_str("Found existing Work: Fix Issue #4821\n");
                if let Some(work) = self.works.iter().find(|work| work.id == FIX) {
                    text.push_str(&format!("Repository: {}\nBranch: {}\nStatus: {}\nCompleted:\n", work.repository, work.branch, work.status));
                    for step in work.steps.iter().filter(|step| step.status == StepStatus::Completed) {
                        text.push_str(&format!("  {} (verified)\n", step.title));
                    }
                    text.push_str(&format!("Next Step: {}\n", work.next_step));
                }
                text.push_str("Don't resume the agent. Resume the work.\n");
            }
            3 => {
                text.push_str("Fix Issue #4821\nWork state comes from recorded evidence, not an agent summary.\nAgent finished != Work finished. Verify outcomes, not agent claims.\nEvidence source: scripted-fixture\n");
                if let Some(work) = self.works.iter().find(|work| work.id == FIX) {
                    for (label, status) in [("Completed", StepStatus::Completed), ("Blocked", StepStatus::Blocked), ("Pending", StepStatus::Pending)] {
                        text.push_str(&format!("{label}:\n"));
                        for step in work.steps.iter().filter(|step| step.status == status) {
                            let summary = work.evidence.iter().rev().find(|e| e.step_id == step.id)
                                .map(|e| format!("{}; exitCode={}", e.id, e.exit_code))
                                .unwrap_or_else(|| "No recorded verification".into());
                            text.push_str(&format!("  {} — {summary}\n", step.title));
                        }
                    }
                    if self.evidence_visible {
                        text.push_str("\nRecorded sample evidence (not a live test run):\n");
                        for evidence in &work.evidence {
                            text.push_str(&format!("Evidence {}: command='{}'; exitCode={}; source={}\n  {}\n",
                                evidence.id, evidence.command, evidence.exit_code, evidence.source, evidence.summary));
                        }
                    } else {
                        text.push_str("Show evidence: inspect the recorded sample commands and results.\n");
                    }
                }
            }
            4 if self.branch_confirmation_pending => text.push_str("Create Related Work?\nProposed: Investigate API v2 Migration\nNothing created yet. Confirm with 'Create related work' or decline with 'Cancel related work'.\nOriginal goal remains fixing issue #4821.\n"),
            4 => {
                text.push_str("Related Work created: Investigate API v2\nFix Issue #4821 -> Investigate API v2\n");
                if let Some(original) = self.works.iter().find(|work| work.id == FIX) {
                    text.push_str(&format!("Original goal unchanged: {}\n", original.goal));
                }
                if let Some(related) = self.works.iter().find(|work| work.id == MIGRATION) {
                    text.push_str(&format!("Repository: {}\nRelevant background: {}\nShared findings: {}\nIndependent goal: {}\nIndependent lifecycle: {}\n",
                        related.context.repository, list(&related.context.background), list(&related.context.findings), related.goal, related.status));
                }
                text.push_str("Fork the work, not the conversation. No transcript copied.\n");
            }
            5 => {
                if let Some(exchange) = &self.exchange {
                    text.push_str(&format!("Automatic structured Work exchange; no human handoff; no transcript copying.\nNeed: {}\nReason: {}\nBlocking: No\nExpected Result: {}\nConsumer: {}\nSchema Version: {}\nBreaking Changes: {}\nCompatibility Notes: Included — {}\nRecommendation: {}\nShare results, not transcripts.\n",
                        exchange.request.need, exchange.request.reason, exchange.request.expected_result, exchange.request.consumer,
                        exchange.response.version, exchange.response.breaking_changes, exchange.response.compatibility_notes.join("; "), exchange.response.recommendation));
                }
            }
            6 => {
                text.push_str("Work Needs Attention: Compatibility test failed\nDecision Required: legacy shell behavior prevents verified completion.\nA. Ignore\nB. Fix compatibility (recommended)\nC. Roll back\nWhy B: preserve the fix while restoring legacy shell behavior; then recheck.\nI manage decisions, not agents.\n");
                if let Some(decision) = &self.decision {
                    text.push_str(&format!("Resolved decision {}: {}\nSelection is not proof of success.\n", decision.choice, decision.outcome));
                }
            }
            7 => {
                text.push_str(&format!("Migration Investigation — Budget and Scheduling\nPriority: {}\nBudget: {} / {} tokens\nMax Concurrency: {}\nStatus: {}\nFindings: Saved\n",
                    self.budget.priority, self.budget.consumed, self.budget.limit, self.budget.max_concurrency,
                    if self.budget.paused { "Paused — Budget Reached" } else { "Resumed" }));
                if let Some(work) = self.works.iter().find(|work| work.id == MIGRATION) {
                    text.push_str(&format!("  {}\nRemaining Work: {}\n", list(&work.findings), work.next_step));
                }
                text.push_str(&format!("Resume Condition: {}\nOptional: Add 10K budget\nNo real process is killed. Allocate resources to outcomes, not sessions.\n", self.budget.resume_condition));
            }
            8 => {
                text.push_str("Work Overview\n");
                for work in &self.works {
                    text.push_str(&format!("\n{}\nStatus: {}\nProgress: {}\nBlockers: {}\nDecisions: {}\nDeliverables: {}\nAcceptance: Pending\n",
                        work.title, work.status, work.progress, list(&work.blockers), list(&work.decisions), list(&work.deliverables)));
                }
                text.push_str("Don't manage agents. Manage work.\n");
            }
            _ => {}
        }
        text
    }
}

#[cfg(test)]
fn list(values: &[String]) -> String {
    if values.is_empty() {
        "None recorded".into()
    } else {
        values.join("; ")
    }
}

pub(in crate::agent_center) fn command(input: &str) -> Result<&str> {
    if let Some(command) = release_journey::INPUTS
        .iter()
        .find(|command| command.eq_ignore_ascii_case(input.trim()))
    {
        return Ok(command);
    }
    let input = input.trim();
    if input.starts_with(work_board::CREATE) || input.starts_with(work_board::START) {
        return Ok(input);
    }
    let command = match input.trim().to_ascii_lowercase().as_str() {
        "open work board" => "Open Work board",
        "open work graph" => "Open Work graph",
        "plan the fix. require implementation and compatibility. high priority, 30k each." => {
            work_graph::PLAN
        }
        "start the required work." => work_graph::START,
        "continue yesterday's fix." => work_graph::RESUME,
        "where are we on the fix?" => work_graph::PROGRESS,
        "investigate the startup warning. related work, low priority, 5k cap." => {
            work_graph::RELATED
        }
        "summarize progress, decisions and spend." => work_graph::AUDIT,
        "what is blocking compatibility?" => work_graph::EXPLAIN,
        "has compatibility been verified? show evidence and next steps." => work_graph::DECISION,
        "show the compatibility evidence." => work_graph::EVIDENCE,
        "record the decision: preserve legacy double quotes. send it to compatibility and run the check." => work_graph::HANDOFF,
        "show decision handoff." => work_graph::HANDOFF_STATUS,
        "open work model" => "Open Work model",
        "open merge story" => "Open merge story",
        "is yesterday's fix ready to merge?" => "Is yesterday's fix ready to merge?",
        "proceed. keep the legacy api unchanged." => "Proceed. Keep the legacy API unchanged.",
        "why did compatibility fail?" => "Why did compatibility fail?",
        "investigate compatibility" => "Investigate compatibility",
        "keep the legacy api unchanged" | "keep the legacy api unchanged." => {
            "Keep the legacy API unchanged."
        }
        "why are you paused?" | "why are you paused" => "Why are you paused?",
        "review progress" => "Review progress",
        "continue fixing issue #4821" | "/resume" => "Continue fixing issue #4821",
        "show work state" | "/state" => "Show work state",
        "show evidence" | "/evidence" => "Show evidence",
        "should we migrate to api v2?" | "/branch" => "Should we migrate to API v2?",
        "create related work" => "Create related work",
        "cancel related work" => "Cancel related work",
        "show schema exchange" | "/exchange" => "Show schema exchange",
        "show attention" | "/attention" => "Show attention",
        "a" | "ignore" => "A",
        "b" | "fix compatibility" => "B",
        "c" | "roll back" => "C",
        "show budget" | "/budget" => "Show budget",
        "add 10k budget" => "Add 10K budget",
        "set token cap to 10k" => "Set token cap to 10K",
        "set token cap to 20k" => "Set token cap to 20K",
        "set token cap to 30k" => "Set token cap to 30K",
        "show work overview" | "/overview" => "Show work overview",
        _ => bail!("Unsupported scripted demo input; use the recommended exact phrase"),
    };
    Ok(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capped_branch() -> Scenario {
        let mut scenario = Scenario::new();
        scenario.enable_clock().unwrap();
        scenario.require_cap_setup().unwrap();
        for input in [
            "Continue fixing issue #4821",
            "Should we migrate to API v2?",
            "Create related work",
        ] {
            scenario.apply(input).unwrap();
        }
        scenario
            .advance(Duration::from_millis(ATTENTION_DELAY_MS))
            .unwrap();
        scenario.apply("B").unwrap();
        scenario
    }

    #[test]
    fn token_cap_requires_confirmation_and_pauses_exactly_at_each_preset() {
        for (input, cap) in [
            ("Set token cap to 10K", 10_000),
            ("Set token cap to 20K", 20_000),
            ("Set token cap to 30K", 30_000),
        ] {
            let mut scenario = capped_branch();
            assert!(scenario.awaiting_cap());
            assert_eq!(scenario.budget.limit, 20_000);
            let waiting = scenario.clone();
            scenario.advance(Duration::from_secs(60)).unwrap();
            assert_eq!(scenario, waiting);
            scenario.apply(input).unwrap();
            assert_eq!(scenario.cap_configured, Some(true));
            assert_eq!(scenario.selected_work_id, MIGRATION);
            assert_eq!(scenario.budget.limit, cap);
            let unchanged = scenario.clone();
            scenario.apply(input).unwrap();
            assert_eq!(scenario, unchanged);
            scenario
                .advance(Duration::from_millis(
                    BUDGET_STEP_MS * (cap / BUDGET_STEP_TOKENS),
                ))
                .unwrap();
            assert_eq!(scenario.budget.consumed, cap);
            assert!(scenario.budget.paused);
            let paused = scenario.clone();
            scenario.advance(Duration::from_secs(60)).unwrap();
            assert_eq!(scenario, paused);
            scenario.apply("Add 10K budget").unwrap();
            assert_eq!(scenario.budget.limit, cap + 10_000);
            assert_eq!(scenario.budget.consumed, cap);
            assert!(!scenario.budget.paused);
            assert_eq!(scenario.works[1].id, paused.works[1].id);
            assert_eq!(
                scenario.works[1].executor_session_id,
                paused.works[1].executor_session_id
            );
            assert_eq!(scenario.works[1].findings, paused.works[1].findings);
            let resumed = scenario.clone();
            scenario.advance(Duration::from_secs(60)).unwrap();
            scenario.apply("Add 10K budget").unwrap();
            assert_eq!(scenario, resumed);
            let restored: Scenario =
                serde_json::from_str(&serde_json::to_string(&scenario).unwrap()).unwrap();
            restored.validate().unwrap();
            assert_eq!(restored, scenario);
        }
    }

    #[test]
    fn token_cap_reconfiguration_preserves_usage_and_rejects_invalid_targets() {
        let mut scenario = Scenario::new();
        scenario.enable_clock().unwrap();
        scenario.require_cap_setup().unwrap();
        scenario.apply("Continue fixing issue #4821").unwrap();
        let before = scenario.clone();
        assert!(scenario.apply("Set token cap to 10K").is_err());
        assert!(scenario.apply("Set token cap to 15K").is_err());
        assert_eq!(scenario, before);
        let mut scenario = capped_branch();
        scenario.apply("Set token cap to 30K").unwrap();
        scenario
            .advance(Duration::from_millis(BUDGET_STEP_MS * 3))
            .unwrap();
        assert_eq!(scenario.budget.consumed, 15_000);
        let before = scenario.clone();
        assert!(scenario.apply("Set token cap to 10K").is_err());
        assert_eq!(scenario, before);
        scenario.apply("Set token cap to 20K").unwrap();
        assert_eq!(scenario.budget.consumed, 15_000);
        scenario
            .advance(Duration::from_millis(BUDGET_STEP_MS))
            .unwrap();
        assert_eq!(scenario.budget.consumed, 20_000);
        assert!(scenario.budget.paused);
        scenario.apply("Set token cap to 30K").unwrap();
        assert!(!scenario.budget.paused);
        assert_eq!(scenario.budget.consumed, 20_000);
        scenario.apply("Set token cap to 20K").unwrap();
        assert!(scenario.budget.paused);
        assert_eq!(scenario.budget.consumed, 20_000);
        scenario.validate().unwrap();
    }

    #[test]
    fn token_cap_setup_preserves_previous_legacy_and_natural_snapshot_semantics() {
        let mut legacy = through_decision("B");
        legacy.apply("Show budget").unwrap();
        let mut natural = natural_branch();
        natural
            .advance(Duration::from_millis(ATTENTION_DELAY_MS))
            .unwrap();
        natural.apply("B").unwrap();
        natural
            .advance(Duration::from_millis(BUDGET_STEP_MS))
            .unwrap();
        for saved in [legacy, natural] {
            let json = serde_json::to_string(&saved).unwrap();
            assert!(!json.contains("capConfigured"));
            let mut reopened: Scenario = serde_json::from_str(&json).unwrap();
            reopened.validate().unwrap();
            assert_eq!(reopened, saved);
            assert!(reopened.require_cap_setup().is_err());
            assert_eq!(reopened, saved);
            if reopened.clock.is_some() {
                reopened
                    .advance(Duration::from_millis(BUDGET_STEP_MS))
                    .unwrap();
                assert_eq!(reopened.budget.consumed, 10_000);
            }
        }
    }

    fn natural_branch() -> Scenario {
        let mut scenario = Scenario::new();
        scenario.enable_clock().unwrap();
        for input in [
            "Continue fixing issue #4821",
            "Should we migrate to API v2?",
            "Create related work",
        ] {
            scenario.apply(input).unwrap();
        }
        scenario
    }

    #[test]
    fn natural_clock_raises_owner_attention_without_a_user_request() {
        let mut scenario = natural_branch();
        let user_requests = scenario
            .events
            .iter()
            .filter(|event| event.kind == EventKind::UserRequest)
            .count();
        let before = scenario.clone();
        assert!(scenario.apply("B").is_err());
        assert!(scenario.apply("Show attention").is_err());
        assert_eq!(scenario, before);
        scenario
            .advance(Duration::from_millis(ATTENTION_DELAY_MS - 1))
            .unwrap();
        assert!(!scenario.attention_pending());
        scenario.advance(Duration::from_millis(1)).unwrap();
        assert!(scenario.attention_pending());
        assert_eq!(scenario.selected_work_id, MIGRATION);
        let event = scenario
            .events
            .iter()
            .find(|event| event.kind == EventKind::AttentionRaised)
            .unwrap();
        assert_eq!(event.work_id, FIX);
        assert!(event.input.is_none());
        assert_eq!(
            scenario
                .events
                .iter()
                .filter(|event| event.kind == EventKind::UserRequest)
                .count(),
            user_requests
        );
        let shown = scenario.clone();
        scenario.advance(Duration::from_secs(100)).unwrap();
        assert_eq!(scenario, shown);
        scenario.validate().unwrap();
    }

    #[test]
    fn natural_decision_drives_timed_budget_and_show_commands_only_navigate() {
        let mut scenario = natural_branch();
        scenario
            .advance(Duration::from_millis(ATTENTION_DELAY_MS))
            .unwrap();
        scenario.apply("B").unwrap();
        assert_eq!(scenario.decision.as_ref().unwrap().work_id, FIX);
        assert_eq!(scenario.works.len(), 3);
        assert_eq!(scenario.works[0].evidence[3].exit_code, 1);
        scenario.apply("Show budget").unwrap();
        assert_eq!(scenario.budget.consumed, 0);
        assert!(!scenario.budget.paused);
        for expected in [5_000, 10_000, 15_000, 20_000] {
            scenario
                .advance(Duration::from_millis(BUDGET_STEP_MS))
                .unwrap();
            assert_eq!(scenario.budget.consumed, expected);
            assert_eq!(scenario.budget.paused, expected == 20_000);
            scenario.validate().unwrap();
        }
        let paused = scenario.clone();
        assert_eq!(paused.works[1].next_step, "Performance Validation");
        scenario.apply("Add 10K budget").unwrap();
        let resumed = scenario.clone();
        assert_eq!(scenario.budget.limit, 30_000);
        assert_eq!(scenario.budget.consumed, 20_000);
        assert!(!scenario.budget.paused);
        assert_eq!(scenario.works[1].findings, paused.works[1].findings);
        assert_eq!(
            scenario.works[1].executor_session_id,
            paused.works[1].executor_session_id
        );
        scenario.advance(Duration::from_secs(500)).unwrap();
        assert_eq!(scenario, resumed);
        let json = serde_json::to_string(&scenario).unwrap();
        let reopened: Scenario = serde_json::from_str(&json).unwrap();
        reopened.validate().unwrap();
        assert_eq!(reopened, resumed);
    }

    #[test]
    fn natural_clock_replay_handles_partial_time_old_v1_and_invalid_system_events() {
        let mut scenario = natural_branch();
        scenario.advance(Duration::from_millis(3_200)).unwrap();
        let mut reopened: Scenario =
            serde_json::from_str(&serde_json::to_string(&scenario).unwrap()).unwrap();
        reopened.advance(Duration::from_millis(6_799)).unwrap();
        assert!(!reopened.attention_pending());
        reopened.advance(Duration::from_millis(1)).unwrap();
        assert!(reopened.attention_pending());
        reopened.validate().unwrap();
        let mut corrupt = reopened.clone();
        corrupt
            .events
            .iter_mut()
            .find(|event| event.kind == EventKind::SystemAdvanced)
            .unwrap()
            .elapsed_ms = None;
        assert!(corrupt.validate().is_err());
        let before = corrupt.clone();
        assert!(corrupt.advance(Duration::from_secs(1)).is_err());
        assert_eq!(corrupt, before);
        let mut legacy = through_decision("B");
        legacy.apply("Show budget").unwrap();
        let old_events = legacy.events.clone();
        let old_works = legacy.works.clone();
        let json = serde_json::to_string(&legacy).unwrap();
        assert!(!json.contains("\"clock\""));
        assert!(!json.contains("\"elapsedMs\""));
        let mut upgraded: Scenario = serde_json::from_str(&json).unwrap();
        upgraded.enable_clock().unwrap();
        assert_eq!(&upgraded.events[..old_events.len()], &old_events);
        assert_eq!(upgraded.works, old_works);
        let migrated = upgraded.clone();
        upgraded.advance(Duration::from_secs(600)).unwrap();
        assert_eq!(upgraded, migrated);
        upgraded.validate().unwrap();
        assert!(upgraded.enable_clock().is_err());
    }

    #[test]
    fn natural_large_and_incremental_advances_have_identical_outcomes() {
        let mut large = natural_branch();
        large
            .advance(Duration::from_millis(ATTENTION_DELAY_MS))
            .unwrap();
        large.apply("B").unwrap();
        let mut incremental = large.clone();
        large
            .advance(Duration::from_millis(BUDGET_STEP_MS * 4))
            .unwrap();
        for _ in 0..4 {
            incremental
                .advance(Duration::from_millis(BUDGET_STEP_MS))
                .unwrap();
        }
        assert_eq!(large.works, incremental.works);
        assert_eq!(large.budget, incremental.budget);
        assert_eq!(large.clock, incremental.clock);
        assert_eq!(
            large
                .events
                .iter()
                .filter(|event| event.kind == EventKind::BudgetReached)
                .count(),
            1
        );
        large.validate().unwrap();
        incremental.validate().unwrap();
    }

    fn through_decision(choice: &str) -> Scenario {
        let mut scenario = Scenario::new();
        for input in [
            "Continue fixing issue #4821",
            "Show work state",
            "Should we migrate to API v2?",
            "Create related work",
            "Show schema exchange",
            "Show attention",
            choice,
        ] {
            scenario.apply(input).unwrap();
        }
        scenario
    }

    #[test]
    fn eight_scenes_and_roundtrip() {
        let mut scenario = Scenario::new();
        assert_eq!(scenario.scene(), 1);
        assert_eq!(scenario.works.len(), 1);
        for tab in [
            "Tab 1: Fix bug",
            "Tab 2: Code review",
            "Tab 3: Migration",
            "Tab 4: Research",
        ] {
            assert!(scenario.content().contains(tab));
        }
        assert!(!scenario.content().contains("Status:"));
        assert!(!scenario.content().contains("Fix Issue #4821"));
        for scene in [2, 3, 4, 4, 5, 6, 6, 7, 8] {
            scenario.apply(scenario.next_input()).unwrap();
            assert_eq!(scenario.scene(), scene);
            scenario.validate().unwrap();
            let json = serde_json::to_string(&scenario).unwrap();
            let reopened: Scenario = serde_json::from_str(&json).unwrap();
            assert_eq!(reopened, scenario);
            reopened.validate().unwrap();
        }
        assert_eq!(scenario.works.len(), 3);
        for label in [
            "Status:",
            "Progress:",
            "Blockers:",
            "Decisions:",
            "Deliverables:",
            "Acceptance:",
        ] {
            assert_eq!(scenario.content().matches(label).count(), 3);
        }
        assert!(scenario.works.iter().all(|w| w.acceptance == ACCEPTANCE));
        let overview = scenario.content();
        assert!(overview.lines().count() <= 32);
        assert!(overview.lines().all(|line| line.chars().count() <= 132));
        assert_eq!(overview.matches("Acceptance: Pending").count(), 3);
        for detail in [
            "Next Step:",
            "Completed:",
            "command=",
            "WorkExecutor",
            "simulated-api-v2-executor",
        ] {
            assert!(!overview.contains(detail), "{detail}");
        }
        assert!(scenario
            .events
            .windows(2)
            .all(|w| w[0].sequence < w[1].sequence && w[0].revision <= w[1].revision));
    }

    #[test]
    fn scene_presentation_reveals_only_relevant_detail() {
        let mut scenario = Scenario::new();
        scenario.apply("/resume").unwrap();
        let resume = scenario.content();
        assert!(resume.contains("Found existing Work: Fix Issue #4821"));
        assert!(resume.contains("Repository: microsoft/foo"));
        assert!(resume.contains("Branch: fix-4821"));
        assert_eq!(resume.matches("(verified)").count(), 3);
        assert!(resume.contains("Next Step: Investigate shell integration behavior"));
        assert!(!resume.contains("Acceptance:"));
        assert!(!resume.contains("exitCode"));
        scenario.apply("/state").unwrap();
        let state = scenario.content();
        for label in [
            "Completed:",
            "Blocked:",
            "Pending:",
            "sample-fix-4821-compatibility; exitCode=1",
        ] {
            assert!(state.contains(label), "{label}");
        }
        assert!(!state.contains("command="));
        scenario.apply("/branch").unwrap();
        scenario.apply("Create related work").unwrap();
        let branch = scenario.content();
        for detail in [
            "Fix Issue #4821 -> Investigate API v2",
            "Repository: microsoft/foo",
            "Relevant background: Shell integration consumes the affected API.",
            "Shared findings: Compatibility fails for the legacy shell adapter.",
            "Independent goal:",
            "Independent lifecycle: Investigating",
        ] {
            assert!(branch.contains(detail), "{detail}");
        }
        assert!(!branch.contains("executor"));
        scenario.apply("/attention").unwrap();
        assert!(scenario.content().contains("Why B: preserve the fix"));
        scenario.apply("B").unwrap();
        scenario.apply("/budget").unwrap();
        assert!(scenario
            .content()
            .contains("Schema 2.3 saved; 3 breaking changes"));
        assert!(scenario
            .content()
            .contains("Remaining Work: Performance Validation"));
    }

    #[test]
    fn rejected_inputs_and_ordering_are_atomic() {
        let mut scenario = Scenario::new();
        for input in [
            "Continue fixing issue #1234",
            "agent finished",
            "",
            "yes",
            "Show work state",
            "Create related work",
            "Show schema exchange",
            "B",
            "Show budget",
            "Add 10K budget",
            "Show work overview",
        ] {
            let before = scenario.clone();
            assert!(scenario.apply(input).is_err(), "{input}");
            assert_eq!(scenario, before);
        }
        scenario.apply("  CONTINUE FIXING ISSUE #4821  ").unwrap();
        for input in [
            "Create related work",
            "Show schema exchange",
            "Show attention",
            "B",
            "Show budget",
            "Show work overview",
        ] {
            let before = scenario.clone();
            assert!(scenario.apply(input).is_err(), "{input}");
            assert_eq!(scenario, before);
        }
    }

    #[test]
    fn branching_is_confirmed_independent_and_automatic() {
        let mut scenario = Scenario::new();
        scenario.apply("/resume").unwrap();
        let original = scenario.works[0].clone();
        scenario.apply("/branch").unwrap();
        assert!(scenario.branch_confirmation_pending);
        assert_eq!(scenario.works.len(), 1);
        assert!(scenario.exchange.is_none());
        scenario.apply("Cancel related work").unwrap();
        assert!(scenario.apply("Create related work").is_err());
        scenario.apply("/branch").unwrap();
        scenario.apply("Create related work").unwrap();
        assert_eq!(scenario.works[0], original);
        let related = &scenario.works[1];
        assert_eq!(related.parent_work_id.as_deref(), Some(FIX));
        assert_ne!(related.executor_session_id, original.executor_session_id);
        assert_ne!(related.goal, original.goal);
        assert_eq!(related.context.repository, original.repository);
        assert_eq!(related.context.background.len(), 1);
        assert!(!serde_json::to_string(&related.context)
            .unwrap()
            .contains("transcript"));
        let exchange = scenario.exchange.as_ref().unwrap();
        assert!(!exchange.request.blocking);
        assert_eq!(exchange.request.consumer, MIGRATION);
        assert_eq!(exchange.response.version, "2.3");
        assert_eq!(exchange.response.breaking_changes, 3);
        let automatic: Vec<_> = scenario
            .events
            .iter()
            .filter(|e| e.input.is_none())
            .collect();
        assert_eq!(automatic.len(), 3);
        assert_eq!(automatic[1].kind, EventKind::SchemaRequest);
        assert_eq!(automatic[2].kind, EventKind::SchemaResponse);
        assert!(automatic.iter().all(|e| e.revision == scenario.revision()));
        let before = scenario.clone();
        scenario.apply("Create related work").unwrap();
        assert_eq!(scenario, before);
    }

    #[test]
    fn evidence_not_claims_or_decisions_drives_progress() {
        let mut scenario = through_decision("Fix compatibility");
        let fix = &scenario.works[0];
        assert_eq!(
            fix.steps
                .iter()
                .filter(|s| s.status == StepStatus::Completed)
                .count(),
            3
        );
        assert_eq!(fix.steps[3].status, StepStatus::Blocked);
        assert_eq!(fix.evidence[3].exit_code, 1);
        assert!(fix.evidence.iter().all(|e| e.source == SOURCE));
        assert_eq!(scenario.decision.as_ref().unwrap().choice, "B");
        assert_eq!(fix.status, "Compatibility recheck queued");
        assert_eq!(fix.evidence, Scenario::new().works[0].evidence);
        let before = scenario.clone();
        scenario.apply("B").unwrap();
        assert_eq!(scenario, before);
        assert!(scenario.apply("A").is_err());
        assert_eq!(scenario, before);
        scenario.apply("/evidence").unwrap();
        assert!(scenario
            .content()
            .contains("command='fixture-test shell-compatibility'"));
        assert!(scenario.content().contains("source=scripted-fixture"));
        let before = scenario.clone();
        scenario.apply("agent finished").unwrap_err();
        assert_eq!(scenario, before);
        scenario.apply("/evidence").unwrap();
        assert_eq!(scenario, before);
    }

    #[test]
    fn budget_is_exact_and_resume_preserves_identity_and_findings() {
        let mut scenario = through_decision("B");
        let migration = scenario.works[1].clone();
        scenario.apply("Show budget").unwrap();
        assert_eq!(scenario.budget.consumed, 20_000);
        assert_eq!(scenario.budget.limit, 20_000);
        assert_eq!(scenario.budget.priority, "Low");
        assert_eq!(scenario.budget.max_concurrency, 1);
        assert!(scenario.budget.paused);
        assert_eq!(scenario.works[1].status, "Paused");
        assert_eq!(scenario.works[1].next_step, "Performance Validation");
        assert_eq!(scenario.budget.resume_condition, "More budget available");
        let paused = scenario.clone();
        scenario.apply("Show budget").unwrap();
        assert_eq!(scenario, paused);
        scenario.apply("Add 10K budget").unwrap();
        assert_eq!(scenario.budget.limit, 30_000);
        assert_eq!(scenario.budget.consumed, 20_000);
        assert!(!scenario.budget.paused);
        assert_eq!(
            scenario.works[1].executor_session_id,
            migration.executor_session_id
        );
        assert_eq!(scenario.works[1].findings, migration.findings);
        assert_eq!(scenario.works[1].evidence, migration.evidence);
        let resumed = scenario.clone();
        scenario.apply("Add 10K budget").unwrap();
        scenario.apply("Show budget").unwrap();
        assert_eq!(scenario, resumed);
        scenario.validate().unwrap();
    }

    #[test]
    fn alternate_decisions_do_not_execute_or_erase_evidence() {
        for choice in ["A", "C"] {
            let mut scenario = through_decision(choice);
            assert_eq!(
                scenario.works[0].evidence,
                Scenario::new().works[0].evidence
            );
            assert_eq!(scenario.works[0].steps[3].status, StepStatus::Blocked);
            assert_eq!(scenario.decision.as_ref().unwrap().choice, choice);
            scenario.apply("/budget").unwrap();
            scenario.apply("/overview").unwrap();
            assert_eq!(scenario.works.len(), 3);
            scenario.validate().unwrap();
        }
    }

    #[test]
    fn corrupt_snapshots_are_rejected() {
        let scenario = through_decision("B");
        let mut corruptions = vec![];
        let mut bad = scenario.clone();
        bad.format_version = 2;
        corruptions.push(bad);
        let mut bad = scenario.clone();
        bad.scene = 9;
        corruptions.push(bad);
        let mut bad = scenario.clone();
        bad.revision += 1;
        corruptions.push(bad);
        let mut bad = scenario.clone();
        bad.works[0].evidence[3].exit_code = 0;
        corruptions.push(bad);
        let mut bad = scenario.clone();
        bad.works[0].steps[3].status = StepStatus::Completed;
        corruptions.push(bad);
        let mut bad = scenario.clone();
        bad.works[1].executor_session_id = bad.works[0].executor_session_id.clone();
        corruptions.push(bad);
        let mut bad = scenario.clone();
        bad.budget.limit = 19_999;
        corruptions.push(bad);
        let mut bad = scenario.clone();
        bad.events[0].sequence = 0;
        corruptions.push(bad);
        let mut bad = scenario.clone();
        bad.events[0].input = None;
        corruptions.push(bad);
        let mut bad = scenario.clone();
        bad.exchange.as_mut().unwrap().request.blocking = true;
        corruptions.push(bad);
        for mut bad in corruptions {
            assert!(bad.validate().is_err());
            let before = bad.clone();
            assert!(bad.apply("/state").is_err());
            assert_eq!(bad, before);
        }
    }
}
