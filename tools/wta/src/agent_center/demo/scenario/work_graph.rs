// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

//! Persisted, scripted Work relationships and independently budgeted runtimes.

use super::*;

pub(in crate::agent_center) const IMPLEMENTATION: &str = "implementation";
pub(in crate::agent_center) const COMPATIBILITY: &str = "compatibility";
pub(in crate::agent_center) const STARTUP: &str = "startup-warning";
pub(in crate::agent_center) const PLAN: &str =
    "Plan the fix. Require implementation and compatibility. High priority, 30K each.";
pub(in crate::agent_center) const START: &str = "Start the required work.";
pub(in crate::agent_center) const RESUME: &str = "Continue yesterday's fix.";
pub(in crate::agent_center) const PROGRESS: &str = "Where are we on the fix?";
pub(in crate::agent_center) const RELATED: &str =
    "Investigate the startup warning. Related work, Low priority, 5K cap.";
pub(in crate::agent_center) const AUDIT: &str = "Summarize progress, decisions and spend.";
pub(in crate::agent_center) const EXPLAIN: &str = "What is blocking compatibility?";
pub(in crate::agent_center) const DECISION: &str =
    "Has compatibility been verified? Show evidence and next steps.";
pub(in crate::agent_center) const EVIDENCE: &str = "Show the compatibility evidence.";
pub(in crate::agent_center) const HANDOFF: &str =
    "Record the decision: preserve legacy double quotes. Send it to compatibility and run the check.";
pub(in crate::agent_center) const HANDOFF_STATUS: &str = "Show decision handoff.";
pub(in crate::agent_center) const CHECK_COMMAND: &str =
    "node --test --test-reporter=tap test-compatibility.cjs";
pub(in crate::agent_center) const RECHECK_ID: &str = "local-compatibility-recheck-1";
const DECISION_ID: &str = "decision-legacy-quoting-1";

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(in crate::agent_center) enum HandoffStage {
    Recorded,
    Delivered,
    Acknowledged,
    Checking,
    Completed,
    Failed,
}

impl HandoffStage {
    pub(in crate::agent_center) fn terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::agent_center) struct Handoff {
    pub decision_id: String,
    pub work_id: String,
    pub executor_session_id: String,
    pub decision: String,
    pub stage: HandoffStage,
    pub worker_pid: Option<u32>,
    pub failure: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::agent_center) struct Receipt {
    pub decision_id: String,
    pub work_id: String,
    pub executor_session_id: String,
    pub update: WorkerUpdate,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::agent_center) enum WorkerUpdate {
    Delivered {
        pid: u32,
    },
    Acknowledged,
    CheckStarted,
    CheckCompleted {
        #[serde(rename = "exitCode")]
        exit_code: i32,
        stdout: String,
        stderr: String,
    },
    Failed {
        message: String,
    },
}

