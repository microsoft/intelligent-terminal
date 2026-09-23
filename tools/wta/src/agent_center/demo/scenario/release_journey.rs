// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

//! One isolated, replayable release story; coordination is scripted, check receipts are local.

use super::*;

pub(in crate::agent_center) const OPEN: &str = "Open release journey";
pub(in crate::agent_center) const OPEN_PARALLEL: &str = "Open parallel Work journey";
pub(in crate::agent_center) const GOAL: &str = "Help me get fix #4821 ready for release.";
pub(in crate::agent_center) const CONFIRM: &str =
    "Yes. Preserve legacy behavior. Both checks must pass.";
pub(in crate::agent_center) const START: &str = "Create it and start.";
pub(in crate::agent_center) const REPAIR: &str = "Proceed. Keep the legacy contract.";
pub(in crate::agent_center) const OVERVIEW: &str = "Show my Work.";
pub(in crate::agent_center) const DIAGNOSE: &str = "While that runs, investigate the startup warning. Report the cause and a safe fix; don't change files.";
pub(in crate::agent_center) const START_DIAGNOSIS: &str = "Start the separate diagnosis Work.";
pub(in crate::agent_center) const PROGRESS: &str = "What needs my attention across my Work?";
pub(in crate::agent_center) const DIAGNOSIS: &str = "startup-diagnosis";
pub(in crate::agent_center) const INPUTS: [&str; 10] = [
    OPEN,
    OPEN_PARALLEL,
    GOAL,
    CONFIRM,
    START,
    REPAIR,
    OVERVIEW,
    DIAGNOSE,
    START_DIAGNOSIS,
    PROGRESS,
];

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(in crate::agent_center) enum DiagnosisStage {
    Draft,
    Assigned,
    Running,
    Ready,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::agent_center) struct Diagnosis {
    pub stage: DiagnosisStage,
}

