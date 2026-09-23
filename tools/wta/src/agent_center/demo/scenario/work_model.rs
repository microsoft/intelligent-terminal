// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

//! English fixtures for the explicitly selected, single-Work presentation.

use super::*;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::agent_center) struct Definition {
    pub workspace: String,
    pub agent_runtime: String,
    pub priority: String,
    pub acceptance_criteria: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_instruction: Option<String>,
}

impl Scenario {
    pub(in crate::agent_center) fn is_work_model(&self) -> bool {
        self.work_graph.is_none() && self.works.iter().any(|work| work.definition.is_some())
    }

    pub(in crate::agent_center) fn is_merge_story(&self) -> bool {
        self.events
            .iter()
            .any(|event| event.input.as_deref() == Some("Open merge story"))
    }

    pub(super) fn enable_merge_story(&mut self) -> Result<Vec<AutomaticEvent>> {
        let automatic = self.enable_work_model()?;
        let work = self.work_mut(FIX)?;
        for evidence in &mut work.evidence {
            match evidence.step_id.as_str() {
                "unit-tests" => {
                    evidence.command = "node --test test-unit.cjs".into();
                    evidence.summary = "3/3 unit tests passed (recorded fixture).".into();
                }
                "compatibility" => {
                    evidence.command = "node --test test-compatibility.cjs".into();
                    evidence.summary = "Legacy adapter expects double-quoted arguments; implementation returns single quotes (recorded fixture).".into();
                }
                _ => {}
            }
        }
        work.findings = vec![
            "Implementation's unit tests pass; the independent legacy compatibility gate fails."
                .into(),
        ];
        work.status = "Blocked: compatibility acceptance".into();
        Ok(automatic)
    }

    pub(super) fn enable_work_model(&mut self) -> Result<Vec<AutomaticEvent>> {
        ensure!(
            !self.resumed && !self.is_work_model() && self.clock.is_some(),
            "Open the Work model only in a fresh natural demo"
        );
        self.work_mut(FIX)?.definition = Some(Definition {
            workspace: "microsoft/foo / fix-4821".into(),
            agent_runtime: "Copilot / default model (simulated)".into(),
            priority: "High".into(),
            acceptance_criteria: vec![
                "Unit tests pass".into(),
                "Legacy shell compatibility passes".into(),
                "Documentation and pull request ready".into(),
            ],
            execution_instruction: None,
        });
        self.budget.work_id = FIX.into();
        self.budget.priority = "High".into();
        self.cap_configured = Some(false);
        Ok(vec![])
    }

    pub(super) fn reduce_work_model(&mut self, input: &str) -> Result<Vec<AutomaticEvent>> {
        let mut automatic = vec![];
        ensure!(
            self.resumed
                || matches!(
                    input,
                    "Continue fixing issue #4821" | "Is yesterday's fix ready to merge?"
                ),
            "First: continue the Work or ask whether the fix is ready to merge"
        );
        match input {
            "Continue fixing issue #4821" => {
                self.resumed = true;
                self.scene = 2;
                self.selected_work_id = FIX.into();
            }
            "Is yesterday's fix ready to merge?" => {
                ensure!(self.is_merge_story(), "Open the merge story first");
                self.resumed = true;
                self.selected_work_id = FIX.into();
                self.scene = 2;
            }
            "Proceed. Keep the legacy API unchanged." => {
                ensure!(
                    self.is_merge_story()
                        && self.events.iter().any(|event| event.input.as_deref()
                            == Some("Is yesterday's fix ready to merge?")),
                    "Review merge readiness before authorizing the proposed action"
                );
                ensure!(self.awaiting_cap() && self.budget.consumed == 0, "The initial proposal has already been authorized; use the existing Work controls");
                automatic.extend(self.reduce_work_model("Set token cap to 20K")?);
                self.reduce_work_model("Keep the legacy API unchanged.")?;
                self.reduce_work_model("Investigate compatibility")?;
            }
            "Why did compatibility fail?" => {
                ensure!(self.is_merge_story(), "Open the merge story first")
            }
            "Show work state" | "Show evidence" => {
                self.scene = 3;
                self.evidence_visible = input == "Show evidence";
            }
            "Show budget" => self.scene = 7,
            "Review progress" => {}
            "Keep the legacy API unchanged." => {
                self.work_mut(FIX)?
                    .definition
                    .as_mut()
                    .ok_or_else(|| anyhow::anyhow!("Missing Work definition"))?
                    .execution_instruction = Some(input.into());
            }
            "Why are you paused?" => ensure!(self.budget.paused, "The Work runtime is not paused"),
            "Set token cap to 10K" | "Set token cap to 20K" | "Set token cap to 30K" => {
                let limit = match input {
                    "Set token cap to 10K" => 10_000,
                    "Set token cap to 20K" => 20_000,
                    _ => 30_000,
                };
                ensure!(
                    limit >= self.budget.consumed,
                    "Cap cannot be below recorded usage"
                );
                self.budget.limit = limit;
                self.cap_configured = Some(true);
                if self.budget.consumed == limit {
                    self.pause_work_model()?;
                }
                automatic.push((
                    EventKind::TokenCapSet,
                    FIX,
                    "Work token cap confirmed; usage and evidence retained.",
                ));
            }
            "Investigate compatibility" => {
                ensure!(!self.awaiting_cap(), "Confirm this Work's token cap first");
                ensure!(
                    self.budget.consumed < self.budget.limit,
                    "Add budget before continuing"
                );
                let clock = self
                    .clock
                    .as_mut()
                    .ok_or_else(|| anyhow::anyhow!("Missing demo clock"))?;
                if clock.budget_due_ms.is_none() {
                    clock.budget_due_ms = Some(
                        clock
                            .elapsed_ms
                            .checked_add(BUDGET_STEP_MS)
                            .ok_or_else(|| anyhow::anyhow!("Demo deadline overflow"))?,
                    );
                }
                self.budget.paused = false;
                self.work_mut(FIX)?.status = "Investigating compatibility".into();
                self.scene = 7;
            }
            "Add 10K budget" => {
                ensure!(
                    self.budget.paused,
                    "The Work must be paused before adding budget"
                );
                self.budget.limit = self
                    .budget
                    .limit
                    .checked_add(10_000)
                    .ok_or_else(|| anyhow::anyhow!("Demo token cap overflow"))?;
                self.reduce_work_model("Investigate compatibility")?;
            }
            _ => bail!("This action is unavailable in the single-Work demo"),
        }
        Ok(automatic)
    }