impl Handoff {
    pub(in crate::agent_center) fn receipt(&self, update: WorkerUpdate) -> Receipt {
        Receipt {
            decision_id: self.decision_id.clone(),
            work_id: self.work_id.clone(),
            executor_session_id: self.executor_session_id.clone(),
            update,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::agent_center) enum Priority {
    High,
    Low,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(in crate::agent_center) enum RuntimeStatus {
    Queued,
    Running,
    Finished,
    PausedAtCap,
}

impl RuntimeStatus {
    pub(in crate::agent_center) fn label(self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::Running => "Running",
            Self::Finished => "Finished",
            Self::PausedAtCap => "Paused at cap",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::agent_center) struct Execution {
    pub work_id: String,
    pub priority: Priority,
    pub cap: u64,
    pub used: u64,
    pub checkpoints: u64,
    pub status: RuntimeStatus,
    pub assignment_turns: u32,
    pub direct_chat_turns: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::agent_center) struct Graph {
    pub required: Vec<String>,
    pub related: Vec<String>,
    pub executions: Vec<Execution>,
    pub started: bool,
    pub next_tick_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff: Option<Handoff>,
}

impl Scenario {
    pub(super) fn enable_work_graph(&mut self) -> Result<Vec<AutomaticEvent>> {
        ensure!(
            !self.resumed
                && !self.is_work_model()
                && self.work_graph.is_none()
                && self.clock.is_some(),
            "Open the Work graph only in a fresh natural demo"
        );
        self.works = vec![Work::fixture(
            FIX,
            "Deliver fix #4821 safely",
            "Implement the fix and preserve legacy compatibility.",
            &[
                (IMPLEMENTATION, "Implementation"),
                (COMPATIBILITY, "Compatibility"),
            ],
        )];
        let goal = self.work_mut(FIX)?;
        goal.context.findings.clear();
        goal.status = "Awaiting work breakdown".into();
        self.work_graph = Some(Graph::default());
        Ok(vec![])
    }

    pub(in crate::agent_center) fn graph(&self) -> Result<&Graph> {
        self.work_graph
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Open the Work graph first"))
    }

    fn graph_mut(&mut self) -> Result<&mut Graph> {
        self.work_graph
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("Open the Work graph first"))
    }

    fn add_graph_work(
        &mut self,
        id: &str,
        title: &str,
        priority: Priority,
        cap: u64,
    ) -> Result<()> {
        let criterion = match id {
            IMPLEMENTATION => "Patch implemented; all three unit tests pass.",
            COMPATIBILITY => "Legacy quoting contract passes independently.",
            _ => "Record a startup-warning diagnosis; do not change the release gate.",
        };
        let mut work = Work::fixture(id, title, criterion, &[("check", criterion)]);
        work.definition = Some(Definition {
            workspace: format!("microsoft/foo / {}", work.branch),
            agent_runtime: "Copilot / default model (simulated)".into(),
            priority: format!("{priority:?}"),
            acceptance_criteria: vec![criterion.into()],
            execution_instruction: Some(
                "Preserve the legacy API. Report evidence, not just completion.".into(),
            ),
        });
        work.context.background = vec![format!("Goal Work: {FIX}; {}", self.works[0].goal)];
        work.context.findings = if id == STARTUP {
            vec!["Source: goal work specification. Startup warning is related, not required; no executor transcript copied.".into()]
        } else {
            vec![]
        };
        self.works.push(work);
        self.graph_mut()?.executions.push(Execution {
            work_id: id.into(),
            priority,
            cap,
            used: 0,
            checkpoints: 0,
            status: RuntimeStatus::Queued,
            assignment_turns: 0,
            direct_chat_turns: 0,
        });
        Ok(())
    }

    pub(super) fn reduce_work_graph(&mut self, input: &str) -> Result<Vec<AutomaticEvent>> {
        ensure!(
            input == PLAN || !self.graph()?.required.is_empty(),
            "First: plan the required Works"
        );
        self.selected_work_id = FIX.into();
        let mut automatic = vec![];
        match input {
            PLAN => {
                ensure!(
                    self.graph()?.required.is_empty(),
                    "The breakdown is already saved"
                );
                self.add_graph_work(IMPLEMENTATION, "Implement the fix", Priority::High, 30_000)?;
                self.add_graph_work(
                    COMPATIBILITY,
                    "Verify compatibility",
                    Priority::High,
                    30_000,
                )?;
                self.graph_mut()?.required = vec![IMPLEMENTATION.into(), COMPATIBILITY.into()];
                self.work_mut(FIX)?.status = "Two required Works; not ready".into();
                automatic.extend([
                    (
                        EventKind::WorkCreated,
                        IMPLEMENTATION,
                        "REQUIRED by fix-4821; High priority; 30K cap; independent runtime.",
                    ),
                    (
                        EventKind::WorkCreated,
                        COMPATIBILITY,
                        "REQUIRED by fix-4821; High priority; 30K cap; independent runtime.",
                    ),
                ]);
            }
            START => {
                ensure!(
                    !self.graph()?.started,
                    "The existing runtimes have already been started"
                );
                self.graph_mut()?.started = true;
                self.schedule_graph()?;
            }
            RESUME => self.resumed = true,
            PROGRESS | AUDIT | DECISION | EVIDENCE | HANDOFF_STATUS => {}
            HANDOFF => {
                ensure!(
                    self.graph()?.handoff.is_none(),
                    "Decision already recorded; use Show decision handoff."
                );
                ensure!(
                    self.graph()?
                        .executions
                        .iter()
                        .any(|execution| execution.work_id == COMPATIBILITY
                            && execution.status == RuntimeStatus::Running),
                    "The compatibility executor must be running"
                );
                let work = self.work_mut(COMPATIBILITY)?;
                ensure!(
                    work.evidence.last().is_some_and(|e| e.exit_code != 0),
                    "Record the original failing compatibility evidence first"
                );
                let decision =
                    "Preserve legacy double quotes; recheck the unchanged implementation.";
                work.decisions.push(format!("{DECISION_ID}: {decision}"));
                let binding = work.executor_session_id.clone();
                self.work_mut(FIX)?
                    .decisions
                    .push(format!("{DECISION_ID}: {decision}"));
                self.graph_mut()?.handoff = Some(Handoff {
                    decision_id: DECISION_ID.into(),
                    work_id: COMPATIBILITY.into(),
                    executor_session_id: binding,
                    decision: decision.into(),
                    stage: HandoffStage::Recorded,
                    worker_pid: None,
                    failure: None,
                });
                automatic.push((EventKind::DecisionRecorded, COMPATIBILITY,
                    "Decision recorded for compatibility: preserve legacy double quotes. Delivery and new evidence are still pending."));
            }
            RELATED => {
                ensure!(
                    self.graph()?.related.is_empty(),
                    "The related investigation already exists"
                );
                self.add_graph_work(STARTUP, "Investigate startup warning", Priority::Low, 5_000)?;
                self.graph_mut()?.related.push(STARTUP.into());
                self.schedule_graph()?;
                automatic.push((EventKind::WorkCreated, STARTUP,
                    "RELATED, not required by fix-4821; Low priority; 5K cap; own runtime and goal-spec handoff."));
            }
            EXPLAIN => {
                let execution = self
                    .graph_mut()?
                    .executions
                    .iter_mut()
                    .find(|execution| execution.work_id == COMPATIBILITY)
                    .ok_or_else(|| anyhow::anyhow!("Compatibility Work is missing"))?;
                ensure!(
                    execution.status == RuntimeStatus::Running,
                    "Direct Work chat requires its running runtime"
                );
                execution.direct_chat_turns += 1;
                self.selected_work_id = COMPATIBILITY.into();
            }
            _ => bail!("Unsupported Work graph input; use Tab for the next request"),
        }
        Ok(automatic)
    }

    fn schedule_graph(&mut self) -> Result<()> {
        if !self.graph()?.started {
            return Ok(());
        }
        let graph = self.graph_mut()?;
        let mut admitted = vec![];
        graph.executions.sort_by_key(|execution| execution.priority);
        let mut slots = 2_usize.saturating_sub(
            graph
                .executions
                .iter()
                .filter(|execution| execution.status == RuntimeStatus::Running)
                .count(),
        );
        for execution in &mut graph.executions {
            if slots > 0 && execution.status == RuntimeStatus::Queued {
                execution.status = RuntimeStatus::Running;
                execution.assignment_turns += 1;
                admitted.push(execution.work_id.clone());
                slots -= 1;
            }
        }
        if graph.next_tick_ms.is_none()
            && graph
                .executions
                .iter()
                .any(|execution| execution.status == RuntimeStatus::Running)
        {
            let now = self
                .clock
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Missing demo clock"))?
                .elapsed_ms;
            self.graph_mut()?.next_tick_ms = Some(
                now.checked_add(BUDGET_STEP_MS)
                    .ok_or_else(|| anyhow::anyhow!("Demo clock overflow"))?,
            );
        }
        for id in admitted {
            self.work_mut(&id)?.status = "Running".into();
        }
        Ok(())
    }

    pub(super) fn advance_work_graph(&mut self, elapsed_ms: u64) -> Result<()> {
        if elapsed_ms == 0 || self.graph()?.next_tick_ms.is_none() {
            return Ok(());
        }
        let now = self
            .clock
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Missing demo clock"))?
            .elapsed_ms
            .checked_add(elapsed_ms)
            .ok_or_else(|| anyhow::anyhow!("Demo clock overflow"))?;
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("Demo revision overflow"))?;
        self.event(
            EventKind::SystemAdvanced,
            FIX.into(),
            None,
            "Scripted independent runtimes advanced; no provider accounting.",
        );
        if let Some(event) = self.events.last_mut() {
            event.elapsed_ms = Some(elapsed_ms);
        }
        while self.graph()?.next_tick_ms.is_some_and(|due| due <= now) {
            let due = self
                .graph()?
                .next_tick_ms
                .ok_or_else(|| anyhow::anyhow!("Missing graph deadline"))?;
            let mut updates = vec![];
            for execution in &mut self.graph_mut()?.executions {
                if execution.status != RuntimeStatus::Running {
                    continue;
                }
                let step = if execution.priority == Priority::High {
                    500
                } else {
                    1_000
                };
                execution.used += step.min(execution.cap - execution.used);
                execution.checkpoints += 1;
                if execution.work_id == IMPLEMENTATION && execution.checkpoints == 3 {
                    execution.status = RuntimeStatus::Finished;
                } else if execution.used == execution.cap {
                    execution.status = RuntimeStatus::PausedAtCap;
                }
                updates.push(execution.clone());
            }
            for execution in updates {
                let work = self.work_mut(&execution.work_id)?;
                work.status = execution.status.label().into();
                work.progress = format!(
                    "Checkpoint {}; {} / {} simulated tokens",
                    execution.checkpoints, execution.used, execution.cap
                );
                if execution.work_id == IMPLEMENTATION && execution.checkpoints == 3 {
                    work.record(
                        "check",
                        "node --test test-unit.cjs",
                        0,
                        "3/3 unit tests passed (scripted fixture).",
                    );
                } else if execution.work_id == COMPATIBILITY && execution.checkpoints == 2 {
                    work.record("check", "node --test test-compatibility.cjs", 1,
                        "Legacy quoting fails: expected double quotes; received single quotes (scripted fixture).");
                    work.blockers =
                        vec!["Legacy quoting acceptance failed; investigation continues.".into()];
                }
                let summary = format!(
                    "Checkpoint {}: {} / {} simulated tokens; {:?}.",
                    execution.checkpoints, execution.used, execution.cap, execution.status
                );
                self.event(
                    if execution.status == RuntimeStatus::PausedAtCap {
                        EventKind::BudgetReached
                    } else {
                        EventKind::BudgetConsumed
                    },
                    execution.work_id,
                    None,
                    &summary,
                );
            }
            self.clock
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("Missing demo clock"))?
                .elapsed_ms = due;
            self.graph_mut()?.next_tick_ms = None;
            self.schedule_graph()?;
        }
        self.clock
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("Missing demo clock"))?
            .elapsed_ms = now;
        let statuses: Vec<_> = self
            .graph()?
            .required
            .iter()
            .map(|id| {
                let status = self
                    .works
                    .iter()
                    .find(|work| &work.id == id)
                    .and_then(|work| work.steps.first())
                    .map(|step| step.status.clone())
                    .ok_or_else(|| anyhow::anyhow!("Required Work has no check: {id}"))?;
                Ok((id.clone(), status))
            })
            .collect::<Result<_>>()?;
        let passed = self.graph_required_passed();
        let goal = self.work_mut(FIX)?;
        for (id, status) in statuses {
            let step = goal
                .steps
                .iter_mut()
                .find(|step| step.id == id)
                .ok_or_else(|| anyhow::anyhow!("Goal has no prerequisite: {id}"))?;
            step.status = status;
        }
        goal.progress = format!("{passed}/2 required checks passed");
        goal.status = "Waiting for required compatibility evidence".into();
        Ok(())
    }

