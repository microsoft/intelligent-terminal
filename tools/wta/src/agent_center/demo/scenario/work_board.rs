// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

//! Isolated, replayable goal-first portfolio fixture, not a production executor.

use super::*;

pub(in crate::agent_center) const CREATE: &str = "Create Work: ";
pub(in crate::agent_center) const START: &str = "Start Work: ";
pub(in crate::agent_center) const PLANNED: &str = "Planned";
pub(in crate::agent_center) const RUNNING: &str = "Running";
pub(in crate::agent_center) const BLOCKED: &str = "Blocked";
pub(in crate::agent_center) const DONE: &str = "Done";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::agent_center) struct Board {
    next_id: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Brief {
    goal: String,
    success: String,
}

fn goal(id: &str, title: &str, criteria: &[(&str, &str)]) -> Work {
    let mut work = Work::fixture(id, title, title, criteria);
    work.parent_work_id = None;
    work.context.background.clear();
    work.context.findings.clear();
    work.executor_session_id.clear();
    work.acceptance = "Not accepted".into();
    work.status = PLANNED.into();
    work.next_step = "Start this Work when ready.".into();
    work
}

impl Scenario {
    pub(super) fn enable_work_board(&mut self) -> Result<Vec<AutomaticEvent>> {
        ensure!(
            !self.resumed
                && !self.is_work_model()
                && self.work_graph.is_none()
                && self.work_board.is_none()
                && self.clock.is_some(),
            "Open the Work board only in a fresh isolated demo"
        );
        let mut fix = goal(
            FIX,
            "Ship fix #4821 without regressions",
            &[
                ("unit", "Unit tests pass"),
                ("compatibility", "Legacy compatibility passes"),
            ],
        );
        fix.record("unit", "node --test test-unit.cjs", 0, "Unit tests pass.");
        fix.record(
            "compatibility",
            "node --test test-compatibility.cjs",
            1,
            "Legacy quoting fails.",
        );
        fix.status = BLOCKED.into();
        fix.blockers = vec!["Legacy quoting check failed.".into()];
        fix.next_step = "Decide the compatibility contract, then recheck.".into();
        fix.executor_session_id = "simulated-fix-4821-executor-1".into();

        let mut migration = goal(
            MIGRATION,
            "Decide whether API v2 is worth migrating",
            &[
                ("schema", "Versioned schema captured"),
                ("risks", "Breaking changes assessed"),
                ("recommendation", "Recommendation reviewed"),
            ],
        );
        migration.record("schema", "fixture-schema-check", 0, "Schema 2.3 captured.");
        migration.status = RUNNING.into();
        migration.next_step = "Assess three breaking changes.".into();
        migration.executor_session_id = "simulated-api-v2-executor-1".into();

        let mut docs = goal(
            DOCS,
            "Publish the onboarding guide",
            &[
                ("links", "Guide links validated"),
                ("review", "Content reviewed and published"),
            ],
        );
        docs.record("links", "fixture-link-check", 0, "Guide links pass.");
        docs.record(
            "review",
            "fixture-publication-check",
            0,
            "Publication reviewed.",
        );
        docs.status = DONE.into();
        docs.acceptance = "Accepted in the seeded demo fixture".into();
        docs.next_step = "Published; no action needed.".into();
        docs.deliverables = vec!["Onboarding guide and reviewed links".into()];
        self.works = vec![fix, migration, docs];
        self.selected_work_id = FIX.into();
        self.work_board = Some(Board { next_id: 1 });
        Ok(vec![])
    }

    pub(super) fn reduce_work_board(&mut self, input: &str) -> Result<Vec<AutomaticEvent>> {
        if let Some(payload) = input.strip_prefix(CREATE) {
            let brief: Brief = serde_json::from_str(payload)?;
            let title = brief.goal.trim();
            let success = brief.success.trim();
            for (label, value, limit) in [("Goal", title, 100), ("Done when", success, 240)] {
                ensure!(
                    !value.is_empty()
                        && value.chars().count() <= limit
                        && !value.chars().any(char::is_control),
                    "{label} must contain 1-{limit} characters on one line"
                );
            }
            let board = self
                .work_board
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("Missing Work board"))?;
            let id = format!("board-work-{}", board.next_id);
            board.next_id = board
                .next_id
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("Work ID overflow"))?;
            self.works.push(goal(&id, title, &[("outcome", success)]));
            self.selected_work_id = id;
        } else if let Some(id) = input.strip_prefix(START) {
            let work = self.work_mut(id)?;
            ensure!(
                work.status == PLANNED,
                "Only a Planned Work can be started; opening a Work does not restart it"
            );
            work.status = RUNNING.into();
            work.executor_session_id = format!("simulated-{id}-executor-1");
            work.next_step = "Execution assigned; waiting for outcome evidence.".into();
            self.selected_work_id = id.into();
        } else {
            ensure!(
                input == "Show work overview",
                "Unsupported Work board request"
            );
        }
        Ok(vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board() -> Scenario {
        let mut state = Scenario::new();
        state.enable_clock().unwrap();
        state.apply("Open Work board").unwrap();
        state
    }

    #[test]
    fn board_goals_have_independent_status_and_creation_does_not_launch_a_session() {
        let mut state = board();
        assert_eq!(
            state
                .works
                .iter()
                .map(|work| work.status.as_str())
                .collect::<Vec<_>>(),
            [BLOCKED, RUNNING, DONE]
        );
        assert!(state.works.iter().all(|work| work.parent_work_id.is_none()));
        let original = state.works.clone();
        state.apply(r#"Create Work: {"goal":"Find the startup warning cause","success":"Document root cause and a safe fix"}"#).unwrap();
        let created = state.works.last().unwrap();
        assert_eq!(created.title, "Find the startup warning cause");
        assert_eq!(created.steps[0].title, "Document root cause and a safe fix");
        assert_eq!(created.status, PLANNED);
        assert!(created.executor_session_id.is_empty());
        assert_eq!(&state.works[..3], original);
        state.apply("Start Work: board-work-1").unwrap();
        let started = state.clone();
        assert_eq!(started.works[3].status, RUNNING);
        assert!(started.works[3].evidence.is_empty());
        assert_eq!(started.works[3].acceptance, "Not accepted");
        assert!(state.apply("Start Work: board-work-1").is_err());
        state.advance(Duration::from_secs(60)).unwrap();
        assert_eq!(
            state, started,
            "no invented outcome or automatic completion"
        );
        let loaded: Scenario =
            serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
        loaded.validate().unwrap();
        assert_eq!(loaded, state);
    }

    #[test]
    fn board_invalid_briefs_and_cross_mode_requests_are_atomic() {
        let mut state = board();
        for input in [
            r#"Create Work: {"goal":"","success":"done"}"#,
            r#"Create Work: {"goal":"goal","success":""}"#,
            r#"Create Work: {"goal":"goal\nbad","success":"done"}"#,
            r#"Create Work: {"goal":"goal","success":"done","extra":1}"#,
            "Start Work: missing",
            "Start Work: fix-4821",
            "Open Work graph",
            "Open Work model",
        ] {
            let before = state.clone();
            assert!(state.apply(input).is_err(), "{input}");
            assert_eq!(state, before, "{input}");
        }
        let input = format!(
            "Create Work: {}",
            serde_json::json!({"goal":"x".repeat(101),"success":"done"})
        );
        assert!(state.apply(&input).is_err());
        state.validate().unwrap();
    }
}