    fn pause_work_model(&mut self) -> Result<()> {
        self.budget.paused = true;
        self.budget.reached = true;
        self.clock
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("Missing demo clock"))?
            .budget_due_ms = None;
        let work = self.work_mut(FIX)?;
        work.status = "Paused at token cap".into();
        work.next_step = "Add budget to continue compatibility investigation".into();
        Ok(())
    }

    pub(super) fn advance_work_model(&mut self, elapsed_ms: u64) -> Result<()> {
        let clock = self
            .clock
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Missing demo clock"))?;
        if elapsed_ms == 0 || clock.budget_due_ms.is_none() {
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
            FIX.into(),
            None,
            "Work runtime simulation advanced.",
        );
        if let Some(event) = self.events.last_mut() {
            event.elapsed_ms = Some(elapsed_ms);
        }
        self.clock
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("Missing demo clock"))?
            .elapsed_ms = now;
        while let Some(due) = self.clock.as_ref().and_then(|clock| clock.budget_due_ms) {
            if due > now {
                break;
            }
            self.budget.consumed = self
                .budget
                .consumed
                .checked_add(BUDGET_STEP_TOKENS)
                .ok_or_else(|| anyhow::anyhow!("Demo usage overflow"))?
                .min(self.budget.limit);
            if self.budget.consumed == self.budget.limit {
                self.pause_work_model()?;
                self.event(
                    EventKind::BudgetReached,
                    FIX.into(),
                    None,
                    "Work paused at cap. Findings, criteria and execution binding retained.",
                );
            } else {
                self.clock
                    .as_mut()
                    .ok_or_else(|| anyhow::anyhow!("Missing demo clock"))?
                    .budget_due_ms = Some(
                    due.checked_add(BUDGET_STEP_MS)
                        .ok_or_else(|| anyhow::anyhow!("Demo deadline overflow"))?,
                );
                self.event(
                    EventKind::BudgetConsumed,
                    FIX.into(),
                    None,
                    "Simulated Work token usage recorded.",
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn story() -> Scenario {
        let mut scenario = Scenario::new();
        scenario.enable_clock().unwrap();
        scenario.require_cap_setup().unwrap();
        scenario.apply("Open Work model").unwrap();
        scenario
    }

    #[test]
    fn work_model_preserves_definition_binding_evidence_and_budget_across_resume() {
        let mut s = story();
        let initial = s.works[0].clone();
        s.apply("Continue fixing issue #4821").unwrap();
        s.apply("Set token cap to 20K").unwrap();
        s.apply("Investigate compatibility").unwrap();
        s.advance(Duration::from_secs(12)).unwrap();
        assert_eq!(s.budget.work_id, initial.id);
        assert_eq!((s.budget.consumed, s.budget.limit), (20_000, 20_000));
        assert!(s.budget.paused);
        s.apply("Continue fixing issue #4821").unwrap();
        assert!(s.budget.paused, "Opening Work cannot restart execution");
        s.apply("Add 10K budget").unwrap();
        assert_eq!((s.budget.consumed, s.budget.limit), (20_000, 30_000));
        assert!(!s.budget.paused);
        s.advance(Duration::from_secs(6)).unwrap();
        assert_eq!(s.budget.consumed, 30_000);
        assert!(s.budget.paused);
        let work = &s.works[0];
        assert_eq!(work.id, initial.id);
        assert_eq!(work.definition, initial.definition);
        assert_eq!(work.executor_session_id, initial.executor_session_id);
        assert_eq!(work.evidence, initial.evidence);
        assert_eq!(work.acceptance, initial.acceptance);
        assert_eq!(work.findings, initial.findings);
        let restored: Scenario = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        restored.validate().unwrap();
        assert_eq!(restored, s);
    }

    #[test]
    fn work_model_rejects_unconfirmed_execution_and_out_of_scope_changes_atomically() {
        let mut s = story();
        s.apply("Continue fixing issue #4821").unwrap();
        let before = s.clone();
        assert!(s.apply("Investigate compatibility").is_err());
        assert!(s.apply("Should we migrate to API v2?").is_err());
        assert!(s.apply("Open Work model").is_err());
        assert_eq!(s, before);
        for cap in ["10K", "20K", "30K"] {
            let mut next = s.clone();
            next.apply(&format!("Set token cap to {cap}")).unwrap();
            next.apply("Investigate compatibility").unwrap();
            next.advance(Duration::from_secs(60)).unwrap();
            assert_eq!(next.budget.consumed, next.budget.limit);
            next.validate().unwrap();
            let stopped = next.clone();
            next.advance(Duration::from_secs(60)).unwrap();
            assert_eq!(next, stopped);
        }
    }

    #[test]
    fn work_model_corrupted_properties_are_rejected_and_legacy_shape_is_unchanged() {
        let legacy = Scenario::new();
        assert!(!serde_json::to_string(&legacy)
            .unwrap()
            .contains("definition"));
        let mut s = story();
        s.works[0].definition.as_mut().unwrap().priority = "Low".into();
        assert!(s.validate().is_err());
    }

    #[test]
    fn work_model_runtime_instruction_survives_budget_and_replay_without_false_acceptance() {
        let mut s = story();
        assert!(s.apply("Keep the legacy API unchanged.").is_err());
        s.apply("Continue fixing issue #4821").unwrap();
        s.apply("Set token cap to 20K").unwrap();
        s.apply("Investigate compatibility").unwrap();
        s.apply("Keep the legacy API unchanged.").unwrap();
        let before = s.clone();
        assert!(s.apply("Why are you paused?").is_err());
        assert_eq!(s, before);
        s.advance(Duration::from_secs(12)).unwrap();
        s.apply("Why are you paused?").unwrap();
        s.apply("Review progress").unwrap();
        s.apply("Add 10K budget").unwrap();
        let definition = s.works[0].definition.as_ref().unwrap();
        assert_eq!(
            definition.execution_instruction.as_deref(),
            Some("Keep the legacy API unchanged.")
        );
        assert_eq!(
            s.works[0].executor_session_id,
            before.works[0].executor_session_id
        );
        assert_eq!(s.works[0].evidence, before.works[0].evidence);
        assert_eq!(s.works[0].acceptance, before.works[0].acceptance);
        let reopened: Scenario = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        reopened.validate().unwrap();
        assert_eq!(reopened, s);
    }

    #[test]
    fn merge_story_requires_a_review_and_authorization_without_changing_failed_evidence() {
        let mut s = Scenario::new();
        s.enable_clock().unwrap();
        s.require_cap_setup().unwrap();
        s.apply("Open merge story").unwrap();
        let before = s.clone();
        assert!(s.apply("Proceed. Keep the legacy API unchanged.").is_err());
        assert_eq!(s, before);
        s.apply("Is yesterday's fix ready to merge?").unwrap();
        assert!(s.clock.as_ref().unwrap().budget_due_ms.is_none());
        s.apply("Proceed. Keep the legacy API unchanged.").unwrap();
        assert!(s.clock.as_ref().unwrap().budget_due_ms.is_some());
        assert_eq!(s.budget.limit, 20_000);
        s.apply("Why did compatibility fail?").unwrap();
        assert_eq!(
            s.works[0].executor_session_id,
            before.works[0].executor_session_id
        );
        assert_eq!(s.works[0].evidence, before.works[0].evidence);
        assert_eq!(s.works[0].acceptance, before.works[0].acceptance);
        assert_eq!(
            s.works[0]
                .definition
                .as_ref()
                .unwrap()
                .execution_instruction
                .as_deref(),
            Some("Keep the legacy API unchanged.")
        );
        let running = s.clone();
        assert!(s.apply("Proceed. Keep the legacy API unchanged.").is_err());
        assert_eq!(s, running);
        s.advance(Duration::from_secs(12)).unwrap();
        assert!(s.budget.paused);
        s.apply("Is yesterday's fix ready to merge?").unwrap();
        assert!(
            s.budget.paused,
            "Readiness queries cannot restart execution"
        );
        let reopened: Scenario = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        reopened.validate().unwrap();
        assert_eq!(reopened, s);
    }
}