    pub(in crate::agent_center) fn graph_required_passed(&self) -> usize {
        self.work_graph.as_ref().map_or(0, |graph| {
            graph
                .required
                .iter()
                .filter(|id| {
                    self.works
                        .iter()
                        .find(|work| &work.id == *id)
                        .and_then(|work| work.evidence.last())
                        .is_some_and(|evidence| evidence.exit_code == 0)
                })
                .count()
        })
    }

    pub(in crate::agent_center) fn graph_response(&self, input: &str) -> Result<String> {
        let graph = self.graph()?;
        Ok(match input {
            PLAN => "Saved on the goal: Implementation AND Compatibility are REQUIRED.\nEach has its own workspace, runtime, acceptance criterion and 30K cap. Both are High priority.\nTwo execution slots: High-priority work is admitted first. Nothing runs until you start it.".into(),
            START => "Delegated the two required Works to separate runtimes. I coordinate their reported state; I do not execute their code.".into(),
            HANDOFF => format!("RECORDED: {DECISION_ID}\nTarget Work: compatibility.\nPreserve legacy double quotes; recheck the unchanged implementation.\nDelivery, acknowledgement and a new check result are still pending."),
            HANDOFF_STATUS => self.graph_handoff_status()?,
            RESUME => {
                let mut text = format!("Found your goal and its two saved prerequisites.\n{}/2 required checks pass. Same Work IDs, runtime bindings, budgets and evidence.\nOpening the goal does not restart execution.", self.graph_required_passed());
                if graph.handoff.is_some() {
                    text.push_str(&format!("\n{}", self.graph_handoff_status()?));
                }
                text
            },
            PROGRESS => format!("{}/2 required checks pass. The goal is NOT ready.\n{}",
                self.graph_required_passed(),
                if self.works.iter().find(|work| work.id == COMPATIBILITY).is_some_and(|work| !work.evidence.is_empty()) {
                    self.works.iter().find(|work| work.id == COMPATIBILITY)
                        .and_then(|work| work.evidence.last())
                        .map(|evidence| format!("Source: {}, exit {}.\nThis status query used registered state; no prompt was sent to either executor.", evidence.id, evidence.exit_code))
                        .ok_or_else(|| anyhow::anyhow!("Compatibility evidence is missing"))?
                } else {
                    "Independent checks are still pending. I used registered Work state, not a new prompt to a busy executor.".into()
                }),
            RELATED => "Created startup-warning: RELATED, not a release prerequisite.\nLow priority; 5K cap; separate workspace and runtime. Handoff: goal specification only, not another agent's transcript.\nExisting executor contexts and assignments are unchanged.".into(),
            DECISION => {
                let mut text = format!(
                    "DECISION: {}; {}/2 required checks passed.\n",
                    if self.graph_required_passed() == 2 { "checks passed; acceptance pending" } else { "not ready" },
                    self.graph_required_passed()
                );
                for (id, label) in [(IMPLEMENTATION, "Unit tests"), (COMPATIBILITY, "Compatibility")] {
                    let work = self.works.iter().find(|work| work.id == id)
                        .ok_or_else(|| anyhow::anyhow!("Required Work is missing: {id}"))?;
                    match work.evidence.last() {
                        Some(evidence) => text.push_str(&format!(
                            "{label}: {} / exit {}.\nSource: {}.\n",
                            if evidence.exit_code == 0 { "PASSED" } else { "FAILED" },
                            evidence.exit_code, evidence.id
                        )),
                        None => text.push_str(&format!("{label}: NOT YET VERIFIED; no result recorded.\n")),
                    }

                }
                text.push_str("A unit-test pass does not verify legacy compatibility.\nNEXT: review the compatibility evidence; resolve any failure and require a passing check.\nRecorded evidence only; no new test or executor prompt.");
                text
            }
            EVIDENCE => {
                let work = self.works.iter().find(|work| work.id == COMPATIBILITY)
                    .ok_or_else(|| anyhow::anyhow!("Compatibility Work is missing"))?;
                if let Some(evidence) = work.evidence.last() {
                    format!(
                        "EVIDENCE: {}\nRequirement: preserve legacy argument quoting.\nCommand: {}\nRecorded exit: {} ({})\nResult: {}\nScope: this legacy quoting check, not every compatibility case.\n{}\nFinal acceptance remains pending.",
                        evidence.id, evidence.command, evidence.exit_code,
                        if evidence.exit_code == 0 { "PASSED" } else { "FAILED" },
                        evidence.summary.replace(" (scripted fixture)", ""),
                        if evidence.exit_code == 0 { "NEXT: review the scoped passing evidence." } else { "NEXT: preserve expected quoting, then rerun this check.\nNo passing rerun or final acceptance is recorded." }
                    )
                } else {
                    "NOT YET VERIFIED: no compatibility evidence is recorded.\nRequirement: preserve legacy argument quoting.\nNEXT: obtain the independent check result before deciding.\nThis query did not run a test or prompt an executor.".into()
                }
            }
            AUDIT => {
                let mut text = format!("SNAPSHOT / event #{}\nGOAL: {}/2 required checks pass; acceptance pending.\n",
                    self.events.len(), self.graph_required_passed());
                for execution in &graph.executions {
                    text.push_str(&format!("{}: {:?}; {} / {} tokens; {}.\n",
                        execution.work_id, execution.priority, execution.used, execution.cap, execution.status.label()));
                }
                text.push_str("DECISIONS / event sources:\n");
                for event in &self.events {
                    if matches!(event.kind, EventKind::WorkCreated | EventKind::BudgetReached | EventKind::DecisionRecorded | EventKind::DemoExecutorUpdate) {
                        text.push_str(&format!("#{} {}: {}\n", event.sequence, event.work_id, event.summary));
                    }
                }
                for work in &self.works {
                    for evidence in &work.evidence {
                        text.push_str(&format!("{} / {}: {} -> exit {}.\n", work.id, evidence.id, evidence.command, evidence.exit_code));
                    }
                }
                text.push_str("Summary reads recorded state; executor contexts are untouched. Simulated tokens, not provider quota.");
                text
            }
            EXPLAIN => if self.works.iter().find(|work| work.id == COMPATIBILITY).is_some_and(|work| !work.evidence.is_empty()) {
                "My required legacy check expects double-quoted arguments; the implementation returns single quotes.\nI own this compatibility investigation and its 30K cap. New passing evidence is required.\nThis direct conversation belongs to my execution context.".into()
            } else {
                "My independent compatibility check is still pending. No result has been recorded yet.\nThis direct conversation belongs to my execution context.".into()
            },
            _ => bail!("Unsupported Work graph response"),
        })
    }