impl Diagnosis {
    pub(in crate::agent_center) fn active(&self) -> bool {
        matches!(
            self.stage,
            DiagnosisStage::Assigned | DiagnosisStage::Running
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::agent_center) struct DiagnosisReport {
    pub cause: String,
    pub recommendation: String,
    pub files_changed: u32,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(in crate::agent_center) enum Stage {
    Welcome,
    Clarifying,
    Plan,
    Assigned,
    Running,
    Blocked,
    RepairAssigned,
    Repairing,
    Ready,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::agent_center) struct Journey {
    pub stage: Stage,
    pub attempt: u8,
    pub unit: Option<i32>,
    pub compatibility: Option<i32>,
    pub overview: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnosis: Option<Diagnosis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel_story: Option<bool>,
}

impl Journey {
    pub(in crate::agent_center) fn active(&self) -> bool {
        matches!(
            self.stage,
            Stage::Assigned | Stage::Running | Stage::RepairAssigned | Stage::Repairing
        )
    }

    pub(in crate::agent_center) fn next_input(&self) -> &'static str {
        if self
            .diagnosis
            .as_ref()
            .is_some_and(|d| d.stage == DiagnosisStage::Draft)
        {
            return START_DIAGNOSIS;
        }
        match self.stage {
            Stage::Welcome => GOAL,
            Stage::Clarifying => CONFIRM,
            Stage::Plan => START,
            Stage::Blocked => REPAIR,
            _ => OVERVIEW,
        }
    }

    pub(in crate::agent_center) fn input_response(&self, input: &str) -> &'static str {
        match input {
            DIAGNOSE => "I'll create a separate Work for the warning.\nDone means a supported diagnosis and a safe recommendation. No file changes.\nRelease execution continues unchanged. Start it?",
            START_DIAGNOSIS => "Diagnosis Work created and assigned its own execution context.\nOnly its goal and read-only instructions were sent to that worker.\nThe release task and its existing sessions are unchanged.",
            _ => self.response(),
        }
    }

    pub(in crate::agent_center) fn response(&self) -> &'static str {
        match self.stage {
            Stage::Welcome => "Tell me the outcome you want. We will agree on what done means before starting execution.",
            Stage::Clarifying => "For this release, unit tests AND legacy compatibility must pass.\nShould we preserve the existing double-quote contract?",
            Stage::Plan => "Agreed. One release Work, two execution tasks:\n1. Implement the fix and run unit tests.\n2. Independently verify legacy compatibility.\nI'll assign the agents and track both checks under this Work.\nCreate it and start?",
            Stage::Assigned => "Work created. I've assigned implementation and independent verification.\nBoth receive the goal and the agreed legacy contract.",
            Stage::Running => "Execution is running. Results will be checked against the Work's two completion criteria.",
            Stage::Blocked => "Unit tests passed, but compatibility failed.\nThe implementation returns single quotes; our contract requires double quotes.\nThis Work is not ready for release.\nI recommend correcting the implementation and rerunning both checks.",
            Stage::RepairAssigned => "Decision saved on this Work. I've sent the repair to implementation, with the failed check attached.\nVerification will rerun against the corrected implementation.",
            Stage::Repairing => "The local repair is applied. Both checks are running again; earlier failure evidence remains in Work memory.",
            Stage::Ready => "Both required checks pass. The fix is ready for your release approval.\nNothing has been published. The goal, decision and evidence remain with this Work.",
            Stage::Failed => "Execution stopped without a complete result. No success is assumed. See the recorded error before starting a fresh demo.",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::agent_center) struct Receipt {
    pub attempt: u8,
    pub update: Update,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::agent_center) enum Update {
    Started,
    Unit {
        exit_code: i32,
        stdout: String,
        stderr: String,
    },
    Compatibility {
        exit_code: i32,
        stdout: String,
        stderr: String,
    },
    Failed {
        message: String,
    },
    Diagnosis {
        update: DiagnosisUpdate,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::agent_center) enum DiagnosisUpdate {
    Started,
    Completed {
        exit_code: i32,
        stdout: String,
        stderr: String,
    },
    Failed {
        message: String,
    },
}

impl Scenario {
    pub(in crate::agent_center) fn release_progress(&self) -> String {
        let mut lines = vec!["Here is where your Work stands:".to_string()];
        for work in self
            .works
            .iter()
            .filter(|work| work.parent_work_id.is_none())
        {
            lines.extend([String::new(), format!("{}: {}", work.title, work.status)]);
            if work.id == FIX {
                let passed = work
                    .steps
                    .iter()
                    .filter(|step| step.status == StepStatus::Completed)
                    .count();
                lines.push(format!(
                    "Release checks: {passed} / {} passed.",
                    work.steps.len()
                ));
            }
            lines.push(format!("Next: {}", work.next_step));
        }
        lines.extend([
            String::new(),
            "This question stays with coordination; execution contexts are unchanged.".into(),
        ]);
        lines.join("\n")
    }

    pub(super) fn enable_release(&mut self, parallel: bool) -> Result<Vec<AutomaticEvent>> {
        ensure!(
            !self.resumed
                && !self.is_work_model()
                && self.work_graph.is_none()
                && self.work_board.is_none()
                && self.clock.is_some(),
            "Open release journey only in a fresh natural demo"
        );
        self.works.clear();
        self.release_journey = Some(Journey {
            stage: Stage::Welcome,
            attempt: 0,
            unit: None,
            compatibility: None,
            overview: false,
            diagnosis: None,
            parallel_story: parallel.then_some(true),
        });
        Ok(vec![])
    }

    pub(super) fn reduce_release(&mut self, input: &str) -> Result<Vec<AutomaticEvent>> {
        let journey = self
            .release_journey
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("Missing release journey"))?;
        let mut automatic = vec![];
        match input {
            GOAL if journey.stage == Stage::Welcome => journey.stage = Stage::Clarifying,
            CONFIRM if journey.stage == Stage::Clarifying => journey.stage = Stage::Plan,
            START if journey.stage == Stage::Plan => {
                journey.stage = Stage::Assigned;
                journey.attempt = 1;
                let mut goal = Work::fixture(
                    FIX,
                    "Ready fix #4821 for release",
                    "Preserve legacy behavior and pass both release checks.",
                    &[
                        ("unit", "Unit tests pass"),
                        ("compatibility", "Legacy compatibility passes"),
                    ],
                );
                goal.parent_work_id = None;
                goal.executor_session_id.clear();
                goal.status = "Running".into();
                goal.context.findings.clear();
                goal.decisions = vec![
                    "Preserve the legacy double-quote contract. Both checks must pass.".into(),
                ];
                goal.acceptance = "Awaiting evidence; release approval remains human-owned".into();
                goal.next_step = "Implementation and independent verification assigned.".into();
                self.works.push(goal);
                for (id, title, criterion) in [
                    (
                        "release-implementation",
                        "Implement and unit-test the fix",
                        "Unit tests pass",
                    ),
                    (
                        "release-verification",
                        "Verify legacy compatibility",
                        "Legacy double-quote contract passes",
                    ),
                ] {
                    let mut work = Work::fixture(id, title, criterion, &[("check", criterion)]);
                    work.executor_session_id = format!("scripted-{id}-session");
                    work.status = "Assigned".into();
                    work.context.findings.clear();
                    work.context.background =
                        vec!["Release fix #4821; preserve legacy double quotes.".into()];
                    self.works.push(work);
                }
                automatic.push((
                    EventKind::WorkCreated,
                    FIX,
                    "Release Work saved; implementation and verification assigned.",
                ));
            }
            REPAIR if journey.stage == Stage::Blocked => {
                journey.stage = Stage::RepairAssigned;
                journey.attempt = 2;
                journey.unit = None;
                journey.compatibility = None;
                self.works[0].status = "Running".into();
                self.works[0].decisions.push(
                    "Correct the implementation, preserve legacy double quotes, rerun both checks."
                        .into(),
                );
                self.works[0].next_step = "Repair authorized; both checks must run again.".into();
                self.works[0].blockers.clear();
                for work in self.works.iter_mut().take(3) {
                    for step in &mut work.steps {
                        step.status = StepStatus::Pending;
                    }
                    work.refresh_progress();
                }
                self.works[1].status = "Repair assigned".into();
                self.works[2].status = "Waiting for repair".into();
                automatic.push((
                    EventKind::DecisionRecorded,
                    FIX,
                    "Repair decision saved and routed to execution.",
                ));
            }
            DIAGNOSE if journey.attempt > 0 && journey.diagnosis.is_none() => {
                journey.diagnosis = Some(Diagnosis {
                    stage: DiagnosisStage::Draft,
                });
            }
            START_DIAGNOSIS
                if journey
                    .diagnosis
                    .as_ref()
                    .is_some_and(|d| d.stage == DiagnosisStage::Draft) =>
            {
                journey.diagnosis = Some(Diagnosis {
                    stage: DiagnosisStage::Assigned,
                });
                let mut work = Work::fixture(
                    DIAGNOSIS,
                    "Explain the startup warning",
                    "Report a supported diagnosis and a safe recommendation; do not change files.",
                    &[
                        ("cause", "Identify the cause"),
                        ("recommendation", "Recommend a safe fix"),
                    ],
                );
                work.parent_work_id = None;
                work.context.issue.clear();
                work.context.background = vec![
                    "Independent startup-warning diagnosis. Read-only; no file changes.".into(),
                ];
                work.context.findings.clear();
                work.executor_session_id = "scripted-startup-diagnosis-session".into();
                work.status = "Assigned".into();
                work.decisions =
                    vec!["Read-only investigation; report, do not apply a fix.".into()];
                work.acceptance = "Awaiting report; review remains human-owned".into();
                work.next_step = "No action needed; diagnosis is assigned.".into();
                self.works.push(work);
                automatic.push((
                    EventKind::WorkCreated,
                    DIAGNOSIS,
                    "Separate diagnosis Work assigned; release execution unchanged.",
                ));
            }
            PROGRESS if !self.works.is_empty() => return Ok(automatic),
            OVERVIEW if !self.works.is_empty() => journey.overview = true,
            _ => bail!("This request is not available at the current release stage"),
        }
        self.selected_work_id = if matches!(input, DIAGNOSE | START_DIAGNOSIS) {
            DIAGNOSIS
        } else {
            FIX
        }
        .into();
        Ok(automatic)
    }

    pub(in crate::agent_center) fn receive_release(&mut self, receipt: Receipt) -> Result<()> {
        self.validate()?;
        let mut next = self.clone();
        next.receive_release_recorded(receipt)?;
        next.validate()?;
        *self = next;
        Ok(())
    }

    pub(super) fn receive_release_recorded(&mut self, receipt: Receipt) -> Result<()> {
        if matches!(receipt.update, Update::Diagnosis { .. }) {
            return self.receive_diagnosis_recorded(receipt);
        }
        let journey = self
            .release_journey
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("Missing release journey"))?;
        ensure!(
            journey.active() && journey.attempt == receipt.attempt,
            "Stale or unexpected release receipt"
        );
        let summary = match &receipt.update {
            Update::Started => {
                ensure!(
                    matches!(journey.stage, Stage::Assigned | Stage::RepairAssigned),
                    "Execution already started"
                );
                journey.stage = if receipt.attempt == 1 {
                    Stage::Running
                } else {
                    Stage::Repairing
                };
                self.works[1].status = "Running".into();
                self.works[2].status = "Running".into();
                journey.response().to_string()
            }
            Update::Unit {
                exit_code,
                stdout,
                stderr,
            }
            | Update::Compatibility {
                exit_code,
                stdout,
                stderr,
            } => {
                ensure!(
                    matches!(journey.stage, Stage::Running | Stage::Repairing),
                    "Start execution before reporting checks"
                );
                ensure!(
                    stdout.len() <= 65536 && stderr.len() <= 16384,
                    "Release check output exceeds limit"
                );
                let unit = matches!(receipt.update, Update::Unit { .. });
                if unit {
                    ensure!(journey.unit.is_none(), "Unit result already recorded");
                    journey.unit = Some(*exit_code);
                } else {
                    ensure!(
                        journey.unit.is_some() && journey.compatibility.is_none(),
                        "Compatibility result arrived out of order"
                    );
                    journey.compatibility = Some(*exit_code);
                }
                let step = if unit { "unit" } else { "compatibility" };
                let evidence = Evidence {
                    id: format!("release-{}-{step}", receipt.attempt),
                    step_id: step.into(),
                    command: format!("node --test --test-reporter=tap test-{step}.cjs"),
                    exit_code: *exit_code,
                    source: "local-node-release-check".into(),
                    summary: format!("{stdout}\n{stderr}"),
                };
                let status = if *exit_code == 0 {
                    StepStatus::Completed
                } else {
                    StepStatus::Blocked
                };
                let child = &mut self.works[if unit { 1 } else { 2 }];
                child.steps[0].status = status.clone();
                child.status = if *exit_code == 0 {
                    "Check passed"
                } else {
                    "Blocked"
                }
                .into();
                child.evidence.push(Evidence {
                    step_id: "check".into(),
                    ..evidence.clone()
                });
                child.refresh_progress();
                let goal = &mut self.works[0];
                goal.steps[if unit { 0 } else { 1 }].status = status;
                goal.evidence.push(evidence);
                goal.refresh_progress();
                if !unit {
                    if journey.unit == Some(0) && *exit_code == 0 {
                        journey.stage = Stage::Ready;
                        goal.status = "Ready for release approval".into();
                        goal.next_step =
                            "Human release approval; nothing has been published.".into();
                        goal.blockers.clear();
                    } else {
                        journey.stage = Stage::Blocked;
                        goal.status = "Blocked".into();
                        goal.blockers = vec!["Release checks have not all passed.".into()];
                        goal.next_step =
                            "Confirm a compatible repair, then rerun both checks.".into();
                    }
                    journey.response().to_string()
                } else {
                    format!(
                        "Unit tests: {} / exit {exit_code}. Compatibility is still pending.",
                        if *exit_code == 0 { "PASSED" } else { "FAILED" }
                    )
                }
            }
            Update::Failed { message } => {
                ensure!(
                    !message.is_empty() && message.len() <= 4096,
                    "Invalid release worker error"
                );
                journey.stage = Stage::Failed;
                for work in self.works.iter_mut().take(3) {
                    work.status = "Execution interrupted".into();
                    work.blockers.push(message.clone());
                }
                format!("Execution interrupted: {message}. No complete result is assumed.")
            }
            Update::Diagnosis { .. } => bail!("Diagnosis receipt was not routed to its Work"),
        };
        self.revision += 1;
        self.event(EventKind::ReleaseExecutorUpdate, FIX.into(), None, &summary);
        let event = self
            .events
            .last_mut()
            .ok_or_else(|| anyhow::anyhow!("Missing release event"))?;
        event.source = "local-node-release-check".into();
        event.release_receipt = Some(receipt);
        Ok(())
    }

    fn receive_diagnosis_recorded(&mut self, receipt: Receipt) -> Result<()> {
        ensure!(receipt.attempt == 1, "Unexpected diagnosis attempt");
        let diagnosis = self
            .release_journey
            .as_mut()
            .and_then(|j| j.diagnosis.as_mut())
            .ok_or_else(|| anyhow::anyhow!("No diagnosis Work has been requested"))?;
        ensure!(
            diagnosis.active(),
            "Diagnosis receipt cannot start or repeat a finished Work"
        );
        let work = self
            .works
            .iter_mut()
            .find(|work| work.id == DIAGNOSIS)
            .ok_or_else(|| anyhow::anyhow!("Diagnosis Work must be confirmed before execution"))?;
        let Update::Diagnosis { update } = &receipt.update else {
            bail!("Wrong diagnosis receipt");
        };
        let summary = match update {
            DiagnosisUpdate::Started => {
                ensure!(
                    diagnosis.stage == DiagnosisStage::Assigned,
                    "Diagnosis already started"
                );
                diagnosis.stage = DiagnosisStage::Running;
                work.status = "Running".into();
                work.next_step = "No action needed; investigating independently.".into();
                "Diagnosis Work: read-only investigation is running in its own context.".into()
            }
            DiagnosisUpdate::Completed {
                exit_code,
                stdout,
                stderr,
            } => {
                ensure!(
                    diagnosis.stage == DiagnosisStage::Running,
                    "Diagnosis has not started"
                );
                ensure!(
                    stdout.len() <= 16384 && stderr.len() <= 16384,
                    "Diagnosis output exceeds limit"
                );
                let summary = if *exit_code == 0 {
                    let report: DiagnosisReport = serde_json::from_str(stdout)?;
                    ensure!(
                        !report.cause.trim().is_empty()
                            && !report.recommendation.trim().is_empty()
                            && report.files_changed == 0,
                        "Diagnosis must provide a read-only report"
                    );
                    diagnosis.stage = DiagnosisStage::Ready;
                    work.status = "Result ready".into();
                    work.next_step = "Review the diagnosis and safe recommendation.".into();
                    work.findings = vec![report.cause.clone(), report.recommendation.clone()];
                    work.deliverables = vec!["Diagnosis report; no file changes.".into()];
                    for step in &mut work.steps {
                        step.status = StepStatus::Completed;
                    }
                    work.refresh_progress();
                    format!("Diagnosis Work: result ready.\nCause: {}\nSafe fix: {}\nNo files changed. The release Work still follows its own checks.",
                        report.cause, report.recommendation)
                } else {
                    diagnosis.stage = DiagnosisStage::Failed;
                    work.status = "Diagnosis failed".into();
                    work.blockers = vec![format!("Local diagnosis exited {exit_code}: {stderr}")];
                    work.next_step = "Inspect the recorded diagnosis error.".into();
                    format!("Diagnosis Work failed / exit {exit_code}. Release execution was not changed.")
                };
                work.evidence.push(Evidence {
                    id: "local-startup-diagnosis-1".into(),
                    step_id: "cause".into(),
                    command: "node diagnose.cjs".into(),
                    exit_code: *exit_code,
                    source: "local-node-diagnosis".into(),
                    summary: format!("{stdout}\n{stderr}"),
                });
                summary
            }
            DiagnosisUpdate::Failed { message } => {
                ensure!(
                    !message.is_empty() && message.len() <= 4096,
                    "Invalid diagnosis failure"
                );
                diagnosis.stage = DiagnosisStage::Failed;
                work.status = "Diagnosis interrupted".into();
                work.blockers.push(message.clone());
                work.next_step = "Inspect the error; reopening does not restart execution.".into();
                format!("Diagnosis Work interrupted: {message}. Release execution was not changed.")
            }
        };
        self.revision += 1;
        self.event(
            EventKind::ReleaseExecutorUpdate,
            DIAGNOSIS.into(),
            None,
            &summary,
        );
        let event = self
            .events
            .last_mut()
            .ok_or_else(|| anyhow::anyhow!("Missing diagnosis event"))?;
        event.source = "local-node-diagnosis".into();
        event.release_receipt = Some(receipt);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn started() -> Scenario {
        let mut state = Scenario::new();
        state.enable_clock().unwrap();
        state.apply(OPEN).unwrap();
        assert!(state.works.is_empty());
        state.apply(GOAL).unwrap();
        state.apply(CONFIRM).unwrap();
        assert!(state.works.is_empty(), "Draft must not create sessions");
        state.apply(START).unwrap();
        state
    }

    fn deliver(state: &mut Scenario, attempt: u8, update: Update) {
        state.receive_release(Receipt { attempt, update }).unwrap();
    }

    #[test]
    fn parallel_work_journey_progress_questions_only_record_coordination_turns() {
        let mut empty = Scenario::new();
        empty.enable_clock().unwrap();
        empty.apply(OPEN_PARALLEL).unwrap();
        let before = empty.clone();
        assert!(empty.apply(PROGRESS).is_err());
        assert_eq!(empty, before);

        let mut state = started();
        state.apply(DIAGNOSE).unwrap();
        state.apply(START_DIAGNOSIS).unwrap();
        let before = state.clone();
        for _ in 0..2 {
            let events = state.events.len();
            state.apply(PROGRESS).unwrap();
            assert_eq!(state.works, before.works);
            assert_eq!(state.release_journey, before.release_journey);
            assert_eq!(state.selected_work_id, before.selected_work_id);
            assert_eq!(state.events.len(), events + 1);
            assert_eq!(
                state.events.last().unwrap().input.as_deref(),
                Some(PROGRESS)
            );
        }
        let response = state.release_progress();
        assert!(response.contains("Ready fix #4821 for release: Running"));
        assert!(response.contains("Explain the startup warning: Assigned"));
        assert_eq!(response.matches("Release checks:").count(), 1);
        assert!(!response.contains("completion criteria passed"));
        assert!(!response.contains("scripted-"));
        state.validate().unwrap();
        let mut replayed = Scenario::new();
        for event in &state.events {
            replayed.replay_event(event).unwrap();
        }
        assert_eq!(state, replayed);
    }

    #[test]
    fn parallel_work_journey_routes_requests_and_results_without_cross_work_mutation() {
        let mut state = started();
        deliver(&mut state, 1, Update::Started);
        let release = state.works.clone();
        state.apply(DIAGNOSE).unwrap();
        assert_eq!(state.works, release, "A draft is not an execution");
        assert_eq!(state.events.last().unwrap().work_id, DIAGNOSIS);
        let draft = state.clone();
        assert!(state
            .receive_release(Receipt {
                attempt: 1,
                update: Update::Diagnosis {
                    update: DiagnosisUpdate::Started,
                }
            })
            .is_err());
        assert_eq!(state, draft);
        state.apply(START_DIAGNOSIS).unwrap();
        assert_eq!(state.works[..3], release);
        assert_eq!(state.works[3].parent_work_id, None);
        assert!(!serde_json::to_string(&state.works[3].context)
            .unwrap()
            .contains("4821"));
        deliver(
            &mut state,
            1,
            Update::Diagnosis {
                update: DiagnosisUpdate::Started,
            },
        );
        let running_b = state.works[3].clone();
        deliver(
            &mut state,
            1,
            Update::Unit {
                exit_code: 0,
                stdout: "passed".into(),
                stderr: String::new(),
            },
        );
        deliver(
            &mut state,
            1,
            Update::Compatibility {
                exit_code: 1,
                stdout: "failed".into(),
                stderr: String::new(),
            },
        );
        assert_eq!(state.works[3], running_b);
        let blocked_a = state.works[..3].to_vec();
        deliver(&mut state, 1, Update::Diagnosis { update: DiagnosisUpdate::Completed {
            exit_code: 0, stdout: r#"{"cause":"Deprecated setting","recommendation":"Review replacement","filesChanged":0}"#.into(),
            stderr: String::new(),
        }});
        assert_eq!(state.works[..3], blocked_a, "Finishing B cannot advance A");
        let ready_b = state.works[3].clone();
        state.apply(REPAIR).unwrap();
        assert_eq!(state.works[3], ready_b, "Repair must only reset A's gates");
        deliver(
            &mut state,
            2,
            Update::Failed {
                message: "Interrupted release".into(),
            },
        );
        assert_eq!(state.works[3], ready_b, "A's error must not change B");
        assert_eq!(
            state.works[1].executor_session_id,
            release[1].executor_session_id
        );
        assert_eq!(
            state.works[2].executor_session_id,
            release[2].executor_session_id
        );
        state.validate().unwrap();
        let finished = state.clone();
        assert!(state.apply(START_DIAGNOSIS).is_err());
        assert!(state
            .receive_release(Receipt {
                attempt: 1,
                update: Update::Diagnosis {
                    update: DiagnosisUpdate::Started,
                }
            })
            .is_err());
        assert_eq!(state, finished);
    }

    #[test]
    fn parallel_work_journey_rejects_invalid_reports_and_isolates_diagnosis_failure() {
        let mut state = started();
        state.apply(DIAGNOSE).unwrap();
        state.apply(START_DIAGNOSIS).unwrap();
        deliver(
            &mut state,
            1,
            Update::Diagnosis {
                update: DiagnosisUpdate::Started,
            },
        );
        let before = state.clone();
        for stdout in [
            "{}",
            r#"{"cause":"x","recommendation":"y","filesChanged":1}"#,
        ] {
            assert!(state
                .receive_release(Receipt {
                    attempt: 1,
                    update: Update::Diagnosis {
                        update: DiagnosisUpdate::Completed {
                            exit_code: 0,
                            stdout: stdout.into(),
                            stderr: String::new()
                        },
                    }
                })
                .is_err());
            assert_eq!(state, before);
        }
        deliver(
            &mut state,
            1,
            Update::Diagnosis {
                update: DiagnosisUpdate::Failed {
                    message: "Interrupted diagnosis".into(),
                },
            },
        );
        assert_eq!(state.works[..3], before.works[..3]);
        assert_eq!(
            state.release_journey.as_ref().unwrap().stage,
            Stage::Assigned
        );
        state.validate().unwrap();
    }

    #[test]
    fn release_journey_requires_confirmation_evidence_and_retains_failure_after_repair() {
        let mut state = started();
        let bindings: Vec<_> = state
            .works
            .iter()
            .map(|w| w.executor_session_id.clone())
            .collect();
        assert_eq!(state.works.len(), 3);
        assert!(bindings[0].is_empty(), "Main agent does not execute");
        let assigned = state.clone();
        assert!(state.apply(START).is_err());
        assert!(state
            .receive_release(Receipt {
                attempt: 2,
                update: Update::Started
            })
            .is_err());
        assert_eq!(state, assigned);
        for attempt in [1, 2] {
            deliver(&mut state, attempt, Update::Started);
            deliver(
                &mut state,
                attempt,
                Update::Unit {
                    exit_code: 0,
                    stdout: "unit output".into(),
                    stderr: String::new(),
                },
            );
            assert_ne!(state.release_journey.as_ref().unwrap().stage, Stage::Ready);
            deliver(
                &mut state,
                attempt,
                Update::Compatibility {
                    exit_code: if attempt == 1 { 1 } else { 0 },
                    stdout: "compatibility output".into(),
                    stderr: String::new(),
                },
            );
            if attempt == 1 {
                assert_eq!(
                    state.release_journey.as_ref().unwrap().stage,
                    Stage::Blocked
                );
                state.apply(REPAIR).unwrap();
            }
        }
        assert_eq!(state.release_journey.as_ref().unwrap().stage, Stage::Ready);
        assert_eq!(state.works[0].evidence.len(), 4);
        assert_eq!(state.works[0].evidence[1].exit_code, 1);
        assert!(state.works[0].acceptance.contains("human"));
        assert_eq!(
            bindings,
            state
                .works
                .iter()
                .map(|w| w.executor_session_id.clone())
                .collect::<Vec<_>>()
        );
        state.apply(OVERVIEW).unwrap();
        state.validate().unwrap();
        let mut altered = state.clone();
        altered.works[0].evidence[1].exit_code = 0;
        assert!(altered.validate().is_err());
    }

    #[test]
    fn release_journey_interruption_is_terminal_and_never_replays_execution() {
        let mut state = started();
        deliver(
            &mut state,
            1,
            Update::Failed {
                message: "UI closed".into(),
            },
        );
        let failed = state.clone();
        assert!(state.apply(REPAIR).is_err());
        assert!(state
            .receive_release(Receipt {
                attempt: 1,
                update: Update::Started
            })
            .is_err());
        assert_eq!(state, failed);
        state.validate().unwrap();
    }
}