    pub(in crate::agent_center) fn receive_graph_receipt(
        &mut self,
        receipt: Receipt,
    ) -> Result<()> {
        self.validate()?;
        let mut next = self.clone();
        next.receive_graph_receipt_recorded(receipt)?;
        *self = next;
        Ok(())
    }

    pub(super) fn receive_graph_receipt_recorded(&mut self, receipt: Receipt) -> Result<()> {
        let current = self
            .graph()?
            .handoff
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No decision is awaiting delivery"))?;
        ensure!(
            receipt.decision_id == current.decision_id
                && receipt.work_id == current.work_id
                && receipt.executor_session_id == current.executor_session_id,
            "Executor receipt does not match the decision, Work and binding"
        );
        let stage = current.stage;
        let (next_stage, summary) = match &receipt.update {
            WorkerUpdate::Delivered { pid } => {
                ensure!(
                    stage == HandoffStage::Recorded && *pid > 0,
                    "Unexpected decision delivery"
                );
                (
                    HandoffStage::Delivered,
                    format!("DELIVERED: {DECISION_ID} to compatibility; local worker PID {pid}."),
                )
            }
            WorkerUpdate::Acknowledged => {
                ensure!(
                    stage == HandoffStage::Delivered,
                    "Acknowledgement requires delivery"
                );
                (HandoffStage::Acknowledged, format!("ACKNOWLEDGED: {DECISION_ID} received by compatibility. Criterion: preserve legacy double quotes."))
            }
            WorkerUpdate::CheckStarted => {
                ensure!(
                    stage == HandoffStage::Acknowledged,
                    "Check requires acknowledged decision"
                );
                (HandoffStage::Checking, format!("CHECK STARTED: {CHECK_COMMAND}\nDecision: {DECISION_ID}; no new result recorded yet."))
            }
            WorkerUpdate::CheckCompleted {
                exit_code,
                stdout,
                stderr,
            } => {
                ensure!(
                    stage == HandoffStage::Checking,
                    "Result requires a started check"
                );
                ensure!(
                    matches!(exit_code, 0 | 1)
                        && stdout.len() <= 65_536
                        && stderr.len() <= 16_384
                        && stdout.contains("# tests 1"),
                    "Invalid or incomplete local check output"
                );
                let work = self.work_mut(COMPATIBILITY)?;
                work.evidence.push(Evidence {
                    id: RECHECK_ID.into(),
                    step_id: "check".into(),
                    command: CHECK_COMMAND.into(),
                    exit_code: *exit_code,
                    source: "local-demo-check-worker".into(),
                    summary: format!("Fresh local legacy quoting check: exit {exit_code}. Captured output is retained in the executor receipt."),
                });
                work.refresh_progress();
                work.blockers = if *exit_code == 0 {
                    vec![]
                } else {
                    vec!["Fresh check still fails. The decision was received; the implementation still needs repair.".into()]
                };
                work.next_step = if *exit_code == 0 {
                    "Review the scoped evidence; final acceptance remains separate.".into()
                } else {
                    "Repair legacy quoting under the recorded decision, then rerun compatibility."
                        .into()
                };
                (HandoffStage::Completed, format!(
                    "NEW EVIDENCE: {RECHECK_ID}\n{CHECK_COMMAND} -> exit {exit_code}.\nDecision received; a fresh check ran.\nFinal acceptance remains pending."
                ))
            }
            WorkerUpdate::Failed { message } => {
                ensure!(
                    !stage.terminal() && !message.trim().is_empty() && message.len() <= 4_096,
                    "Invalid executor failure"
                );
                (
                    HandoffStage::Failed,
                    format!("HANDOFF FAILED: {message}\nNo new check result may be assumed."),
                )
            }
        };
        let handoff = self
            .graph_mut()?
            .handoff
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("Missing decision handoff"))?;
        handoff.stage = next_stage;
        if let WorkerUpdate::Delivered { pid } = &receipt.update {
            handoff.worker_pid = Some(*pid);
        }
        if let WorkerUpdate::Failed { message } = &receipt.update {
            handoff.failure = Some(message.clone());
        }
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("Demo revision overflow"))?;
        self.event(
            EventKind::DemoExecutorUpdate,
            COMPATIBILITY.into(),
            None,
            &summary,
        );
        let event = self
            .events
            .last_mut()
            .ok_or_else(|| anyhow::anyhow!("Missing receipt event"))?;
        event.source = "local-demo-check-worker".into();
        event.executor_receipt = Some(receipt);
        Ok(())
    }

    pub(in crate::agent_center) fn graph_handoff_status(&self) -> Result<String> {
        let Some(handoff) = &self.graph()?.handoff else {
            return Ok("No decision handoff recorded. No new compatibility check has run.".into());
        };
        let mut text = format!(
            "DECISION: {}\nTarget: {} / same executor binding.\nStatus: {:?}\n",
            handoff.decision_id, handoff.work_id, handoff.stage
        );
        if handoff.stage == HandoffStage::Completed {
            let evidence = self
                .works
                .iter()
                .find(|work| work.id == COMPATIBILITY)
                .and_then(|work| work.evidence.last())
                .ok_or_else(|| anyhow::anyhow!("Completed handoff has no evidence"))?;
            text.push_str(&format!("Recorded -> Delivered -> Acknowledged -> Checked.\nFresh evidence: {} / exit {}.\n",
                evidence.id, evidence.exit_code));
            text.push_str(if evidence.exit_code == 0 {
                "The scoped check passed; final acceptance remains pending."
            } else {
                "Compatibility still fails. NEXT: repair the implementation under the recorded contract, then rerun.\nThe handoff completed; the goal did not."
            });
        } else if let Some(error) = &handoff.failure {
            text.push_str(&format!("Failure: {error}\nNo new check result recorded."));
        } else {
            text.push_str("No new check result recorded yet. The original failure is retained.");
        }
        Ok(text)
    }

    pub(super) fn graph_next_input(&self) -> &'static str {
        let Some(graph) = &self.work_graph else {
            return PLAN;
        };
        if graph.required.is_empty() {
            PLAN
        } else if !graph.started {
            START
        } else if !self.resumed {
            RESUME
        } else if !self
            .events
            .iter()
            .any(|event| event.input.as_deref() == Some(PROGRESS))
        {
            PROGRESS
        } else if graph.related.is_empty() {
            RELATED
        } else {
            AUDIT
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn planned() -> Scenario {
        let mut saved = Scenario::new();
        saved.enable_clock().unwrap();
        saved.apply("Open Work graph").unwrap();
        saved.apply(PLAN).unwrap();
        saved
    }

    fn handoff_ready() -> Scenario {
        let mut saved = planned();
        saved.apply(START).unwrap();
        saved.advance(Duration::from_secs(9)).unwrap();
        saved.apply(HANDOFF).unwrap();
        saved
    }

    #[test]
    fn graph_handoff_requires_ordered_targeted_receipts_and_preserves_unrelated_work() {
        let mut saved = handoff_ready();
        let request = saved.graph().unwrap().handoff.clone().unwrap();
        let old = saved.clone();
        assert!(saved.apply(HANDOFF).is_err());
        assert_eq!(saved, old);
        assert!(saved
            .receive_graph_receipt(request.receipt(WorkerUpdate::Acknowledged))
            .is_err());
        let mut wrong = request.receipt(WorkerUpdate::Delivered { pid: 42 });
        wrong.work_id = IMPLEMENTATION.into();
        assert!(saved.receive_graph_receipt(wrong).is_err());
        assert_eq!(saved, old);
        for update in [
            WorkerUpdate::Delivered { pid: 42 },
            WorkerUpdate::Acknowledged,
            WorkerUpdate::CheckStarted,
        ] {
            saved
                .receive_graph_receipt(request.receipt(update))
                .unwrap();
            assert_eq!(
                saved.works[2].evidence.len(),
                1,
                "No result may be invented before completion"
            );
            saved.validate().unwrap();
        }
        saved
            .receive_graph_receipt(request.receipt(WorkerUpdate::CheckCompleted {
                exit_code: 1,
                stdout: "TAP version 13\n# tests 1\n# pass 0\n# fail 1\n".into(),
                stderr: String::new(),
            }))
            .unwrap();
        assert_eq!(saved.works[1], old.works[1]);
        assert_eq!(
            saved.graph().unwrap().executions,
            old.graph().unwrap().executions
        );
        assert_eq!(
            saved.works[2].executor_session_id,
            old.works[2].executor_session_id
        );
        assert_eq!(saved.works[2].evidence.len(), 2);
        assert_eq!(saved.works[2].evidence[1].id, RECHECK_ID);
        assert_eq!(saved.works[2].evidence[1].source, "local-demo-check-worker");
        assert_eq!(saved.graph_required_passed(), 1);
        assert!(saved
            .graph_response(HANDOFF_STATUS)
            .unwrap()
            .contains("The handoff completed; the goal did not"));
        assert!(saved.graph_response(PROGRESS).unwrap().contains(RECHECK_ID));
        let final_state = saved.clone();
        assert!(saved
            .receive_graph_receipt(request.receipt(WorkerUpdate::CheckStarted))
            .is_err());
        assert_eq!(saved, final_state);
        saved.validate().unwrap();
        let restored: Scenario =
            serde_json::from_str(&serde_json::to_string(&saved).unwrap()).unwrap();
        restored.validate().unwrap();
        assert_eq!(saved, restored);
    }

    #[test]
    fn graph_handoff_failures_and_queries_do_not_fabricate_new_evidence() {
        let mut saved = planned();
        let initial = saved.clone();
        assert!(saved.apply(HANDOFF).is_err());
        assert_eq!(saved, initial);
        saved = handoff_ready();
        let request = saved.graph().unwrap().handoff.clone().unwrap();
        saved.advance(Duration::from_secs(30)).unwrap();
        assert_eq!(
            saved.graph().unwrap().handoff.as_ref().unwrap().stage,
            HandoffStage::Recorded
        );
        assert_eq!(
            saved.works[2].evidence.len(),
            1,
            "Clock activity is not a check receipt"
        );
        saved
            .receive_graph_receipt(request.receipt(WorkerUpdate::Failed {
                message: "Node worker unavailable".into(),
            }))
            .unwrap();
        for input in [RESUME, PROGRESS, DECISION, EVIDENCE, HANDOFF_STATUS, AUDIT] {
            saved.apply(input).unwrap();
            assert_eq!(saved.works[2].evidence.len(), 1);
        }
        assert!(saved
            .graph_response(HANDOFF_STATUS)
            .unwrap()
            .contains("No new check result recorded"));
        assert!(!saved
            .graph_response(HANDOFF_STATUS)
            .unwrap()
            .contains("Acknowledged -> Checked"));
        saved.validate().unwrap();
    }

    #[test]
    fn graph_management_does_not_prompt_restart_or_rebind_executors() {
        let mut saved = planned();
        saved.apply(START).unwrap();
        saved.advance(Duration::from_secs(9)).unwrap();
        let before = saved.graph().unwrap().executions.clone();
        let workers = saved.works[1..].to_vec();
        for input in [
            RESUME, PROGRESS, PROGRESS, DECISION, EVIDENCE, AUDIT, RELATED,
        ] {
            saved.apply(input).unwrap();
        }
        assert_eq!(&saved.graph().unwrap().executions[..2], &before);
        assert_eq!(&saved.works[1..3], &workers);
        assert_eq!(
            saved.graph().unwrap().required,
            [IMPLEMENTATION, COMPATIBILITY]
        );
        assert_eq!(saved.graph().unwrap().related, [STARTUP]);
        assert_eq!(saved.graph_required_passed(), 1);
        assert_eq!(saved.works[3].context.findings.len(), 1);
        saved.advance(Duration::from_secs(3)).unwrap();
        assert!(saved.graph().unwrap().executions[1].checkpoints > before[1].checkpoints);
        saved.apply(EXPLAIN).unwrap();
        assert_eq!(saved.graph().unwrap().executions[1].direct_chat_turns, 1);
        saved.validate().unwrap();
        let restored: Scenario =
            serde_json::from_str(&serde_json::to_string(&saved).unwrap()).unwrap();
        restored.validate().unwrap();
        assert_eq!(saved, restored);
    }

    #[test]
    fn graph_decision_distinguishes_missing_evidence_from_recorded_failure() {
        let mut saved = planned();
        let pending = saved.graph_response(DECISION).unwrap();
        assert!(pending.contains("0/2 required checks passed"));
        assert_eq!(pending.matches("NOT YET VERIFIED").count(), 2);
        assert!(!pending.contains("FAILED"));
        assert!(saved
            .graph_response(EVIDENCE)
            .unwrap()
            .contains("no compatibility evidence"));
        saved.apply(DECISION).unwrap();
        saved.apply(EVIDENCE).unwrap();
        saved.apply(START).unwrap();
        saved.advance(Duration::from_secs(6)).unwrap();
        let partial = saved.graph_response(DECISION).unwrap();
        assert!(partial.contains("Unit tests: NOT YET VERIFIED"));
        assert!(partial.contains("Compatibility: FAILED / exit 1"));
        saved.advance(Duration::from_secs(3)).unwrap();
        saved.apply(DECISION).unwrap();
        saved.apply(EVIDENCE).unwrap();
        let decision = saved.graph_response(DECISION).unwrap();
        assert!(decision.contains("1/2 required checks passed"));
        assert!(decision.contains("Unit tests: PASSED / exit 0"));
        assert!(decision.contains("Compatibility: FAILED / exit 1"));
        assert!(decision.contains("unit-test pass does not verify legacy compatibility"));
        for work in &saved.works[1..] {
            assert!(decision.contains(&work.evidence[0].id));
        }
        let evidence = saved.graph_response(EVIDENCE).unwrap();
        assert!(evidence.contains("node --test test-compatibility.cjs"));
        assert!(evidence.contains("expected double quotes; received single quotes"));
        assert!(evidence.contains("not every compatibility case"));
        assert!(evidence.contains("No passing rerun or final acceptance"));
        saved.validate().unwrap();
        let restored: Scenario =
            serde_json::from_str(&serde_json::to_string(&saved).unwrap()).unwrap();
        restored.validate().unwrap();
        assert_eq!(restored.graph_response(DECISION).unwrap(), decision);
        assert_eq!(restored.graph_response(EVIDENCE).unwrap(), evidence);
    }

    #[test]
    fn graph_priority_orders_admission_and_low_cap_cannot_take_high_allocation() {
        let mut saved = planned();
        saved.apply(RELATED).unwrap();
        saved.apply(START).unwrap();
        assert_eq!(
            saved.graph().unwrap().executions[2].status,
            RuntimeStatus::Queued
        );
        saved.advance(Duration::from_secs(24)).unwrap();
        let graph = saved.graph().unwrap();
        assert_eq!(graph.executions[0].status, RuntimeStatus::Finished);
        assert_eq!(graph.executions[1].status, RuntimeStatus::Running);
        assert_eq!(graph.executions[2].status, RuntimeStatus::PausedAtCap);
        assert_eq!(graph.executions[2].used, 5_000);
        let paused = graph.executions[2].clone();
        let important = graph.executions[1].used;
        saved.advance(Duration::from_secs(30)).unwrap();
        assert_eq!(saved.graph().unwrap().executions[2], paused);
        assert!(saved.graph().unwrap().executions[1].used > important);
        saved.advance(Duration::from_secs(1_000)).unwrap();
        assert!(saved
            .graph()
            .unwrap()
            .executions
            .iter()
            .all(|execution| execution.used <= execution.cap));
        assert_eq!(saved.graph_required_passed(), 1);
        saved.validate().unwrap();
        let last = saved.clone();
        saved.advance(Duration::from_secs(10_000)).unwrap();
        assert_eq!(saved, last);
    }

    #[test]
    fn graph_rejects_duplicate_dispatch_and_tampered_relationships_or_budgets() {
        let mut saved = planned();
        saved.apply(START).unwrap();
        saved.apply(RELATED).unwrap();
        let before = saved.clone();
        for input in [PLAN, START, RELATED, "Add 10K budget"] {
            assert!(saved.apply(input).is_err());
            assert_eq!(saved, before);
        }
        saved.graph_mut().unwrap().required.pop();
        assert!(saved.validate().is_err());
        saved = before.clone();
        saved.graph_mut().unwrap().executions[2].cap = 30_000;
        assert!(saved.validate().is_err());
        let legacy = Scenario::new();
        assert!(!serde_json::to_string(&legacy)
            .unwrap()
            .contains("workGraph"));
    }
}
