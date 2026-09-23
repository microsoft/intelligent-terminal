// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

//! Isolated story adapter for the ordinary Agent Center views and composer.

use super::*;
use crate::agent_center::demo::{
    scenario::{EventKind, Scenario, StepStatus, Work},
    Store,
};

mod release_journey;
mod work_board;
mod work_graph;
mod work_model;
pub(super) use release_journey::render as render_release_journey;
pub(super) use work_board::render as render_work_board;
pub(super) use work_graph::render as render_work_graph;
pub(super) use work_model::render as render_work_model;

const PROJECT: &str = "demo-microsoft-foo";
const FIX: &str = "fix-4821";
const MIGRATION: &str = "api-v2";

pub(super) struct Presentation {
    pub scenario: Scenario,
    pub overview_scroll: u16,
    pub follow_selection: bool,
    reset_pending: bool,
    pub legacy: bool,
    pub legacy_index: usize,
    pub legacy_views: Vec<WorkView>,
    pub expanded_details: bool,
    menu_return_focus: Option<Focus>,
}

pub(super) const LEGACY_TABS: [&str; 4] = ["Fix bug", "Code review", "Migration", "Research"];

fn legacy_views() -> Vec<WorkView> {
    [
        (
            "Fix issue #4821: the shell reports a failure.",
            "Patched exit-status handling. Unit tests: 42/42 passed.",
            "Is this ready to merge?",
            "Compatibility: 7/8. Not ready to merge.\nDocumentation and PR are pending.",
        ),
        (
            "Review the shell integration patch.",
            "The legacy adapter may expect the original exit-status semantics.",
            "Did the compatibility test pass?",
            "Ask that session for its evidence. My review is still pending.",
        ),
        (
            "Would API v2 simplify our shell adapter?",
            "I need a versioned schema and compatibility notes.",
            "Can we bundle migration into the bug fix?",
            "Performance and breaking changes are unverified.\nKeep the investigation separate.",
        ),
        (
            "Research shell exit-status compatibility.",
            "Two options: translate status at the boundary, or add a compatibility layer.",
            "Which one did we choose?",
            "No decision is recorded here. The fix and review sessions have separate context.",
        ),
    ]
    .into_iter()
    .map(|(first, answer, followup, last)| WorkView {
        messages: vec![
            message("user", first),
            message("assistant", answer),
            message("user", followup),
            message("assistant", last),
        ],
        ..WorkView::default()
    })
    .collect()
}

pub(super) fn banner(state: &State) -> Option<String> {
    if state.menu.is_some() || state.pending.is_some() || state.form.is_some() {
        return None;
    }
    let saved = &state.demo.as_ref()?.scenario;
    if saved.attention_pending() {
        Some("Fix Issue #4821 | Compatibility failed - human decision needed\nB: Fix compatibility, then recheck (recommended) | F6: decide".into())
    } else if saved.clock.is_some() && saved.decision.is_some() {
        if saved.awaiting_cap() {
            return Some(
                "Investigate API v2 | Set token cap to start | F6: 10K / 20K / 30K".into(),
            );
        }
        Some(format!(
            "Investigate API v2 | Tokens: {} / {} | {}",
            saved.budget.consumed,
            saved.budget.limit,
            if saved.budget.paused {
                "Paused at cap"
            } else if saved.budget.reached {
                "Resumed"
            } else {
                "Running"
            }
        ))
    } else {
        None
    }
}

pub(super) fn next_hint(saved: &Scenario) -> String {
    if saved.clock.is_some()
        && saved.resumed
        && saved.exchange.is_none()
        && !saved.branch_confirmation_pending
    {
        return "F5: review evidence before choosing next work".into();
    }
    if saved.clock.is_none()
        || !saved.resumed
        || saved.exchange.is_none() && !saved.branch_confirmation_pending
    {
        return t!("agent_center.demo_next", input = saved.next_input()).into_owned();
    }
    if saved.branch_confirmation_pending {
        "F4: confirm or cancel related Work".into()
    } else if saved.attention_pending() {
        "F6: decide compatibility".into()
    } else if saved.awaiting_cap() && saved.decision.is_some() {
        "F6: set migration token cap (default 20K)".into()
    } else if saved.budget.paused {
        "F6: add 10K budget | F2: Work Overview".into()
    } else if saved.decision.is_some() && !saved.budget.reached {
        "Usage advances automatically | F2: Work Overview".into()
    } else if saved.budget.reached {
        "F2: Work Overview | F5: details".into()
    } else {
        "F5: schema details | Waiting for compatibility review".into()
    }
}

pub(super) fn menu_title(state: &State, menu: &workflow::Menu) -> String {
    let inputs = || {
        menu.items.iter().filter_map(|(_, choice)| match choice {
            workflow::Choice::DemoInput(input) => Some(input.as_str()),
            _ => None,
        })
    };
    if inputs().any(|input| matches!(input, "A" | "B" | "C")) {
        "Fix Issue #4821 | Decision".into()
    } else if inputs().any(|input| input.starts_with("Set token cap")) {
        "Investigate API v2 | Token cap".into()
    } else if inputs().any(|input| matches!(input, "Create related work" | "Cancel related work")) {
        "Fix Issue #4821 | Related work".into()
    } else if inputs().any(|input| input == "Add 10K budget") {
        "Investigate API v2 | Budget".into()
    } else {
        let owner = state
            .selected_view()
            .map(projection::goal)
            .unwrap_or_else(|| "Global conversation".into());
        format!("{owner} | Actions")
    }
}

pub(super) fn f6_hint(state: &State) -> &'static str {
    let Some(demo) = &state.demo else {
        return "F6 Actions";
    };
    let saved = &demo.scenario;
    if saved.branch_confirmation_pending {
        "F6 Branch"
    } else if saved.attention_pending()
        || (saved.clock.is_none() && saved.scene == 6 && saved.decision.is_none())
    {
        "F6 Decision"
    } else if saved.awaiting_cap() && saved.decision.is_some() {
        "F6 Set cap"
    } else if saved.budget.paused {
        "F6 Add budget"
    } else {
        "F6 Actions"
    }
}

fn close_menu(state: &mut State) {
    state.menu = None;
    if let Some(focus) = state
        .demo
        .as_mut()
        .and_then(|demo| demo.menu_return_focus.take())
    {
        state.focus = focus;
    }
}

enum InputAction {
    None,
    Submit(String),
    Quit,
}

fn scenario(state: &State) -> Result<&Scenario> {
    Ok(&state
        .demo
        .as_ref()
        .context("Missing demo presentation")?
        .scenario)
}

fn message(role: &str, text: impl Into<String>) -> Value {
    json!({"role":role, "text":text.into(), "status":"Completed"})
}

fn list(items: &[String]) -> String {
    if items.is_empty() {
        "None recorded".into()
    } else {
        items.join("; ")
    }
}

pub(super) fn fields(work: &Work) -> [(&'static str, String); 6] {
    [
        ("Status", work.status.clone()),
        ("Progress", work.progress.clone()),
        ("Blockers", list(&work.blockers)),
        ("Decisions", list(&work.decisions)),
        ("Deliverables", list(&work.deliverables)),
        ("Acceptance", "Pending".into()),
    ]
}

fn work_summary(work: &Work) -> String {
    fields(work)
        .into_iter()
        .map(|(label, value)| format!("{label}: {value}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn step_facts(work: &Work) -> String {
    [
        ("Completed", StepStatus::Completed),
        ("Blocked", StepStatus::Blocked),
        ("Pending", StepStatus::Pending),
    ]
    .into_iter()
    .map(|(label, status)| {
        let steps = work
            .steps
            .iter()
            .filter(|step| step.status == status)
            .map(|step| step.title.clone())
            .collect::<Vec<_>>();
        format!("{label}: {}", list(&steps))
    })
    .collect::<Vec<_>>()
    .join("\n")
}

fn evidence(work: &Work) -> String {
    let mut text = String::from("Recorded sample evidence, not live execution:\n");
    for item in &work.evidence {
        text.push_str(&format!(
            "{}\ncommand: {}\nexitCode: {} | source: {}\n{}\n",
            item.id, item.command, item.exit_code, item.source, item.summary
        ));
    }
    text
}

fn exchange_text(scenario: &Scenario) -> Result<String> {
    let exchange = scenario
        .exchange
        .as_ref()
        .context("Missing schema exchange")?;
    Ok(format!(
        "Structured Work exchange (sample; no transcript copied)\n\
         Request owner: {MIGRATION}; result producer: {FIX}\n\
         Request:\n{}\nResponse:\n{}\nShare results, not transcripts.",
        serde_json::to_string_pretty(&exchange.request)?,
        serde_json::to_string_pretty(&exchange.response)?
    ))
}

fn reply(scenario: &Scenario, input: &str) -> Result<String> {
    if scenario.is_work_model() {
        return Ok(format!(
            "{input}\nWork definition, evidence and recorded usage retained."
        ));
    }
    let work = scenario
        .works
        .iter()
        .find(|work| work.id == scenario.selected_work_id)
        .context("Missing story Work")?;
    Ok(match input {
        "Continue fixing issue #4821" => format!(
            "Resumed {}.\n{}",
            work.title,
            step_facts(work),
        ),
        "Show work state" => {
            let mut text = format!(
                "{}\n{}\nWork state comes from recorded evidence, not an agent summary.\n",
                work.title,
                work_summary(work)
            );
            for step in &work.steps {
                text.push_str(&format!("  {:?}: {}\n", step.status, step.title));
            }
            text.push_str(
                "Agent finished != Work finished. Use Show evidence for commands and exit codes.",
            );
            text
        }
        "Show evidence" => evidence(work),
        "Should we migrate to API v2?" if scenario.branch_confirmation_pending => {
            "Create Related Work: Investigate API v2?\nThe bug fix keeps its original goal.\nF4: confirm or cancel."
                .into()
        }
        "Should we migrate to API v2?" => {
            "The related Work already exists: Investigate API v2. Open it from F2.".into()
        }
        "Cancel related work" => {
            "Related-work proposal cancelled. No Work created; original goal unchanged.".into()
        }
        "Create related work" => format!(
            "Related Work created: {}\nSchema 2.3 | 3 breaking changes | Proceed with caution\nF5: request, response and compatibility notes",
            work.title,
        ),
        "Show schema exchange" => exchange_text(scenario)?,
        "Show attention" => "Work Needs Attention: Compatibility test failed\n\
            Decision Required: legacy shell behavior prevents verified completion.\n\
            A. Ignore\nB. Fix compatibility (recommended)\nC. Roll back\n\
            Why B: preserve the fix and restore legacy shell behavior, then recheck.\n\
            F4 or F6: choose an explicit decision. Selection is not proof of success."
            .into(),
        "A" | "B" | "C" => {
            let decision = scenario.decision.as_ref().context("Missing decision")?;
            format!(
                "Decision {} recorded.\nCompatibility still failed; recheck required.\nDocumentation follow-up created.",
                decision.choice
            )
        }
        "Add 10K budget" => format!("Cap increased to {}K. Migration resumed.\nSame Work, findings and history.", scenario.budget.limit / 1000),
        "Set token cap to 10K" | "Set token cap to 20K" | "Set token cap to 30K" =>
            format!("Token cap set: {}K\nUsage preserved: {} tokens. {}",
                scenario.budget.limit / 1000, scenario.budget.consumed,
                if scenario.budget.paused { "Paused at cap." }
                else if scenario.decision.is_some() { "Migration can continue." }
                else { "Waiting for the compatibility decision." }),
        "Show budget" => format!(
            "Migration Investigation - Budget and Scheduling\n\
             Budget: {} / {} tokens (simulated)\nPriority: {} | Max Concurrency: {}\n\
             Status: {}\nFindings: Saved\nRemaining Work: {}\nResume Condition: {}\n\
             {}",
            scenario.budget.consumed,
            scenario.budget.limit,
            scenario.budget.priority,
            scenario.budget.max_concurrency,
            if scenario.budget.paused {
                "Paused - Budget Reached"
            } else if !scenario.budget.reached {
                "Investigating"
            } else {
                "Resumed"
            },
            work.next_step,
            scenario.budget.resume_condition,
            if scenario.budget.paused {
                "Add 10K budget resumes this same Work and history. No process is killed."
            } else if !scenario.budget.reached {
                "Simulated consumption advances automatically; this read does not consume budget."
            } else {
                "Same Work ID, executor identity and history retained. No fresh session."
            }
        ),
        "Show work overview" => {
            "Work Overview: all three Works, with status, progress, blockers, decisions, \
             deliverables and acceptance. Select a Work with Up/Down and Enter.\n\
             Don't manage agents. Manage work."
                .into()
        }
        _ => bail!("Unsupported persisted demo input"),
    })
}

fn project(work: &Work, scenario: &Scenario) -> Value {
    let continuation = if work.id == scenario.budget.work_id && scenario.budget.paused {
        "Paused"
    } else if !work.blockers.is_empty() {
        "WaitingForInput"
    } else {
        "Ready"
    };
    json!({
        "work":{"id":work.id, "kind":"Work", "projectId":PROJECT,
            "version":scenario.revision + 1, "lifecycle":"Active"},
        "spec":{"goal":work.title},
        "continuation":{"state":continuation, "canClaimExecutor":false,
            "canRestartSession":false, "pendingInputCount":0},
        "demo":work
    })
}

fn push_work(state: &mut State, id: &str, role: &str, text: impl Into<String>) {
    state
        .views
        .entry(Some(id.into()))
        .or_default()
        .messages
        .push(message(role, text));
}

fn project_histories(state: &mut State, saved: &Scenario) -> Result<()> {
    if saved.release_journey.is_some() {
        return release_journey::histories(state, saved);
    }
    if saved.work_board.is_some() {
        return work_board::histories(state, saved);
    }
    if saved.work_graph.is_some() {
        return work_graph::histories(state, saved);
    }
    if saved.is_work_model() {
        return work_model::histories(state, saved);
    }
    state.chat.messages = vec![message(
        "assistant",
        "One existing Work: Fix Issue #4821.\nResume it here, or browse Works with F2.",
    )];
    for view in state.views.values_mut() {
        view.messages.clear();
        view.records.clear();
    }
    let mut replay = Scenario::new();
    if !saved.resumed {
        push_work(
            state,
            FIX,
            "assistant",
            "microsoft/foo | fix-4821\nUnit tests passed; compatibility failed.",
        );
    }
    for event in &saved.events {
        replay.replay_event(event)?;
        if event.kind != EventKind::UserRequest {
            match event.kind {
                EventKind::AttentionRaised => push_work(
                    state,
                    &event.work_id,
                    "assistant",
                    "Compatibility failed. Your decision is needed (F6).",
                ),
                EventKind::BudgetReached => push_work(
                    state,
                    &event.work_id,
                    "assistant",
                    if replay.is_work_model() {
                        format!("Token cap reached: {}K. Work paused.\nFindings saved; compatibility acceptance remains pending.", replay.budget.limit / 1000)
                    } else {
                        format!("Token cap reached: {}K. Migration paused.\nFindings saved; performance validation remains.", replay.budget.limit / 1000)
                    },
                ),
                _ => {}
            }
            continue;
        }
        let input = event
            .input
            .as_deref()
            .context("Missing recorded demo input")?;
        let response = reply(&replay, input)?;
        push_work(state, &event.work_id, "user", input);
        push_work(state, &event.work_id, "assistant", response);
        if input == "Continue fixing issue #4821" {
            state.chat.messages.push(message("user", input));
            state.chat.messages.push(message(
                "assistant",
                "Opened Fix Issue #4821. F2 returns to your Works.",
            ));
        }
        if input == "Create related work" {
            push_work(
                state,
                FIX,
                "assistant",
                "Shared schema 2.3 with Investigate API v2. Bug-fix goal unchanged.",
            );
        }
    }
    for event in &saved.events {
        if event.kind == EventKind::WorkCreated && event.work_id == "documentation" {
            push_work(
                state,
                &event.work_id,
                "assistant",
                "Documentation follow-up created. Compatibility acceptance is still pending.",
            );
        }
    }
    if let Some(exchange) = &saved.exchange {
        for id in [FIX, MIGRATION] {
            let records = &mut state.views.entry(Some(id.into())).or_default().records;
            records.insert(
                "demo-schema-request".into(),
                json!({"kind":"DemoSchemaRequest","ownerWorkId":MIGRATION,
                    "recipientWorkId":FIX,"request":exchange.request}),
            );
            records.insert(
                "demo-schema-response".into(),
                json!({"kind":"DemoSchemaResponse","ownerWorkId":FIX,
                    "consumerWorkId":MIGRATION,"response":exchange.response}),
            );
        }
    }
    Ok(())
}

fn synchronize(state: &mut State, saved: Scenario) -> Result<()> {
    saved.validate()?;
    state.works = saved
        .works
        .iter()
        .map(|work| project(work, &saved))
        .collect();
    state.projects =
        vec![json!({"id":PROJECT, "name":"microsoft/foo", "root":"microsoft/foo (fixture)"})];
    for work in &saved.works {
        state.views.entry(Some(work.id.clone())).or_default();
    }
    project_histories(state, &saved)?;
    let legacy = !saved.resumed
        && !saved.is_work_model()
        && saved.work_graph.is_none()
        && saved.work_board.is_none()
        && saved.release_journey.is_none();
    match &mut state.demo {
        Some(demo) => demo.scenario = saved,
        None => {
            state.demo = Some(Presentation {
                scenario: saved,
                overview_scroll: 0,
                follow_selection: true,
                reset_pending: false,
                legacy,
                legacy_index: 0,
                legacy_views: legacy_views(),
                expanded_details: false,
                menu_return_focus: None,
            })
        }
    }
    Ok(())
}

fn open_work(state: &mut State, id: &str) -> Result<()> {
    state
        .demo
        .as_mut()
        .context("Missing demo presentation")?
        .legacy = false;
    state
        .demo
        .as_mut()
        .context("Missing demo presentation")?
        .expanded_details = false;
    state
        .demo
        .as_mut()
        .context("Missing demo presentation")?
        .menu_return_focus = None;
    let work = state
        .works
        .iter()
        .find(|work| work["work"]["id"] == id)
        .cloned()
        .context("Unknown demo Work")?;
    state.open_work(&json!({
        "context":{"selectedWorkId":id, "conversationId":format!("demo-chat-{id}"),
            "consoleSessionId":"demo-console", "contextVersion":scenario(state)?.revision + 1,
            "projectId":PROJECT},
        "workView":work,
        "conversation":{"id":format!("demo-chat-{id}"), "kind":"Conversation"}
    }))?;
    state.focus = Focus::Composer;
    state.view_mut().details_open = false;
    Ok(())
}

fn home(state: &mut State) {
    if let Some(demo) = &mut state.demo {
        demo.legacy = false;
        demo.menu_return_focus = None;
    }
    state.sync_view_context();
    let global = state.global_selection.clone();
    state.context.global_conversation = true;
    state.context.work_id = None;
    state.context.project_id = global.project;
    state.context.conversation_id = global.conversation;
    state.context.console_session_id = global.console;
    state.context.context_version = global.version;
    state.task_list = false;
    state.dashboard = false;
    state.menu = None;
    state.focus = Focus::Composer;
    state.view_mut().details_open = false;
}

fn new_state(saved: Scenario) -> Result<State> {
    let mut state = State::new();
    state.task_list = false;
    synchronize(&mut state, saved)?;
    if scenario(&state)?.release_journey.is_some() {
        home(&mut state);
    } else if scenario(&state)?.work_board.is_some() {
        work_board::show(&mut state);
    } else if scenario(&state)?.work_graph.is_some() {
        home(&mut state);
    } else if scenario(&state)?.resumed || scenario(&state)?.is_work_model() {
        let id = scenario(&state)?.selected_work_id.clone();
        open_work(&mut state, &id)?;
        state.task_list = scenario(&state)?.scene == 8;
    }
    Ok(state)
}

pub(super) fn details(state: &State) -> Result<String> {
    if state
        .demo
        .as_ref()
        .is_some_and(|demo| demo.expanded_details)
    {
        return full_details(state);
    }
    let saved = scenario(state)?;
    let id = state
        .selected_view()
        .and_then(|work| work["work"]["id"].as_str());
    let Some(work) = saved.works.iter().find(|work| Some(work.id.as_str()) == id) else {
        return Ok("Select a Work with F2 to inspect its evidence.".into());
    };
    let parent = work
        .parent_work_id
        .as_deref()
        .and_then(|id| saved.works.iter().find(|work| work.id == id))
        .map(|work| work.title.as_str())
        .unwrap_or("None");
    let mut text = format!(
        "Work ID: {} | Parent: {}\nRepository: {} | Branch: {}\nGoal: {}\n{}\nAcceptance: Pending\n\nEvidence (scripted-fixture):\n",
        work.id, parent, work.repository, work.branch, work.goal, step_facts(work)
    );
    for item in &work.evidence {
        text.push_str(&format!(
            "{} | exitCode: {}\n  {}\n",
            item.id, item.exit_code, item.command
        ));
    }
    if work.evidence.is_empty() {
        text.push_str("No recorded checks.\n");
    }
    if let Some(exchange) = saved
        .exchange
        .as_ref()
        .filter(|_| matches!(work.id.as_str(), FIX | MIGRATION))
    {
        text.push_str(&format!(
            "\nSchema exchange | Request owner: api-v2 | Result owner: fix-4821\n\
             Need: {} | Consumer: {}\nReason: {} | Blocking: {} | Expected: {}\n\
             Schema {} | Breaking changes: {} | Source: {}\n\
             Compatibility: {}\nRecommendation: {}\n",
            exchange.request.need,
            exchange.request.consumer,
            exchange.request.reason,
            exchange.request.blocking,
            exchange.request.expected_result,
            exchange.response.version,
            exchange.response.breaking_changes,
            exchange.response.source,
            exchange.response.compatibility_notes.join("; "),
            exchange.response.recommendation
        ));
    }
    if work.id == MIGRATION {
        text.push_str(&format!(
            "\nTokens: {} / {} | Priority: {} | Max concurrency: {}\n\
             Next: {} | Resume: {}\n",
            saved.budget.consumed,
            saved.budget.limit,
            saved.budget.priority,
            saved.budget.max_concurrency,
            work.next_step,
            saved.budget.resume_condition
        ));
    }
    Ok(text)
}

fn full_details(state: &State) -> Result<String> {
    let saved = scenario(state)?;
    let id = state
        .selected_view()
        .and_then(|work| work["work"]["id"].as_str());
    let Some(work) = saved.works.iter().find(|work| Some(work.id.as_str()) == id) else {
        return Ok(
            "Work Overview\nF2: select a Work to inspect its state and sample evidence.".into(),
        );
    };
    let parent = work
        .parent_work_id
        .as_deref()
        .map(|id| {
            saved
                .works
                .iter()
                .find(|work| work.id == id)
                .map(|work| work.title.as_str())
                .unwrap_or(id)
        })
        .unwrap_or("None");
    let mut text = format!(
        "{}\nWork ID: {}\nParent Work: {}\nGoal: {}\nRepository: {}\nBranch: {}\n{}\nNext Step: {}\n\n{}",
        work.title, work.id, parent,
        work.goal, work.repository, work.branch, work_summary(work), work.next_step, evidence(work)
    );
    if saved.exchange.is_some() && matches!(work.id.as_str(), FIX | MIGRATION) {
        text.push_str(&format!("\n{}\n", exchange_text(saved)?));
    }
    if work.id == MIGRATION {
        text.push_str(&format!(
            "\nBudget: {} / {} tokens (simulated)\nPriority: {}\nMax Concurrency: {}\nExecutor: {} (simulated)\n",
            saved.budget.consumed, saved.budget.limit, saved.budget.priority,
            saved.budget.max_concurrency, work.executor_session_id
        ));
    }
    Ok(text)
}

fn menu(state: &State) -> Result<workflow::Menu> {
    let saved = scenario(state)?;
    if saved.awaiting_cap() && saved.decision.is_some() {
        return cap_menu(saved);
    }
    let inputs = if saved.branch_confirmation_pending {
        vec!["Create related work", "Cancel related work"]
    } else if saved.attention_pending()
        || (saved.clock.is_none() && saved.scene == 6 && saved.decision.is_none())
    {
        vec!["B", "A", "C"]
    } else if saved.budget.paused {
        if saved.clock.is_some() {
            vec!["Add 10K budget"]
        } else {
            vec!["Add 10K budget", "Show work overview"]
        }
    } else if saved.clock.is_some() && saved.exchange.is_some() {
        vec![]
    } else if saved.clock.is_some() {
        vec![saved.next_input()]
    } else {
        vec![saved.next_input(), "Show evidence", "Show work overview"]
    };
    let mut items = inputs
        .into_iter()
        .map(|input| {
            let label = match input {
                "B" => "B. Fix compatibility (recommended) - Fix Issue #4821",
                "A" => "A. Ignore - Fix Issue #4821",
                "C" => "C. Roll back - Fix Issue #4821",
                input => input,
            };
            (label.into(), workflow::Choice::DemoInput(input.into()))
        })
        .collect::<Vec<_>>();
    if saved.clock.is_some() && !saved.branch_confirmation_pending {
        items.push(("View Work details".into(), workflow::Choice::DemoDetails));
        items.push(("Work Overview".into(), workflow::Choice::Overview));
        if saved.exchange.is_some() && !saved.attention_pending() {
            items.push((
                "Set migration token cap".into(),
                workflow::Choice::DemoTokenCap,
            ));
        }
    }
    Ok(workflow::Menu { items, selected: 0 })
}

fn cap_menu(saved: &Scenario) -> Result<workflow::Menu> {
    anyhow::ensure!(
        saved.exchange.is_some() || saved.is_work_model(),
        "Create migration before setting its token cap"
    );
    Ok(workflow::Menu {
        items: vec![
            (
                if saved.is_work_model() {
                    "10K tokens".into()
                } else {
                    "Migration cap: 10K tokens".into()
                },
                workflow::Choice::DemoInput("Set token cap to 10K".into()),
            ),
            (
                if saved.is_work_model() {
                    "20K tokens".into()
                } else {
                    "Migration cap: 20K tokens (default)".into()
                },
                workflow::Choice::DemoInput("Set token cap to 20K".into()),
            ),
            (
                if saved.is_work_model() {
                    "30K tokens".into()
                } else {
                    "Migration cap: 30K tokens".into()
                },
                workflow::Choice::DemoInput("Set token cap to 30K".into()),
            ),
            ("Cancel".into(), workflow::Choice::DemoCancel),
        ],
        selected: match saved.budget.limit {
            10_000 => 0,
            30_000 => 2,
            _ => 1,
        },
    })
}

fn key(state: &mut State, code: KeyCode, modifiers: KeyModifiers) -> Result<InputAction> {
    anyhow::ensure!(state.demo.is_some(), "Demo router requires demo state");
    if code == KeyCode::Char('q') && modifiers.contains(KeyModifiers::CONTROL) {
        return Ok(InputAction::Quit);
    }
    if scenario(state)?.release_journey.is_some() {
        if let Some(action) = release_journey::key(state, code)? {
            return Ok(action);
        }
    }
    if scenario(state)?.work_board.is_some() {
        return work_board::key(state, code, modifiers);
    }
    if scenario(state)?.work_graph.is_some() {
        if let Some(action) = work_graph::key(state, code)? {
            return Ok(action);
        }
    }
    if scenario(state)?.is_work_model() {
        if let Some(action) = work_model::key(state, code)? {
            return Ok(action);
        }
    }
    if state.demo.as_ref().is_some_and(|demo| demo.legacy) {
        let demo = state.demo.as_mut().context("Missing demo presentation")?;
        match code {
            KeyCode::Left => demo.legacy_index = (demo.legacy_index + 3) % LEGACY_TABS.len(),
            KeyCode::Right => demo.legacy_index = (demo.legacy_index + 1) % LEGACY_TABS.len(),
            KeyCode::F(1) | KeyCode::F(2) => {}
            KeyCode::PageUp | KeyCode::PageDown => {
                let view = &mut demo.legacy_views[demo.legacy_index];
                view.follow = false;
                view.scroll = if code == KeyCode::PageUp {
                    view.scroll.saturating_sub(10)
                } else {
                    view.scroll.saturating_add(10)
                };
            }
            _ => {}
        }
        if !matches!(code, KeyCode::F(1) | KeyCode::F(2)) {
            return Ok(InputAction::None);
        }
    }
    match code {
        KeyCode::F(1) => {
            home(state);
            return Ok(InputAction::None);
        }
        KeyCode::F(2) => {
            close_menu(state);
            state
                .demo
                .as_mut()
                .context("Missing demo presentation")?
                .legacy = false;
            state.menu = None;
            state.task_list = true;
            state.task_index = state
                .works
                .iter()
                .position(|work| work["work"]["id"].as_str() == state.context.work_id.as_deref())
                .unwrap_or(0);
            state.view_mut().details_open = false;
            state.focus = Focus::Composer;
            state
                .demo
                .as_mut()
                .context("Missing demo presentation")?
                .follow_selection = true;
            return Ok(InputAction::None);
        }
        KeyCode::F(4) | KeyCode::F(6) => {
            if state.menu.is_some() {
                close_menu(state);
            } else {
                let menu = menu(state)?;
                state
                    .demo
                    .as_mut()
                    .context("Missing demo presentation")?
                    .menu_return_focus = Some(state.focus);
                state.menu = Some(menu);
                state.focus = Focus::Actions;
            }
            return Ok(InputAction::None);
        }
        _ => {}
    }
    if let Some(menu) = state.menu.as_mut() {
        match code {
            KeyCode::Esc => close_menu(state),
            KeyCode::Up => menu.selected = menu.selected.saturating_sub(1),
            KeyCode::Down => {
                menu.selected = (menu.selected + 1).min(menu.items.len().saturating_sub(1))
            }
            KeyCode::Enter => {
                if let Some((_, choice)) = menu.items.get(menu.selected) {
                    let choice = choice.clone();
                    if !matches!(choice, workflow::Choice::DemoTokenCap) {
                        close_menu(state);
                    }
                    match choice {
                        workflow::Choice::DemoInput(input) => {
                            return Ok(InputAction::Submit(input))
                        }
                        workflow::Choice::DemoDetails => {
                            state.view_mut().details_open = true;
                            state.focus = Focus::Details;
                        }
                        workflow::Choice::DemoTokenCap => {
                            state.menu = Some(cap_menu(scenario(state)?)?);
                        }
                        workflow::Choice::DemoCancel => {}
                        workflow::Choice::Overview => {
                            return key(state, KeyCode::F(2), KeyModifiers::NONE)
                        }
                        _ => bail!("Unexpected production choice in demo menu"),
                    }
                }
            }
            _ => {}
        }
        return Ok(InputAction::None);
    }
    if code == KeyCode::F(5) {
        let view = state.view_mut();
        view.details_open = !view.details_open;
        state.focus = if view.details_open {
            Focus::Details
        } else {
            Focus::Composer
        };
        return Ok(InputAction::None);
    }
    if code == KeyCode::F(7) && state.focus == Focus::Details {
        let demo = state.demo.as_mut().context("Missing demo presentation")?;
        demo.expanded_details = !demo.expanded_details;
        state.view_mut().details_scroll = 0;
        return Ok(InputAction::None);
    }
    if state.focus == Focus::Details {
        let view = state.view_mut();
        match code {
            KeyCode::Esc => {
                view.details_open = false;
                state.focus = Focus::Composer;
            }
            KeyCode::Up | KeyCode::PageUp => {
                view.details_scroll =
                    view.details_scroll
                        .saturating_sub(if code == KeyCode::Up { 1 } else { 10 })
            }
            KeyCode::Down | KeyCode::PageDown => {
                view.details_scroll = view
                    .details_scroll
                    .saturating_add(if code == KeyCode::Down { 1 } else { 10 })
            }
            _ => {}
        }
        return Ok(InputAction::None);
    }
    if state.task_list {
        match code {
            KeyCode::Esc => state.task_list = false,
            KeyCode::Up | KeyCode::Down => {
                state.task_index = if code == KeyCode::Up {
                    state.task_index.saturating_sub(1)
                } else {
                    (state.task_index + 1).min(state.works.len().saturating_sub(1))
                };
                state
                    .demo
                    .as_mut()
                    .context("Missing demo presentation")?
                    .follow_selection = true;
            }
            KeyCode::PageUp | KeyCode::PageDown => {
                let demo = state.demo.as_mut().context("Missing demo presentation")?;
                demo.overview_scroll = if code == KeyCode::PageUp {
                    demo.overview_scroll.saturating_sub(10)
                } else {
                    demo.overview_scroll.saturating_add(10)
                };
                demo.follow_selection = false;
            }
            KeyCode::Enter => {
                let id = state
                    .works
                    .get(state.task_index)
                    .and_then(|work| work["work"]["id"].as_str())
                    .context("Select a demo Work")?
                    .to_owned();
                open_work(state, &id)?;
            }
            _ => {}
        }
        return Ok(InputAction::None);
    }
    match code {
        KeyCode::Tab => {
            let next = scenario(state)?.next_input();
            state.editor_view_mut().replace_draft(next.into());
        }
        KeyCode::Enter if modifiers.contains(KeyModifiers::SHIFT) => {
            state.editor_view_mut().insert("\n")
        }
        KeyCode::Enter => {
            return Ok(InputAction::Submit(
                state.editor_view_mut().draft.trim().into(),
            ))
        }
        KeyCode::Char(character)
            if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            state
                .editor_view_mut()
                .insert(character.encode_utf8(&mut [0; 4]))
        }
        KeyCode::PageUp | KeyCode::PageDown => {
            let view = state.reading_view_mut();
            view.follow = false;
            view.scroll = if code == KeyCode::PageUp {
                view.scroll.saturating_sub(10)
            } else {
                view.scroll.saturating_add(10)
            };
        }
        KeyCode::End if modifiers.contains(KeyModifiers::CONTROL) => {
            state.reading_view_mut().follow = true
        }
        KeyCode::Esc => state.editor_view_mut().editor.anchor = None,
        KeyCode::F(_) => bail!("This production control is unavailable in the scripted demo"),
        _ => {
            let view = state.editor_view_mut();
            view.editor.key(&mut view.draft, code, modifiers);
        }
    }
    Ok(InputAction::None)
}

fn candidate(state: &mut State, input: &str) -> Result<Option<Scenario>> {
    match input.trim() {
        "/help" => {
            state.notice = "F1: global conversation | F2: Work Overview | F4/F6: choices | \
                F5: details | Tab: next story input | /reset | /quit"
                .into();
            state.notice_kind = NoticeKind::Attention;
            Ok(None)
        }
        "/reset" => {
            state
                .demo
                .as_mut()
                .context("Missing demo presentation")?
                .reset_pending = true;
            state.notice = t!("agent_center.demo_reset").into_owned();
            state.notice_kind = NoticeKind::Attention;
            Ok(None)
        }
        "/reset confirm" => {
            anyhow::ensure!(
                state.demo.as_ref().is_some_and(|demo| demo.reset_pending),
                "Use /reset before confirming"
            );
            let mut fresh = Scenario::new();
            fresh.enable_clock()?;
            fresh.require_cap_setup()?;
            Ok(Some(fresh))
        }
        input => {
            let mut next = scenario(state)?.clone();
            next.apply(input)?;
            if scenario(state)?.work_graph.is_some() {
                let command = crate::agent_center::demo::scenario::command(input)?;
                anyhow::ensure!(
                    (command != crate::agent_center::demo::scenario::work_graph::EXPLAIN)
                        == state.context.global_conversation,
                    "Wrong conversation: F1 for Main agent, F4 for Compatibility agent"
                );
            }
            if scenario(state)?.is_work_model() {
                let command = crate::agent_center::demo::scenario::command(input)?;
                if let Some(main) = work_model::main_input(command) {
                    anyhow::ensure!(
                        main == state.context.global_conversation,
                        "Wrong conversation: use F1 for Main agent or F4 for this Work agent"
                    );
                }
            }
            Ok(Some(next))
        }
    }
}

fn committed(state: &mut State, saved: Scenario, input: &str) -> Result<()> {
    if input.trim() == "/reset confirm" {
        *state = new_state(saved)?;
        return Ok(());
    }
    if saved.release_journey.is_some() {
        state.editor_view_mut().clear_draft();
        synchronize(state, saved)?;
        home(state);
        state.task_list = input == crate::agent_center::demo::scenario::release_journey::OVERVIEW;
        state.notice.clear();
        state.chat.follow = true;
        return Ok(());
    }
    if saved.work_board.is_some() {
        if state.form.is_none() && state.editor_view_mut().draft.trim() == input.trim() {
            state.editor_view_mut().clear_draft();
        }
        synchronize(state, saved)?;
        state.form = None;
        state.notice.clear();
        state.notice_kind = NoticeKind::Info;
        let saved = scenario(state)?;
        state.task_index = saved
            .works
            .iter()
            .position(|work| work.id == saved.selected_work_id)
            .context("Selected board Work is missing")?;
        work_board::show(state);
        return Ok(());
    }
    if state.editor_view_mut().draft.trim() == input.trim() {
        state.editor_view_mut().clear_draft();
    }
    let old_revision = scenario(state)?.revision;
    let stay_main = (scenario(state)?.is_work_model() || saved.work_graph.is_some())
        && state.context.global_conversation;
    synchronize(state, saved)?;
    state.menu = None;
    state.notice.clear();
    state.notice_kind = NoticeKind::Info;
    state
        .demo
        .as_mut()
        .context("Missing demo presentation")?
        .reset_pending = false;
    if scenario(state)?.revision != old_revision {
        let id = scenario(state)?.selected_work_id.clone();
        if stay_main {
            home(state);
        } else {
            open_work(state, &id)?;
        }
        state.editor_view_mut().follow = true;
        if scenario(state)?.scene == 8 {
            state.task_list = true;
            state
                .demo
                .as_mut()
                .context("Missing demo presentation")?
                .follow_selection = true;
        }
    }
    Ok(())
}

async fn persist(store: Store, saved: Scenario) -> Result<(Store, Scenario, Result<()>)> {
    tokio::task::spawn_blocking(move || {
        let result = store.save(&saved).context("Saving isolated demo state");
        (store, saved, result)
    })
    .await
    .context("Joining demo persistence task")
}

pub(super) async fn run(mut store: Store, saved: Scenario) -> Result<()> {
    use crate::agent_center::demo::scenario::release_journey::{
        DiagnosisStage, DiagnosisUpdate, Receipt as WorkReceipt, Update as WorkUpdate,
    };
    let mut saved = saved;
    let check_root = store.root().to_path_buf();
    if let Some(journey) = saved
        .release_journey
        .as_ref()
        .filter(|journey| journey.active())
    {
        saved.receive_release(
            crate::agent_center::demo::scenario::release_journey::Receipt {
                attempt: journey.attempt,
                update: crate::agent_center::demo::scenario::release_journey::Update::Failed {
                    message:
                        "The previous UI closed during execution. Reopening does not replay checks."
                            .into(),
                },
            },
        )?;
        let (returned_store, interrupted, result) = persist(store, saved).await?;
        store = returned_store;
        result?;
        saved = interrupted;
    }
    if saved
        .release_journey
        .as_ref()
        .and_then(|j| j.diagnosis.as_ref())
        .is_some_and(|d| d.active())
    {
        saved.receive_release(WorkReceipt {
            attempt: 1,
            update: WorkUpdate::Diagnosis {
                update: DiagnosisUpdate::Failed {
                    message:
                        "The previous UI closed during diagnosis. Reopening does not replay it."
                            .into(),
                },
            },
        })?;
        let (returned_store, interrupted, result) = persist(store, saved).await?;
        store = returned_store;
        result?;
        saved = interrupted;
    }
    if let Some(handoff) = saved
        .work_graph
        .as_ref()
        .and_then(|graph| graph.handoff.as_ref())
        .filter(|handoff| !handoff.stage.terminal())
        .cloned()
    {
        saved.receive_graph_receipt(handoff.receipt(
            crate::agent_center::demo::scenario::work_graph::WorkerUpdate::Failed {
                message: "The previous UI closed before the check completed. No new result is assumed; the decision is retained.".into(),
            },
        ))?;
        let (returned_store, interrupted, result) = persist(store, saved).await?;
        store = returned_store;
        result?;
        saved = interrupted;
    }
    if saved.clock.is_none() {
        let fresh = saved.revision == 0;
        saved.enable_clock()?;
        if fresh {
            saved.require_cap_setup()?;
        }
        let (returned_store, enabled, result) = persist(store, saved).await?;
        store = returned_store;
        result?;
        saved = enabled;
    }
    let mut state = new_state(saved)?;
    enable_raw_mode()?;
    let _guard = TerminalGuard;
    #[cfg(windows)]
    let mut input = input::ConsoleInput::new()?;
    #[cfg(not(windows))]
    let mut input = crossterm::event::EventStream::new();
    execute!(
        std::io::stdout(),
        EnterAlternateScreen,
        EnableBracketedPaste
    )?;
    let mut terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    let period = std::time::Duration::from_secs(1);
    let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_tick = tokio::time::Instant::now();
    let mut check_worker: Option<crate::agent_center::demo::check_worker::Worker> = None;
    let mut release_worker: Option<crate::agent_center::demo::release_worker::Worker> = None;
    let mut diagnosis_worker: Option<crate::agent_center::demo::release_worker::Worker> = None;
    loop {
        if diagnosis_worker.is_none()
            && scenario(&state)?
                .release_journey
                .as_ref()
                .and_then(|j| j.diagnosis.as_ref())
                .is_some_and(|d| d.stage == DiagnosisStage::Assigned)
        {
            diagnosis_worker = Some(
                crate::agent_center::demo::release_worker::Worker::start_diagnosis(
                    check_root.clone(),
                ),
            );
        }
        if release_worker.is_none() {
            if let Some(journey) = scenario(&state)?
                .release_journey
                .as_ref()
                .filter(|journey| {
                    matches!(journey.stage,
                    crate::agent_center::demo::scenario::release_journey::Stage::Assigned |
                    crate::agent_center::demo::scenario::release_journey::Stage::RepairAssigned)
                })
            {
                release_worker = Some(if journey.parallel_story == Some(true) {
                    crate::agent_center::demo::release_worker::Worker::start_parallel(
                        check_root.clone(),
                        journey.attempt,
                    )
                } else {
                    crate::agent_center::demo::release_worker::Worker::start(
                        check_root.clone(),
                        journey.attempt,
                    )
                });
            }
        }
        if check_worker.is_none() {
            if let Some(handoff) = scenario(&state)?
                .work_graph
                .as_ref()
                .and_then(|graph| graph.handoff.as_ref())
                .filter(|handoff| {
                    handoff.stage
                        == crate::agent_center::demo::scenario::work_graph::HandoffStage::Recorded
                })
            {
                check_worker = Some(crate::agent_center::demo::check_worker::Worker::start(
                    check_root.clone(),
                    handoff.clone(),
                ));
            }
        }
        terminal.draw(|frame| renderer::render(frame, &mut state))?;
        let event = tokio::select! {
            event = input.next() => event.context("Demo console input closed")??,
            receipt = async {
                match diagnosis_worker.as_mut() {
                    Some(worker) => worker.next().await,
                    None => std::future::pending().await,
                }
            } => {
                let mut next = scenario(&state)?.clone();
                let receipt = match receipt {
                    Ok(receipt) => receipt,
                    Err(error) => WorkReceipt { attempt: 1, update: WorkUpdate::Diagnosis {
                        update: DiagnosisUpdate::Failed { message: format!("{error:#}").chars().take(1000).collect() },
                    }},
                };
                anyhow::ensure!(matches!(receipt.update, WorkUpdate::Diagnosis { .. }), "Diagnosis worker returned another Work's result");
                next.receive_release(receipt)?;
                let finished = next.release_journey.as_ref().and_then(|j| j.diagnosis.as_ref()).is_some_and(|d| !d.active());
                let (returned_store, saved, result) = persist(store, next).await?;
                store = returned_store;
                result?;
                synchronize(&mut state, saved)?;
                if finished {
                    if let Some(worker) = diagnosis_worker.take() { worker.finish().await?; }
                }
                continue;
            }
            receipt = async {
                match release_worker.as_mut() {
                    Some(worker) => worker.next().await,
                    None => std::future::pending().await,
                }
            } => {
                let mut next = scenario(&state)?.clone();
                let receipt = match receipt {
                    Ok(receipt) => receipt,
                    Err(error) => crate::agent_center::demo::scenario::release_journey::Receipt {
                        attempt: next.release_journey.as_ref().context("Missing release journey")?.attempt,
                        update: crate::agent_center::demo::scenario::release_journey::Update::Failed {
                            message: format!("{error:#}").chars().take(1000).collect(),
                        },
                    },
                };
                anyhow::ensure!(!matches!(receipt.update, WorkUpdate::Diagnosis { .. }), "Release worker returned another Work's result");
                next.receive_release(receipt)?;
                let finished = next.release_journey.as_ref().is_some_and(|journey| !journey.active());
                let (returned_store, saved, result) = persist(store, next).await?;
                store = returned_store;
                result?;
                synchronize(&mut state, saved)?;
                if finished {
                    if let Some(worker) = release_worker.take() {
                        worker.finish().await?;
                    }
                }
                continue;
            }
            receipt = async {
                match check_worker.as_mut() {
                    Some(worker) => worker.next().await,
                    None => std::future::pending().await,
                }
            } => {
                let mut next = scenario(&state)?.clone();
                let result = receipt.and_then(|receipt| next.receive_graph_receipt(receipt));
                let result = match result {
                    Ok(()) => {
                        let terminal = next.work_graph.as_ref()
                            .and_then(|graph| graph.handoff.as_ref())
                            .is_some_and(|handoff| handoff.stage.terminal());
                        let (returned_store, saved, result) = persist(store, next).await?;
                        store = returned_store;
                        result?;
                        if terminal {
                            if let Some(worker) = check_worker.take() {
                                worker.finish().await?;
                            }
                        }
                        synchronize(&mut state, saved)
                    }
                    Err(error) => {
                        drop(check_worker.take());
                        // Preserve an explicit terminal failure rather than resubmitting a recorded job.
                        if let Some(handoff) = next.work_graph.as_ref().and_then(|graph| graph.handoff.as_ref())
                            .filter(|handoff| !handoff.stage.terminal()).cloned()
                        {
                            let message: String = format!("{error:#}").chars().take(1000).collect();
                            next.receive_graph_receipt(handoff.receipt(
                                crate::agent_center::demo::scenario::work_graph::WorkerUpdate::Failed { message },
                            ))?;
                            let (returned_store, saved, result) = persist(store, next).await?;
                            store = returned_store;
                            result?;
                            synchronize(&mut state, saved)?;
                        }
                        Err(error)
                    }
                };
                if let Err(error) = result {
                    state.notice = format!("{error:#}");
                    state.notice_kind = NoticeKind::Error;
                }
                continue;
            }
            _ = tick.tick() => {
                let now = tokio::time::Instant::now();
                let elapsed = now.duration_since(last_tick);
                last_tick = now;
                let mut saved = scenario(&state)?.clone();
                let result = saved.advance(elapsed);
                let result = match result {
                    Ok(()) if saved != *scenario(&state)? => {
                        let (returned_store, saved, result) = persist(store, saved).await?;
                        store = returned_store;
                        result.and_then(|()| synchronize(&mut state, saved))
                    }
                    other => other,
                };
                if let Err(error) = result {
                    state.notice = format!("{error:#}");
                    state.notice_kind = NoticeKind::Error;
                }
                continue;
            }
        };
        let action = match event {
            Event::Key(event) if event.kind != KeyEventKind::Release => {
                key(&mut state, event.code, event.modifiers)
            }
            Event::Paste(text) => {
                if !state.demo.as_ref().is_some_and(|demo| demo.legacy)
                    && state.menu.is_none()
                    && state.focus == Focus::Composer
                {
                    handle_paste(&mut state, &text);
                }
                Ok(InputAction::None)
            }
            _ => Ok(InputAction::None),
        };
        let result = match action {
            Ok(InputAction::Quit) => break,
            Ok(InputAction::Submit(input)) if input.eq_ignore_ascii_case("/quit") => break,
            Ok(InputAction::Submit(input)) => match candidate(&mut state, &input) {
                Ok(Some(saved)) => {
                    let (returned_store, saved, result) = persist(store, saved).await?;
                    store = returned_store;
                    if result.is_ok() && input.trim() == "/reset confirm" {
                        drop(check_worker.take());
                        drop(release_worker.take());
                        drop(diagnosis_worker.take());
                    }
                    result.and_then(|()| committed(&mut state, saved, &input))
                }
                Ok(None) => {
                    state.editor_view_mut().clear_draft();
                    Ok(())
                }

                Err(error) => Err(error),
            },
            Ok(InputAction::None) => Ok(()),
            Err(error) => Err(error),
        };
        if let Err(error) = result {
            state.notice = format!("{error:#}");
            state.notice_kind = NoticeKind::Error;
        }
    }
    terminal.show_cursor()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    struct TestDirectory(std::path::PathBuf);
    impl Drop for TestDirectory {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn natural_state() -> State {
        let mut saved = Scenario::new();
        saved.enable_clock().unwrap();
        new_state(saved).unwrap()
    }

    fn cap_setup_state() -> State {
        let mut saved = Scenario::new();
        saved.enable_clock().unwrap();
        saved.require_cap_setup().unwrap();
        let mut state = new_state(saved).unwrap();
        press(&mut state, KeyCode::F(1));
        for input in [
            "Continue fixing issue #4821",
            "Should we migrate to API v2?",
            "Create related work",
        ] {
            submit(&mut state, input);
        }
        advance(&mut state, 10_000);
        submit(&mut state, "B");
        state
    }

    #[test]
    fn parallel_work_journey_overview_counts_goals_not_internal_execution_tasks() {
        use crate::agent_center::demo::scenario::release_journey::*;
        let mut state = natural_state();
        press(&mut state, KeyCode::F(1));
        submit(&mut state, OPEN_PARALLEL);
        press(&mut state, KeyCode::F(2));
        assert!(!state.task_list);
        for input in [GOAL, CONFIRM, START, DIAGNOSE] {
            submit(&mut state, input);
        }
        let draft = draw(&mut state, 150, 42);
        assert!(draft.contains("ALL WORK / 1 GOALS"), "{draft}");
        assert!(draft.contains("NEW WORK DRAFT"), "{draft}");
        let release = scenario(&state).unwrap().works.clone();
        submit(&mut state, START_DIAGNOSIS);
        assert_eq!(scenario(&state).unwrap().works[..3], release);
        let screen = draw(&mut state, 150, 42);
        for text in [
            "ALL WORK / 2 GOALS",
            "WORK A / RELEASE",
            "WORK B / DIAGNOSIS",
            "Explain the startup warning",
            "Own execution context. Release task unchanged.",
        ] {
            assert!(screen.contains(text), "{text}\n{screen}");
        }
        assert!(!screen.contains("scripted-startup-diagnosis-session"));
        let saved = scenario(&state).unwrap().clone();
        press(&mut state, KeyCode::F(2));
        assert!(draw(&mut state, 150, 42).contains("2 INDEPENDENT GOALS"));
        press(&mut state, KeyCode::F(1));
        assert_eq!(saved, *scenario(&state).unwrap());
        assert_eq!(draw(&mut state, 150, 42), screen);
        submit(&mut state, PROGRESS);
        assert_eq!(scenario(&state).unwrap().works, saved.works);
        assert_eq!(
            scenario(&state).unwrap().release_journey,
            saved.release_journey
        );
        let progress = draw(&mut state, 150, 42);
        for text in [
            PROGRESS,
            "Here is where your Work stands:",
            "Release checks:",
            "This question stays with coordination",
            "execution contexts are unchanged.",
        ] {
            assert!(progress.contains(text), "{text}\n{progress}");
        }
        let restored = scenario(&state).unwrap().clone();
        let mut reopened = new_state(restored).unwrap();
        assert_eq!(draw(&mut reopened, 150, 42), progress);
    }

    #[test]
    fn release_journey_native_conversation_keeps_one_goal_and_reopens_without_launching_sessions() {
        use crate::agent_center::demo::scenario::release_journey::*;
        let mut state = natural_state();
        press(&mut state, KeyCode::F(1));
        submit(&mut state, OPEN);
        assert!(draw(&mut state, 150, 48).contains("No Work created. No sessions started."));
        for input in [GOAL, CONFIRM] {
            state.chat.replace_draft(input.into());
            let InputAction::Submit(request) = press(&mut state, KeyCode::Enter) else {
                panic!("Missing request");
            };
            submit(&mut state, &request);
        }
        assert!(scenario(&state).unwrap().works.is_empty());
        let draft = draw(&mut state, 150, 48);
        assert!(
            draft.contains("DRAFT / AWAITING YOUR CONFIRMATION"),
            "{draft}"
        );
        assert!(draft.contains("two execution tasks"), "{draft}");
        submit(&mut state, START);
        let running = draw(&mut state, 150, 48);
        assert!(running.contains("Implementation agent"), "{running}");
        assert!(running.contains("Verification agent"), "{running}");
        assert!(!running.contains("scripted-release-"));
        assert!(state.context.global_conversation);
        let saved = scenario(&state).unwrap().clone();
        press(&mut state, KeyCode::F(2));
        assert!(draw(&mut state, 150, 48).contains("ALL WORK / 1 RELEASE GOAL"));
        assert_eq!(
            *scenario(&state).unwrap(),
            saved,
            "Navigation must not execute"
        );
        press(&mut state, KeyCode::F(1));
        assert!(draw(&mut state, 80, 24).contains("enlarge"));
        let root = TestDirectory(
            std::env::temp_dir().join(format!("wta-release-ui-{}", uuid::Uuid::new_v4())),
        );
        let (store, _) = Store::open(&root.0, false).unwrap();
        store.save(&saved).unwrap();
        drop(store);
        let (_store, reopened) = Store::open(&root.0, false).unwrap();
        let mut restored = new_state(reopened).unwrap();
        assert_eq!(draw(&mut restored, 150, 48), running);
    }

    #[test]
    fn work_board_shows_all_goals_creates_a_real_brief_and_reopens_from_sqlite() {
        let mut state = natural_state();
        press(&mut state, KeyCode::F(1));
        submit(&mut state, "Open Work board");
        let screen = draw(&mut state, 150, 42);
        for text in [
            "WORK BOARD",
            "3 WORKS",
            "1 BLOCKED",
            "1 RUNNING",
            "1 DONE",
            "Ship fix #4821 without regressions",
            "Decide whether API v2 is worth migrating",
            "Publish the onboarding guide",
            "Legacy quoting check failed.",
            "+ NEW WORK",
        ] {
            assert!(screen.contains(text), "{text}\n{screen}");
        }
        assert!(!screen.contains("executor-1"));
        assert!(!screen.contains("checkpoint"));
        press(&mut state, KeyCode::F(1));
        state
            .chat
            .replace_draft("retain my management draft".into());
        press(&mut state, KeyCode::F(2));
        press(&mut state, KeyCode::Char('n'));
        assert!(draw(&mut state, 150, 42).contains("NEW WORK / DEFINE AN OUTCOME"));
        handle_paste(&mut state, "Explain the startup warning");
        press(&mut state, KeyCode::Tab);
        handle_paste(&mut state, "Record the root cause and a safe fix");
        let InputAction::Submit(input) = press(&mut state, KeyCode::Enter) else {
            panic!("missing create action")
        };
        submit(&mut state, &input);
        assert_eq!(state.chat.draft, "retain my management draft");
        assert!(state.form.is_none());
        assert!(state.task_list);
        assert_eq!(state.task_index, 3);
        let created = scenario(&state).unwrap().clone();
        assert_eq!(created.works[3].status, "Planned");
        assert!(created.works[3].executor_session_id.is_empty());
        let screen = draw(&mut state, 150, 42);
        for text in [
            "4 WORKS",
            "1 PLANNED",
            "Explain the startup warning",
            "0 / 1 completion criteria met",
        ] {
            assert!(screen.contains(text), "{text}\n{screen}");
        }
        press(&mut state, KeyCode::Enter);
        assert!(!state.task_list);
        assert!(draw(&mut state, 150, 42).contains("Record the root cause and a safe fix"));
        assert_eq!(scenario(&state).unwrap(), &created, "opening is read-only");
        press(&mut state, KeyCode::F(2));
        let InputAction::Submit(input) = press(&mut state, KeyCode::Char('s')) else {
            panic!("missing start action")
        };
        submit(&mut state, &input);
        assert_eq!(scenario(&state).unwrap().works[3].status, "Running");
        assert_eq!(&scenario(&state).unwrap().works[..3], &created.works[..3]);
        let screen = draw(&mut state, 150, 42);
        assert!(screen.contains("2 RUNNING"));
        let root = TestDirectory(
            std::env::temp_dir().join(format!("work-board-{}", uuid::Uuid::new_v4())),
        );
        let (store, _) = Store::open(&root.0, false).unwrap();
        store.save(scenario(&state).unwrap()).unwrap();
        drop(store);
        let (store, saved) = Store::open(&root.0, false).unwrap();
        let mut reopened = new_state(saved).unwrap();
        assert!(reopened.task_list);
        assert_eq!(scenario(&reopened).unwrap(), scenario(&state).unwrap());
        assert!(draw(&mut reopened, 150, 42).contains("Explain the startup warning"));
        drop(store);
    }

    #[test]
    fn work_board_invalid_create_cancel_navigation_and_small_windows_preserve_work() {
        let mut state = natural_state();
        press(&mut state, KeyCode::F(1));
        submit(&mut state, "Open Work board");
        let before = scenario(&state).unwrap().clone();
        press(&mut state, KeyCode::Char('n'));
        handle_paste(&mut state, "Do not save an incomplete goal");
        press(&mut state, KeyCode::Tab);
        let InputAction::Submit(input) = press(&mut state, KeyCode::Enter) else {
            panic!("missing create action")
        };
        assert!(candidate(&mut state, &input).is_err());
        assert!(state.form.is_some());
        assert_eq!(scenario(&state).unwrap(), &before);
        press(&mut state, KeyCode::Esc);
        assert!(state.form.is_none());
        for (width, height) in [(150, 42), (100, 34), (76, 28), (40, 12), (1, 1)] {
            let _ = draw(&mut state, width, height);
        }
        for _ in 0..8 {
            press(&mut state, KeyCode::Down);
        }
        assert_eq!(state.task_index, 2);
        press(&mut state, KeyCode::Enter);
        let InputAction::Submit(input) = press(&mut state, KeyCode::Char('s')) else {
            panic!("missing start action")
        };
        assert!(
            candidate(&mut state, &input).is_err(),
            "Done must not be restarted"
        );
        assert_eq!(scenario(&state).unwrap(), &before);
        press(&mut state, KeyCode::F(2));
        assert!(state.task_list);
    }

    #[test]
    fn work_graph_management_preserves_runtime_context_and_survives_sqlite_reopen() {
        use crate::agent_center::demo::scenario::work_graph::*;
        let mut state = natural_state();
        press(&mut state, KeyCode::F(1));
        submit(&mut state, "Open Work graph");
        assert!(state.context.global_conversation);
        press(&mut state, KeyCode::Tab);
        assert_eq!(state.editor_view_mut().draft, PLAN);
        assert!(
            matches!(press(&mut state, KeyCode::Enter), InputAction::Submit(input) if input == PLAN)
        );
        submit(&mut state, PLAN);
        submit(&mut state, START);
        advance(&mut state, 9_000);
        let runtime_chat = state.views[&Some(COMPATIBILITY.into())].messages.clone();
        let binding = scenario(&state).unwrap().works[2]
            .executor_session_id
            .clone();
        for input in [RESUME, PROGRESS, DECISION, EVIDENCE, RELATED, AUDIT] {
            submit(&mut state, input);
        }
        let conversation = serde_json::to_string(&state.chat.messages).unwrap();
        assert!(conversation.contains("unit-test pass does not verify legacy compatibility"));
        assert!(conversation.contains("expected double quotes; received single quotes"));
        assert_eq!(
            runtime_chat,
            state.views[&Some(COMPATIBILITY.into())].messages
        );
        assert_eq!(
            scenario(&state).unwrap().works[2].executor_session_id,
            binding
        );
        advance(&mut state, 15_000);
        let screen = draw(&mut state, 160, 45);
        for expected in [
            "REQUIRED",
            "RELATED / non-blocking",
            "Paused at cap",
            "5000 / 5000",
            "Running | checkpoint 8",
            "Assignments: 1 | Direct chats: 0",
            "To: Main agent",
            "To: Compatibility agent",
            "NOT provider quota",
        ] {
            assert!(screen.contains(expected), "{expected}\n{screen}");
        }
        let before = scenario(&state).unwrap().clone();
        assert!(candidate(&mut state, EXPLAIN).is_err());
        assert_eq!(&before, scenario(&state).unwrap());
        state.editor_view_mut().replace_draft("main draft".into());
        press(&mut state, KeyCode::F(4));
        assert!(candidate(&mut state, PROGRESS).is_err());
        submit(&mut state, EXPLAIN);
        state.editor_view_mut().replace_draft("worker draft".into());
        press(&mut state, KeyCode::F(1));
        assert_eq!(state.editor_view_mut().draft, "main draft");
        press(&mut state, KeyCode::F(4));
        assert_eq!(state.editor_view_mut().draft, "worker draft");
        let root = TestDirectory(
            std::env::temp_dir().join(format!("work-graph-{}", uuid::Uuid::new_v4())),
        );
        let (store, _) = Store::open(&root.0, false).unwrap();
        store.save(scenario(&state).unwrap()).unwrap();
        drop(store);
        let (store, saved) = Store::open(&root.0, false).unwrap();
        let mut reopened = new_state(saved).unwrap();
        assert_eq!(reopened.chat.messages, state.chat.messages);
        assert_eq!(
            reopened.views[&Some(COMPATIBILITY.into())].messages,
            state.views[&Some(COMPATIBILITY.into())].messages
        );
        assert_eq!(scenario(&reopened).unwrap(), scenario(&state).unwrap());
        for (width, height) in [(160, 45), (110, 30), (80, 24), (1, 1)] {
            let _ = draw(&mut reopened, width, height);
        }
        drop(store);
    }

    #[test]
    fn graph_receipts_reach_both_chats_preserve_drafts_and_reopen_without_dispatch() {
        use crate::agent_center::demo::scenario::work_graph::*;
        let mut state = natural_state();
        press(&mut state, KeyCode::F(1));
        for input in ["Open Work graph", PLAN, START] {
            submit(&mut state, input);
        }
        advance(&mut state, 9_000);
        submit(&mut state, HANDOFF);
        state.editor_view_mut().replace_draft("main draft".into());
        press(&mut state, KeyCode::F(4));
        state.editor_view_mut().replace_draft("worker draft".into());
        let request = scenario(&state)
            .unwrap()
            .graph()
            .unwrap()
            .handoff
            .clone()
            .unwrap();
        for (update, expected) in [
            (WorkerUpdate::Delivered { pid: 42 }, "DELIVERED"),
            (WorkerUpdate::Acknowledged, "ACKNOWLEDGED"),
            (WorkerUpdate::CheckStarted, "CHECK STARTED"),
            (
                WorkerUpdate::CheckCompleted {
                    exit_code: 1,
                    stdout: "TAP version 13\n# tests 1\n# pass 0\n# fail 1\n".into(),
                    stderr: String::new(),
                },
                "NEW EVIDENCE",
            ),
        ] {
            let mut saved = scenario(&state).unwrap().clone();
            saved
                .receive_graph_receipt(request.receipt(update))
                .unwrap();
            synchronize(&mut state, saved).unwrap();
            assert_eq!(state.editor_view_mut().draft, "worker draft");
            for messages in [
                &state.chat.messages,
                &state.views[&Some(COMPATIBILITY.into())].messages,
            ] {
                assert!(serde_json::to_string(messages).unwrap().contains(expected));
            }
            let screen = draw(&mut state, 160, 45);
            assert!(screen.contains("Local compatibility worker"), "{screen}");
            assert!(screen.contains("Decision handoff:"), "{screen}");
            assert!(screen.contains(expected), "{screen}");
        }
        press(&mut state, KeyCode::F(1));
        assert_eq!(state.editor_view_mut().draft, "main draft");
        let root = TestDirectory(
            std::env::temp_dir().join(format!("handoff-ui-{}", uuid::Uuid::new_v4())),
        );
        let (store, _) = Store::open(&root.0, false).unwrap();
        store.save(scenario(&state).unwrap()).unwrap();
        drop(store);
        let (store, saved) = Store::open(&root.0, false).unwrap();
        assert!(saved
            .graph()
            .unwrap()
            .handoff
            .as_ref()
            .unwrap()
            .stage
            .terminal());
        let mut reopened = new_state(saved).unwrap();
        assert_eq!(reopened.chat.messages, state.chat.messages);
        assert_eq!(
            reopened.views[&Some(COMPATIBILITY.into())].messages,
            state.views[&Some(COMPATIBILITY.into())].messages
        );
        assert!(candidate(&mut reopened, HANDOFF).is_err());
        assert_eq!(scenario(&reopened).unwrap().works[2].evidence.len(), 2);
        assert!(draw(&mut reopened, 160, 45).contains("Goal acceptance: PENDING"));
        drop(store);
    }

    #[test]
    fn merge_story_routes_readiness_and_approval_to_main_then_explains_failure_in_work_chat() {
        let mut state = natural_state();
        press(&mut state, KeyCode::F(1));
        submit(&mut state, "Open merge story");
        assert!(candidate(&mut state, "Is yesterday's fix ready to merge?").is_err());
        press(&mut state, KeyCode::F(1));
        submit(&mut state, "Is yesterday's fix ready to merge?");
        let screen = draw(&mut state, 140, 45);
        assert!(screen.contains("Not ready to merge"), "{screen}");
        assert!(screen.contains("Merge: BLOCKED"));
        assert!(screen.contains("Awaiting your approval"));
        assert!(scenario(&state)
            .unwrap()
            .clock
            .as_ref()
            .unwrap()
            .budget_due_ms
            .is_none());
        submit(&mut state, "Proceed. Keep the legacy API unchanged.");
        let evidence = scenario(&state).unwrap().works[0].evidence.clone();
        assert!(state.context.global_conversation);
        press(&mut state, KeyCode::F(4));
        submit(&mut state, "Why did compatibility fail?");
        let screen = draw(&mut state, 140, 45);
        for expected in [
            "double-quoted",
            "implementation returns single",
            "Running",
            "Merge: BLOCKED",
            "Instruction saved",
        ] {
            assert!(screen.contains(expected), "{expected}\n{screen}");
        }
        assert!(!serde_json::to_string(&state.chat.messages)
            .unwrap()
            .contains("The legacy adapter requires"));
        let reopened = new_state(scenario(&state).unwrap().clone()).unwrap();
        assert_eq!(
            reopened.views[&Some(FIX.into())].messages,
            state.views[&Some(FIX.into())].messages
        );
        assert_eq!(reopened.chat.messages, state.chat.messages);
        assert_eq!(scenario(&reopened).unwrap().works[0].evidence, evidence);
    }

    #[test]
    fn work_model_definition_evidence_and_budget_share_one_native_work() {
        let mut state = natural_state();
        press(&mut state, KeyCode::F(1));
        submit(&mut state, "Open Work model");
        let initial = scenario(&state).unwrap().clone();
        let screen = draw(&mut state, 108, 36);
        for marker in [
            "Workspace",
            "Agent runtime",
            "Priority",
            "Acceptance criteria",
            "Token cap",
            "Fix Issue #4821",
        ] {
            assert!(screen.contains(marker), "{marker}\n{screen}");
        }
        assert!(!screen.contains("Global conversation"));
        press(&mut state, KeyCode::F(1));
        state
            .editor_view_mut()
            .insert("Continue fixing issue #4821");
        press(&mut state, KeyCode::F(4));
        press(&mut state, KeyCode::F(1));
        assert_eq!(state.editor_view_mut().draft, "Continue fixing issue #4821");
        assert_eq!(scenario(&state).unwrap(), &initial);
        submit(&mut state, "Continue fixing issue #4821");
        press(&mut state, KeyCode::F(5));
        let screen = draw(&mut state, 108, 36);
        assert!(screen.contains("Legacy shell compatibility passes"));
        assert!(screen.contains("exitCode: 1"));
        assert!(screen.contains("FAIL"));
        press(&mut state, KeyCode::F(6));
        let screen = draw(&mut state, 108, 36);
        assert!(screen.contains("> 20K tokens"));
        assert!(!screen.contains("exitCode:"));
        press(&mut state, KeyCode::Esc);
        assert!(draw(&mut state, 108, 36).contains("exitCode: 1"));
        press(&mut state, KeyCode::F(6));
        let InputAction::Submit(input) = press(&mut state, KeyCode::Enter) else {
            panic!("Expected cap confirmation")
        };
        submit(&mut state, &input);
        let InputAction::Submit(input) = press(&mut state, KeyCode::F(8)) else {
            panic!("Expected investigation")
        };
        submit(&mut state, &input);
        advance(&mut state, 12_000);
        let screen = draw(&mut state, 108, 36);
        assert!(screen.contains("20000 / 20000 tokens"));
        assert!(screen.contains("Paused at token cap"));
        assert!(screen.contains("Findings saved"));
        assert_eq!(scenario(&state).unwrap().works.len(), 1);
        assert_eq!(scenario(&state).unwrap().budget.work_id, FIX);
        let reopened = new_state(scenario(&state).unwrap().clone()).unwrap();
        assert!(!reopened.demo.as_ref().unwrap().legacy);
        assert!(reopened.demo.as_ref().unwrap().scenario.budget.paused);
    }

    #[test]
    fn work_model_conversations_route_independently_and_readonly_panels_preserve_drafts() {
        let mut state = natural_state();
        press(&mut state, KeyCode::F(1));
        submit(&mut state, "Open Work model");
        let saved = scenario(&state).unwrap().clone();
        for (width, height) in [(108, 36), (80, 24), (40, 14), (8, 4), (1, 1)] {
            let _ = draw(&mut state, width, height);
        }
        press(&mut state, KeyCode::Char('x'));
        assert_eq!(state.editor_view_mut().draft, "x");
        press(&mut state, KeyCode::F(5));
        press(&mut state, KeyCode::Char('y'));
        assert_eq!(state.editor_view_mut().draft, "x");
        press(&mut state, KeyCode::Esc);
        assert_eq!(scenario(&state).unwrap(), &saved);
        assert!(candidate(&mut state, "Continue fixing issue #4821").is_err());
        press(&mut state, KeyCode::F(1));
        submit(&mut state, "Continue fixing issue #4821");
        assert!(state.context.global_conversation);
        assert!(candidate(&mut state, "Keep the legacy API unchanged.").is_err());
        submit(&mut state, "Set token cap to 20K");
        submit(&mut state, "Investigate compatibility");
        press(&mut state, KeyCode::F(4));
        assert_eq!(state.editor_view_mut().draft, "x");
        state.editor_view_mut().clear_draft();
        submit(&mut state, "Keep the legacy API unchanged.");
        let screen = draw(&mut state, 140, 45);
        assert!(screen.contains("To: Fix #4821 / Work agent"));
        assert!(screen.contains("saved that instruction"));
        assert!(screen.contains("Running"));
        assert!(!serde_json::to_string(&state.chat.messages)
            .unwrap()
            .contains("saved that instruction"));
        advance(&mut state, 12_000);
        submit(&mut state, "Why are you paused?");
        let paused = scenario(&state).unwrap().clone();
        let mut reopened = new_state(paused.clone()).unwrap();
        assert_eq!(
            reopened.views[&Some(FIX.into())].messages,
            state.views[&Some(FIX.into())].messages
        );
        assert_eq!(reopened.chat.messages, state.chat.messages);
        assert!(draw(&mut reopened, 140, 45).contains("Paused at token cap"));
        press(&mut state, KeyCode::F(1));
        assert!(draw(&mut state, 140, 45).contains("needs your decision"));
        submit(&mut state, "Add 10K budget");
        assert!(state.context.global_conversation);
        assert_eq!(
            scenario(&state).unwrap().works[0].definition,
            paused.works[0].definition
        );
        submit(&mut state, "Review progress");
        let screen = draw(&mut state, 140, 45);
        assert!(screen.contains("Not accepted yet"), "{screen}");
        assert!(State::new().demo.is_none());
    }

    #[test]
    fn demo_decision_and_cap_modals_replace_migration_evidence_and_restore_it_on_escape() {
        let _locale = crate::test_support::lock_locale();
        rust_i18n::set_locale("en-US");
        for (width, height) in [(108, 35), (80, 24)] {
            for cap in [false, true] {
                for expanded in [false, true] {
                    let mut state = if cap {
                        cap_setup_state()
                    } else {
                        natural_branch()
                    };
                    if !cap {
                        advance(&mut state, 10_000);
                    }
                    open_work(&mut state, MIGRATION).unwrap();
                    state
                        .editor_view_mut()
                        .insert("migration draft survives the modal");
                    press(&mut state, KeyCode::Left);
                    press(&mut state, KeyCode::F(5));
                    if expanded {
                        press(&mut state, KeyCode::F(7));
                    }
                    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                    let evidence = paint(&mut terminal, &mut state);
                    assert!(evidence.contains("Investigate API v2 | Evidence"));
                    assert!(evidence.contains("Work ID: api-v2"));
                    assert!(evidence.contains(if cap { "F6 Set cap" } else { "F6 Decision" }));
                    let selection = state.selection();
                    let saved = scenario(&state).unwrap().clone();
                    let cursor = state.editor_view_mut().editor.cursor;
                    let scroll = state.view().unwrap().details_scroll;
                    press(&mut state, KeyCode::F(6));
                    let menu = paint(&mut terminal, &mut state);
                    let owner = if cap {
                        "Investigate API v2 | Token cap"
                    } else {
                        "Fix Issue #4821 | Decision"
                    };
                    assert!(menu.contains(owner), "{menu}");
                    assert!(
                        menu.contains("Up/Down Select | Enter Confirm | Esc Cancel"),
                        "{menu}"
                    );
                    assert!(
                        menu.contains(if cap {
                            "> Migration cap: 20K tokens (default)"
                        } else {
                            "> B. Fix compatibility (recommended)"
                        }),
                        "{menu}"
                    );
                    for stale in [
                        "Work ID:",
                        "Schema 2.3",
                        "Breaking changes:",
                        "sample-api-v2",
                        "F5 Back",
                        "F7 Expand",
                        "migration draft",
                        "Investigate API v2 | Evidence",
                    ] {
                        assert!(!menu.contains(stale), "{stale}\n{menu}");
                    }
                    assert_eq!(state.focus, Focus::Actions);
                    assert_eq!(paint(&mut terminal, &mut state), menu);
                    press(&mut state, KeyCode::Esc);
                    assert_eq!(state.focus, Focus::Details);
                    assert_eq!(state.selection(), selection);
                    assert_eq!(scenario(&state).unwrap(), &saved);
                    assert_eq!(
                        state.editor_view_mut().draft,
                        "migration draft survives the modal"
                    );
                    assert_eq!(state.editor_view_mut().editor.cursor, cursor);
                    assert_eq!(state.view().unwrap().details_scroll, scroll);
                    assert_eq!(state.demo.as_ref().unwrap().expanded_details, expanded);
                    assert_eq!(paint(&mut terminal, &mut state), evidence);
                    assert_eq!(state.queued_jobs, 0);
                }
            }
        }
    }

    #[test]
    fn demo_evidence_hint_does_not_gate_branching_and_modal_title_tracks_visible_choices() {
        let mut state = natural_state();
        press(&mut state, KeyCode::F(1));
        submit(&mut state, "Continue fixing issue #4821");
        assert_eq!(
            next_hint(scenario(&state).unwrap()),
            "F5: review evidence before choosing next work"
        );
        assert_eq!(f6_hint(&state), "F6 Actions");
        submit(&mut state, "Should we migrate to API v2?");
        assert!(scenario(&state).unwrap().branch_confirmation_pending);
        assert_eq!(f6_hint(&state), "F6 Branch");
        submit(&mut state, "Create related work");
        press(&mut state, KeyCode::F(5));
        press(&mut state, KeyCode::F(4));
        advance(&mut state, 10_000);
        assert_eq!(
            menu_title(&state, state.menu.as_ref().unwrap()),
            "Investigate API v2 | Actions"
        );
        press(&mut state, KeyCode::F(4));
        assert_eq!(state.focus, Focus::Details);
        assert_eq!(f6_hint(&state), "F6 Decision");
        press(&mut state, KeyCode::F(6));
        assert_eq!(
            menu_title(&state, state.menu.as_ref().unwrap()),
            "Fix Issue #4821 | Decision"
        );
    }

    #[test]
    fn demo_token_cap_menu_is_explicit_scoped_and_cancel_preserves_draft() {
        let mut state = cap_setup_state();
        state.editor_view_mut().insert("keep the issue draft");
        let before = scenario(&state).unwrap().clone();
        advance(&mut state, 60_000);
        assert_eq!(scenario(&state).unwrap(), &before);
        assert!(draw(&mut state, 80, 24).contains("Set token cap to start"));
        press(&mut state, KeyCode::F(6));
        let menu = draw(&mut state, 80, 24);
        assert!(
            menu.contains("> Migration cap: 20K tokens (default)"),
            "{menu}"
        );
        assert!(menu.contains("Investigate API v2 | Token cap"));
        press(&mut state, KeyCode::Esc);
        assert_eq!(scenario(&state).unwrap(), &before);
        assert_eq!(state.editor_view_mut().draft, "keep the issue draft");
        press(&mut state, KeyCode::F(4));
        press(&mut state, KeyCode::Down);
        press(&mut state, KeyCode::Down);
        press(&mut state, KeyCode::Enter);
        assert_eq!(scenario(&state).unwrap(), &before);
        press(&mut state, KeyCode::F(6));
        press(&mut state, KeyCode::Up);
        let InputAction::Submit(input) = press(&mut state, KeyCode::Enter) else {
            panic!("cap confirmation")
        };
        assert_eq!(input, "Set token cap to 10K");
        submit(&mut state, &input);
        assert_eq!(scenario(&state).unwrap().budget.limit, 10_000);
        assert_eq!(state.context.work_id.as_deref(), Some(MIGRATION));
        assert_eq!(state.views[&Some(FIX.into())].draft, "keep the issue draft");
        advance(&mut state, 6_000);
        assert!(scenario(&state).unwrap().budget.paused);
        assert_eq!(scenario(&state).unwrap().budget.consumed, 10_000);
        assert_eq!(state.queued_jobs, 0);
    }

    #[test]
    fn demo_compact_overview_preserves_three_works_and_six_fields_at_small_sizes() {
        let mut state = cap_setup_state();
        submit(&mut state, "Set token cap to 20K");
        advance(&mut state, 12_000);
        press(&mut state, KeyCode::F(2));
        for (width, height) in [(108, 34), (108, 35), (110, 36), (80, 24), (132, 48)] {
            let screen = draw(&mut state, width, height);
            for title in [
                "Fix Issue #4821",
                "Investigate API v2",
                "Prepare Documentation",
            ] {
                assert!(screen.contains(title), "{title}\n{screen}");
            }
            for field in [
                "Status:",
                "Progress:",
                "Blockers:",
                "Decisions:",
                "Deliverables:",
                "Acceptance:",
            ] {
                assert_eq!(screen.matches(field).count(), 3, "{field}\n{screen}");
            }
            assert_eq!(
                screen.matches("Work story demo | Simulated data").count(),
                1
            );
            assert!(!screen.contains("WorkExecutor"));
            assert!(screen.contains("Tokens: 20000 / 20000"));
            assert!(screen.contains("Paused at cap"));
            assert!(screen.contains("Schema 2.3 + notes"));
            assert!(screen.contains("Compatibility failed"));
        }
        let screen = draw(&mut state, 160, 48);
        for line in screen.lines() {
            assert!(line.chars().take(26).all(|ch| ch == ' '));
            assert!(line.chars().skip(134).all(|ch| ch == ' '));
        }
        open_work(&mut state, FIX).unwrap();
        press(&mut state, KeyCode::F(5));
        let evidence = details(&state).unwrap();
        assert!(evidence.contains("fixture-test shell-compatibility"));
        assert!(evidence.contains("exitCode: 1"));
        assert!(evidence.contains("Evidence (scripted-fixture)"));
        open_work(&mut state, MIGRATION).unwrap();
        assert!(details(&state).unwrap().contains("Breaking changes: 3"));
        assert!(details(&state).unwrap().contains("Schema 2.3"));
        assert!(full_details(&state)
            .unwrap()
            .contains("\"breakingChanges\": 3"));
        assert!(full_details(&state)
            .unwrap()
            .contains("\"version\": \"2.3\""));
        assert!(State::new().demo.is_none());
    }

    #[test]
    fn demo_compact_details_fit_evidence_and_complete_handoff_at_100_by_36() {
        let mut state = natural_branch();
        open_work(&mut state, FIX).unwrap();
        state
            .editor_view_mut()
            .insert("keep this draft hidden while reading");
        advance(&mut state, 10_000);
        let saved = scenario(&state).unwrap().clone();
        press(&mut state, KeyCode::F(5));
        let screen = draw(&mut state, 100, 36);
        for marker in [
            "Work story demo",
            "Work ID: fix-4821",
            "sample-fix-4821-compatibility",
            "exitCode: 1",
            "fixture-test shell-compatibility",
            "Evidence (scripted-fixture)",
            "Request owner: api-v2",
            "Result owner: fix-4821",
            "Need: API v2 schema",
            "Consumer: api-v2",
            "Reason: Migration evaluation",
            "Blocking: false",
            "Expected: Versioned schema",
            "Schema 2.3",
            "Breaking changes: 3",
            "Source: scripted-fixture",
            "Legacy shell adapters need an explicit v2 compatibility layer.",
            "Recommendation: Proceed with caution",
        ] {
            assert!(screen.contains(marker), "{marker}\n{screen}");
        }
        assert!(!screen.contains("keep this draft hidden while reading"));
        press(&mut state, KeyCode::F(7));
        assert!(state.demo.as_ref().unwrap().expanded_details);
        assert!(details(&state).unwrap().contains("\"breakingChanges\": 3"));
        assert!(details(&state)
            .unwrap()
            .contains("7/8 compatibility tests passed"));
        press(&mut state, KeyCode::F(7));
        assert!(!state.demo.as_ref().unwrap().expanded_details);
        assert_eq!(scenario(&state).unwrap(), &saved);
        press(&mut state, KeyCode::F(5));
        assert_eq!(
            state.editor_view_mut().draft,
            "keep this draft hidden while reading"
        );
        let messages = &state.views[&Some(FIX.into())].messages;
        assert!(!messages.iter().any(|message| message["text"]
            == "microsoft/foo | fix-4821\nUnit tests passed; compatibility failed."));
    }

    #[tokio::test]
    async fn demo_cap_confirmation_persists_before_consumption_and_survives_reopen() {
        let root = TestDirectory(
            std::env::temp_dir().join(format!("wta-cap-demo-{}", uuid::Uuid::new_v4())),
        );
        let (store, _) = Store::open(&root.0, false).unwrap();
        let mut state = cap_setup_state();
        store.save(scenario(&state).unwrap()).unwrap();
        press(&mut state, KeyCode::F(6));
        let InputAction::Submit(input) = press(&mut state, KeyCode::Enter) else {
            panic!("cap confirmation")
        };
        let next = candidate(&mut state, &input).unwrap().unwrap();
        assert!(scenario(&state).unwrap().awaiting_cap());
        let (store, saved, result) = persist(store, next).await.unwrap();
        result
            .and_then(|()| committed(&mut state, saved, &input))
            .unwrap();
        let expected = scenario(&state).unwrap().clone();
        drop(store);
        let (store, saved) = Store::open(&root.0, false).unwrap();
        assert_eq!(saved, expected);
        assert_eq!(saved.cap_configured, Some(true));
        let mut reopened = new_state(saved).unwrap();
        advance(&mut reopened, 3_000);
        assert_eq!(scenario(&reopened).unwrap().budget.consumed, 5_000);
        assert_eq!(
            scenario(&reopened)
                .unwrap()
                .events
                .iter()
                .filter(|event| event.kind == EventKind::TokenCapSet)
                .count(),
            1
        );
        drop(store);
    }

    fn natural_branch() -> State {
        let mut state = natural_state();
        press(&mut state, KeyCode::F(1));
        submit(&mut state, "Continue fixing issue #4821");
        submit(&mut state, "Should we migrate to API v2?");
        press(&mut state, KeyCode::F(4));
        let InputAction::Submit(input) = press(&mut state, KeyCode::Enter) else {
            panic!("explicit branch confirmation");
        };
        submit(&mut state, &input);
        state
    }

    #[test]
    fn demo_natural_hints_and_menus_offer_navigation_not_director_commands() {
        let mut state = natural_branch();
        state.editor_view_mut().insert("preserved while inspecting");
        let saved = scenario(&state).unwrap().clone();
        press(&mut state, KeyCode::F(4));
        let menu = draw(&mut state, 132, 48);
        assert!(menu.contains("View Work details"));
        for manual in [
            "Show attention",
            "Show budget",
            "Show schema exchange",
            "Show work overview",
        ] {
            assert!(!menu.contains(manual), "{menu}");
        }
        press(&mut state, KeyCode::Enter);
        assert_eq!(state.focus, Focus::Details);
        assert_eq!(state.editor_view_mut().draft, "preserved while inspecting");
        assert_eq!(scenario(&state).unwrap(), &saved);
        press(&mut state, KeyCode::F(5));
        press(&mut state, KeyCode::F(4));
        press(&mut state, KeyCode::Down);
        press(&mut state, KeyCode::Enter);
        assert!(state.task_list);
        assert_eq!(scenario(&state).unwrap(), &saved);
        advance(&mut state, 10_000);
        assert!(next_hint(scenario(&state).unwrap()).contains("F6: decide"));
        submit(&mut state, "B");
        assert!(next_hint(scenario(&state).unwrap()).contains("advances automatically"));
        advance(&mut state, 12_000);
        assert!(next_hint(scenario(&state).unwrap()).contains("F6: add 10K"));
        press(&mut state, KeyCode::F(6));
        let menu = draw(&mut state, 132, 48);
        assert!(!menu.contains("Show budget"));
        assert!(!menu.contains("Show schema exchange"));
    }

    fn advance(state: &mut State, milliseconds: u64) {
        let mut saved = scenario(state).unwrap().clone();
        saved
            .advance(std::time::Duration::from_millis(milliseconds))
            .unwrap();
        synchronize(state, saved).unwrap();
    }

    #[test]
    fn demo_legacy_session_tabs_are_selectable_distinct_and_leave_for_global_work() {
        let mut state = natural_state();
        let before = scenario(&state).unwrap().clone();
        let first = draw(&mut state, 132, 48);
        assert!(first.contains("[Fix bug]"));
        assert!(first.contains("the shell reports a failure"));
        assert!(first.contains("Work story demo | Simulated data"));
        for (tab, marker) in [
            ("Code review", "Ask that session"),
            ("Migration", "Keep the investigation separate"),
            ("Research", "No decision is recorded"),
        ] {
            press(&mut state, KeyCode::Right);
            let screen = draw(&mut state, 132, 48);
            assert!(screen.contains(&format!("[{tab}]")), "{screen}");
            assert!(screen.contains(marker), "{screen}");
            assert!(!screen.contains("the shell reports a failure"));
        }
        press(&mut state, KeyCode::Left);
        assert!(draw(&mut state, 132, 48).contains("[Migration]"));
        assert_eq!(scenario(&state).unwrap(), &before);
        press(&mut state, KeyCode::F(1));
        assert!(draw(&mut state, 132, 48).contains("One existing Work"));
        assert!(!state.demo.as_ref().unwrap().legacy);
        for ch in "Continue fixing ".chars() {
            press(&mut state, KeyCode::Char(ch));
        }
        handle_paste(&mut state, "issue #4821");
        let InputAction::Submit(input) = press(&mut state, KeyCode::Enter) else {
            panic!("resume")
        };
        submit(&mut state, &input);
        let screen = draw(&mut state, 132, 48);
        for marker in [
            "Completed: Reproduce issue",
            "Blocked: Compatibility test",
            "Pending: Documentation",
        ] {
            assert!(screen.contains(marker), "{screen}");
        }
        press(&mut state, KeyCode::F(5));
        assert!(draw(&mut state, 132, 48).contains("Scene 3/8"));
        assert_eq!(scenario(&state).unwrap().scene, 2);
        press(&mut state, KeyCode::F(5));
        press(&mut state, KeyCode::Tab);
        assert_eq!(
            state.editor_view_mut().draft,
            "Should we migrate to API v2?"
        );
    }

    #[test]
    fn demo_natural_attention_is_proactive_owner_scoped_and_does_not_steal_focus() {
        let mut state = natural_branch();
        assert!(draw(&mut state, 132, 48).contains("Schema 2.3 | 3 breaking changes"));
        state.editor_view_mut().insert("migration draft to retain");
        press(&mut state, KeyCode::Left);
        let cursor = state.editor_view_mut().editor.cursor;
        let selection = state.selection();
        press(&mut state, KeyCode::F(5));
        advance(&mut state, 9_999);
        assert!(!scenario(&state).unwrap().attention_pending());
        advance(&mut state, 1);
        assert_eq!(state.focus, Focus::Details);
        assert_eq!(state.selection(), selection);
        assert_eq!(state.editor_view_mut().draft, "migration draft to retain");
        assert_eq!(state.editor_view_mut().editor.cursor, cursor);
        let screen = draw(&mut state, 132, 48);
        assert!(screen.contains("Fix Issue #4821 | Compatibility failed"));
        assert!(screen.contains("human decision needed"));
        press(&mut state, KeyCode::F(2));
        assert!(draw(&mut state, 132, 48).contains("Fix Issue #4821 | Compatibility failed"));
        press(&mut state, KeyCode::F(6));
        let menu = draw(&mut state, 132, 48);
        assert!(menu.contains("B. Fix compatibility (recommended) - Fix Issue #4821"));
        let InputAction::Submit(input) = press(&mut state, KeyCode::Enter) else {
            panic!("decision")
        };
        submit(&mut state, &input);
        assert_eq!(
            scenario(&state).unwrap().decision.as_ref().unwrap().work_id,
            FIX
        );
        assert_eq!(scenario(&state).unwrap().works[0].evidence[3].exit_code, 1);
        assert_eq!(
            state.views[&Some(MIGRATION.into())].draft,
            "migration draft to retain"
        );
        assert_eq!(state.queued_jobs, 0);
    }

    #[test]
    fn demo_natural_timer_preserves_open_menu_and_automatic_budget_final_overview() {
        let mut state = natural_branch();
        press(&mut state, KeyCode::F(4));
        let original_menu = state
            .menu
            .as_ref()
            .unwrap()
            .items
            .iter()
            .map(|(label, _)| label.clone())
            .collect::<Vec<_>>();
        advance(&mut state, 10_000);
        assert_eq!(
            state
                .menu
                .as_ref()
                .unwrap()
                .items
                .iter()
                .map(|(label, _)| label.clone())
                .collect::<Vec<_>>(),
            original_menu
        );
        assert!(banner(&state).is_none());
        press(&mut state, KeyCode::Esc);
        assert!(banner(&state).unwrap().contains("human decision needed"));
        submit(&mut state, "B");
        open_work(&mut state, MIGRATION).unwrap();
        state.editor_view_mut().insert("do not clear this draft");
        press(&mut state, KeyCode::F(2));
        let selection = state.selection();
        for consumed in [5_000, 10_000, 15_000, 20_000] {
            advance(&mut state, 3_000);
            let screen = draw(&mut state, 132, 48);
            assert!(
                screen.contains(&format!("Tokens: {consumed} / 20000")),
                "{screen}"
            );
            assert_eq!(state.selection(), selection);
            assert!(state.task_list);
            assert_eq!(
                state.views[&Some(MIGRATION.into())].draft,
                "do not clear this draft"
            );
        }
        let screen = draw(&mut state, 132, 48);
        for title in [
            "Fix Issue #4821",
            "Investigate API v2",
            "Prepare Documentation",
        ] {
            assert!(screen.contains(title));
        }
        assert_eq!(screen.matches("Acceptance: Pending").count(), 3);
        assert!(!screen.contains("WorkExecutor"));
        assert!(screen.contains("(from Fix Issue #4821)"));
        assert!(screen.contains("Paused at cap"));
        press(&mut state, KeyCode::F(6));
        let InputAction::Submit(input) = press(&mut state, KeyCode::Enter) else {
            panic!("budget action")
        };
        assert_eq!(input, "Add 10K budget");
        submit(&mut state, &input);
        let saved = scenario(&state).unwrap().clone();
        advance(&mut state, 50_000);
        assert_eq!(scenario(&state).unwrap(), &saved);
        assert_eq!(
            state.views[&Some(MIGRATION.into())].draft,
            "do not clear this draft"
        );
        assert!(saved
            .events
            .iter()
            .filter_map(|event| event.input.as_deref())
            .all(|input| !input.starts_with("Show ")));
    }

    #[tokio::test]
    async fn demo_autonomous_persistence_failure_and_reopen_do_not_replay_attention() {
        let root = TestDirectory(
            std::env::temp_dir().join(format!("wta-natural-demo-{}", uuid::Uuid::new_v4())),
        );
        let (store, _) = Store::open(&root.0, false).unwrap();
        let mut state = natural_branch();
        store.save(scenario(&state).unwrap()).unwrap();
        state
            .editor_view_mut()
            .insert("keep draft after I/O failure");
        let database = rusqlite::Connection::open(root.0.join("work-story.sqlite3")).unwrap();
        database.execute_batch("CREATE TRIGGER reject_clock BEFORE UPDATE ON scenario BEGIN SELECT RAISE(FAIL, 'clock save failed'); END;").unwrap();
        let mut candidate = scenario(&state).unwrap().clone();
        candidate
            .advance(std::time::Duration::from_secs(10))
            .unwrap();
        let (store, saved, result) = persist(store, candidate).await.unwrap();
        let error = result
            .and_then(|()| synchronize(&mut state, saved))
            .unwrap_err();
        state.notice = format!("{error:#}");
        state.notice_kind = NoticeKind::Error;
        assert!(!scenario(&state).unwrap().attention_pending());
        assert!(draw(&mut state, 132, 48).contains("clock save failed"));
        assert_eq!(
            state.editor_view_mut().draft,
            "keep draft after I/O failure"
        );
        database.execute_batch("DROP TRIGGER reject_clock").unwrap();
        let mut candidate = scenario(&state).unwrap().clone();
        candidate
            .advance(std::time::Duration::from_secs(10))
            .unwrap();
        let (store, saved, result) = persist(store, candidate).await.unwrap();
        result
            .and_then(|()| synchronize(&mut state, saved))
            .unwrap();
        let messages = state.views[&Some(FIX.into())].messages.clone();
        drop(store);
        drop(database);
        let (store, saved) = Store::open(&root.0, false).unwrap();
        let mut reopened = new_state(saved).unwrap();
        advance(&mut reopened, 50_000);
        assert_eq!(reopened.views[&Some(FIX.into())].messages, messages);
        assert_eq!(
            scenario(&reopened)
                .unwrap()
                .events
                .iter()
                .filter(|event| event.kind == EventKind::AttentionRaised)
                .count(),
            1
        );
        submit(&mut reopened, "B");
        let mut saved = scenario(&reopened).unwrap().clone();
        saved.advance(std::time::Duration::from_secs(12)).unwrap();
        let (store, saved, result) = persist(store, saved).await.unwrap();
        result
            .and_then(|()| synchronize(&mut reopened, saved))
            .unwrap();
        let paused = scenario(&reopened).unwrap().clone();
        drop(store);
        let (store, saved) = Store::open(&root.0, false).unwrap();
        let mut final_view = new_state(saved).unwrap();
        advance(&mut final_view, 60_000);
        assert_eq!(scenario(&final_view).unwrap(), &paused);
        assert!(scenario(&final_view).unwrap().budget.paused);
        press(&mut final_view, KeyCode::F(2));
        assert!(draw(&mut final_view, 132, 48).contains("Scene 8/8"));
        drop(store);
    }

    fn draw(state: &mut State, width: u16, height: u16) -> String {
        let _locale = crate::test_support::lock_locale();
        rust_i18n::set_locale("en-US");
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        paint(&mut terminal, state)
    }

    fn paint(terminal: &mut Terminal<TestBackend>, state: &mut State) -> String {
        terminal
            .draw(|frame| renderer::render(frame, state))
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .chunks(usize::from(terminal.backend().buffer().area.width))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn press(state: &mut State, code: KeyCode) -> InputAction {
        key(state, code, KeyModifiers::NONE).unwrap()
    }

    fn submit(state: &mut State, input: &str) {
        let next = candidate(state, input).unwrap().unwrap();
        // Pure tests model the successful persistence boundary.
        committed(state, next, input).unwrap();
        assert_eq!(state.queued_jobs, 0);
        assert!(state.mutations.is_empty());
        assert!(!state.busy);
    }

    fn branched() -> State {
        let mut state = new_state(Scenario::new()).unwrap();
        for input in [
            "Continue fixing issue #4821",
            "Show work state",
            "Should we migrate to API v2?",
            "Create related work",
        ] {
            submit(&mut state, input);
        }
        state
    }

    fn completed() -> State {
        let mut state = branched();
        for input in [
            "Show schema exchange",
            "Show attention",
            "B",
            "Show budget",
            "Show work overview",
        ] {
            submit(&mut state, input);
        }
        state
    }

    #[test]
    fn demo_f1_f2_share_state_and_open_local_work_with_scoped_drafts() {
        let mut state = new_state(Scenario::new()).unwrap();
        assert!(State::new().demo.is_none());
        let initial = draw(&mut state, 132, 48);
        assert!(initial.contains("Historical agent sessions"));
        assert!(initial.contains("Work story demo | Simulated data"));
        assert!(!initial.contains("Tab 1"));
        press(&mut state, KeyCode::F(1));
        assert!(draw(&mut state, 132, 48).contains("One existing Work"));
        state.chat.insert("global draft");
        let canonical = scenario(&state).unwrap().clone();
        press(&mut state, KeyCode::F(2));
        let overview = draw(&mut state, 132, 48);
        assert!(overview.contains("> Fix Issue #4821"));
        assert!(!overview.contains("Investigate API v2"));
        press(&mut state, KeyCode::Enter);
        assert_eq!(state.context.work_id.as_deref(), Some(FIX));
        assert!(!state.context.global_conversation);
        state.editor_view_mut().insert("fix draft");
        press(&mut state, KeyCode::F(1));
        assert_eq!(state.editor_view_mut().draft, "global draft");
        press(&mut state, KeyCode::F(2));
        press(&mut state, KeyCode::Enter);
        assert_eq!(state.editor_view_mut().draft, "fix draft");
        assert_eq!(scenario(&state).unwrap(), &canonical);
        assert_eq!(state.queued_jobs, 0);
    }

    #[test]
    fn demo_branch_menu_requires_confirmation_and_supports_cancellation() {
        let mut state = new_state(Scenario::new()).unwrap();
        submit(&mut state, "Continue fixing issue #4821");
        assert!(candidate(&mut state, "Create related work").is_err());
        submit(&mut state, "Should we migrate to API v2?");
        state
            .editor_view_mut()
            .replace_draft("keep this Work draft".into());
        assert_eq!(state.works.len(), 1);
        press(&mut state, KeyCode::F(4));
        let modal = draw(&mut state, 132, 48);
        assert!(modal.contains("Create related work"));
        assert!(modal.contains("Cancel related work"));
        press(&mut state, KeyCode::Down);
        let InputAction::Submit(input) = press(&mut state, KeyCode::Enter) else {
            panic!("menu submit")
        };
        submit(&mut state, &input);
        assert_eq!(state.works.len(), 1);
        assert_eq!(state.editor_view_mut().draft, "keep this Work draft");
        assert!(!scenario(&state).unwrap().branch_confirmation_pending);
        submit(&mut state, "Should we migrate to API v2?");
        press(&mut state, KeyCode::F(6));
        let InputAction::Submit(input) = press(&mut state, KeyCode::Enter) else {
            panic!("menu submit")
        };
        submit(&mut state, &input);
        assert_eq!(state.works.len(), 2);
        assert_eq!(state.context.work_id.as_deref(), Some(MIGRATION));
        assert_eq!(
            scenario(&state).unwrap().works[1].parent_work_id.as_deref(),
            Some(FIX)
        );
        open_work(&mut state, FIX).unwrap();
        assert_eq!(state.editor_view_mut().draft, "keep this Work draft");
    }

    #[test]
    fn demo_b_resolves_decision_without_passing_failed_evidence() {
        let mut state = branched();
        submit(&mut state, "Show attention");
        press(&mut state, KeyCode::F(6));
        let choices = draw(&mut state, 132, 48);
        assert!(choices.contains("B. Fix compatibility"));
        let InputAction::Submit(input) = press(&mut state, KeyCode::Enter) else {
            panic!("decision")
        };
        assert_eq!(input, "B");
        let before = scenario(&state).unwrap().works[0].evidence.clone();
        submit(&mut state, &input);
        let saved = scenario(&state).unwrap();
        assert!(saved.decision.as_ref().unwrap().resolved);
        assert_eq!(saved.works[0].evidence, before);
        assert_eq!(saved.works[0].evidence.last().unwrap().exit_code, 1);
        assert!(saved.works[0].acceptance.starts_with("Pending"));
        assert_eq!(state.works.len(), 3);
        assert!(draw(&mut state, 132, 48).contains("Compatibility still failed"));
    }

    #[test]
    fn demo_schema_handoff_is_attached_to_owner_work_histories_and_details() {
        let mut state = branched();
        submit(&mut state, "Show schema exchange");
        for id in [FIX, MIGRATION] {
            let records = &state.views[&Some(id.into())].records;
            assert_eq!(records["demo-schema-request"]["ownerWorkId"], MIGRATION);
            assert_eq!(records["demo-schema-response"]["ownerWorkId"], FIX);
            assert_eq!(
                records["demo-schema-response"]["response"]["version"],
                "2.3"
            );
            assert_eq!(
                records["demo-schema-response"]["response"]["breakingChanges"],
                3
            );
        }
        let fix_messages = serde_json::to_string(&state.views[&Some(FIX.into())].messages).unwrap();
        let migration_messages =
            serde_json::to_string(&state.views[&Some(MIGRATION.into())].messages).unwrap();
        assert!(!migration_messages.contains("Don't resume the agent"));
        assert!(!fix_messages.contains("\"text\":\"Show schema exchange\""));
        assert!(migration_messages.contains("Request owner: api-v2"));
        press(&mut state, KeyCode::F(5));
        let details = details(&state).unwrap();
        assert!(details.contains("Schema") || details.contains("schema"));
        assert!(details.contains("Breaking changes: 3"));
        assert!(draw(&mut state, 132, 48).contains("Parent:"));
    }

    #[test]
    fn demo_budget_resumes_same_work_and_reopen_reconstructs_scoped_history() {
        let mut state = branched();
        for input in ["Show attention", "B", "Show budget"] {
            submit(&mut state, input);
        }
        let paused = scenario(&state).unwrap().clone();
        assert_eq!(paused.budget.consumed, 20_000);
        assert_eq!(paused.budget.limit, 20_000);
        assert!(paused.budget.paused);
        let migration = paused.works[1].clone();
        submit(&mut state, "Add 10K budget");
        let resumed = scenario(&state).unwrap().clone();
        assert_eq!(resumed.budget.limit, 30_000);
        assert!(!resumed.budget.paused);
        assert_eq!(resumed.works[1].id, migration.id);
        assert_eq!(
            resumed.works[1].executor_session_id,
            migration.executor_session_id
        );
        assert_eq!(resumed.works[1].findings, migration.findings);
        let mut reopened = new_state(resumed).unwrap();
        for id in [FIX, MIGRATION, "documentation"] {
            assert_eq!(
                state.views[&Some(id.into())].messages,
                reopened.views[&Some(id.into())].messages
            );
        }
        assert!(draw(&mut reopened, 132, 48).contains("30K"));
        state.editor_view_mut().insert("migration draft");
        open_work(&mut state, FIX).unwrap();
        state.editor_view_mut().insert("fix draft");
        open_work(&mut state, MIGRATION).unwrap();
        assert_eq!(state.editor_view_mut().draft, "migration draft");
    }

    #[test]
    fn demo_final_overview_has_three_six_field_cards_at_132_by_48() {
        let mut state = completed();
        let screen = draw(&mut state, 132, 48);
        for title in [
            "Fix Issue #4821",
            "Investigate API v2",
            "Prepare Documentation",
        ] {
            assert!(screen.contains(title), "{title}\n{screen}");
        }
        for label in [
            "Status:",
            "Progress:",
            "Blockers:",
            "Decisions:",
            "Deliverables:",
            "Acceptance:",
        ] {
            assert_eq!(screen.matches(label).count(), 3, "{label}\n{screen}");
        }
        assert!(screen.contains("Compatibility failed"));
        assert!(screen.contains("(from Fix Issue #4821)"));
        assert!(screen.contains("Work story demo | Simulated data"));
        assert!(!screen.contains("Tab 1"));
    }

    #[test]
    fn demo_work_overview_renders_projected_ordinary_work_records() {
        let mut state = new_state(Scenario::new()).unwrap();
        assert_eq!(state.works[0]["work"]["kind"], "Work");
        assert_eq!(state.works[0]["work"]["lifecycle"], "Active");
        assert_eq!(state.works[0]["work"]["projectId"], PROJECT);
        state.works[0]["spec"]["goal"] = json!("Projected ordinary Work");
        state.works[0]["demo"]["status"] = json!("Projected evidence status");
        press(&mut state, KeyCode::F(2));
        let screen = draw(&mut state, 132, 48);
        assert!(screen.contains("Projected ordinary Work"));
        assert!(screen.contains("Projected evidence status"));
    }

    #[test]
    fn demo_narrow_overview_and_details_scroll_without_mutating_story() {
        let mut state = completed();
        let saved = scenario(&state).unwrap().clone();
        let before = draw(&mut state, 44, 18);
        press(&mut state, KeyCode::PageDown);
        let after = draw(&mut state, 44, 18);
        assert_ne!(before, after);
        assert!(state.demo.as_ref().unwrap().overview_scroll > 0);
        press(&mut state, KeyCode::Down);
        press(&mut state, KeyCode::Down);
        let docs = draw(&mut state, 44, 18);
        assert!(docs.contains("Prepare Documentation"));
        press(&mut state, KeyCode::Enter);
        press(&mut state, KeyCode::F(5));
        press(&mut state, KeyCode::F(7));
        let before = draw(&mut state, 44, 18);
        press(&mut state, KeyCode::PageDown);
        let after = draw(&mut state, 44, 18);
        assert_ne!(before, after);
        assert_eq!(scenario(&state).unwrap(), &saved);
    }

    #[test]
    fn demo_character_paste_tab_progression_and_unknown_inputs_are_isolated() {
        let mut state = new_state(Scenario::new()).unwrap();
        press(&mut state, KeyCode::F(1));
        for ch in "Continue fixing ".chars() {
            press(&mut state, KeyCode::Char(ch));
        }
        handle_paste(&mut state, "issue #4821");
        assert_eq!(state.chat.draft, "Continue fixing issue #4821");
        let InputAction::Submit(input) = press(&mut state, KeyCode::Enter) else {
            panic!("submit")
        };
        submit(&mut state, &input);
        press(&mut state, KeyCode::Tab);
        assert_eq!(state.editor_view_mut().draft, "Show work state");
        let InputAction::Submit(input) = press(&mut state, KeyCode::Enter) else {
            panic!("submit")
        };
        submit(&mut state, &input);
        press(&mut state, KeyCode::Tab);
        assert_eq!(
            state.editor_view_mut().draft,
            "Should we migrate to API v2?"
        );
        let saved = scenario(&state).unwrap().clone();
        assert!(candidate(&mut state, "launch production agent").is_err());
        assert!(candidate(&mut state, "Show budget").is_err());
        assert_eq!(scenario(&state).unwrap(), &saved);
        let (sender, mut receiver) = mpsc::unbounded_channel();
        assert!(queue(&mut state, &sender, JobKind::Refresh).is_err());
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        assert_eq!(state.queued_jobs, 0);
        assert!(state.pending.is_none());
        assert!(state.mutations.is_empty());
    }

    #[test]
    fn demo_reset_requires_confirmation_and_quit_never_queues_jobs() {
        let mut state = branched();
        assert!(candidate(&mut state, "/reset confirm").is_err());
        assert!(candidate(&mut state, "/reset").unwrap().is_none());
        assert_eq!(state.works.len(), 2);
        submit(&mut state, "/reset confirm");
        assert_eq!(state.works.len(), 1);
        assert!(state.context.global_conversation);
        assert!(state.chat.draft.is_empty());
        assert!(matches!(
            key(&mut state, KeyCode::Char('q'), KeyModifiers::CONTROL).unwrap(),
            InputAction::Quit
        ));
        assert_eq!(state.queued_jobs, 0);
    }

    #[tokio::test]
    async fn demo_save_precedes_projection_and_failure_preserves_draft_and_durable_history() {
        let root = TestDirectory(
            std::env::temp_dir().join(format!("wta-integrated-demo-{}", uuid::Uuid::new_v4())),
        );
        let (store, saved) = Store::open(&root.0, false).unwrap();
        let mut state = new_state(saved).unwrap();
        press(&mut state, KeyCode::F(1));
        let input = "Continue fixing issue #4821";
        state.chat.replace_draft(input.into());
        let next = candidate(&mut state, input).unwrap().unwrap();
        assert!(state.context.global_conversation);
        let (store, saved, result) = persist(store, next).await.unwrap();
        result
            .and_then(|()| committed(&mut state, saved, input))
            .unwrap();
        assert_eq!(state.context.work_id.as_deref(), Some(FIX));
        assert!(state.chat.draft.is_empty());
        drop(store);
        let (store, saved) = Store::open(&root.0, false).unwrap();
        let reopened = new_state(saved).unwrap();
        assert_eq!(
            state.views[&Some(FIX.into())].messages,
            reopened.views[&Some(FIX.into())].messages
        );

        let input = "Show work state";
        state.editor_view_mut().replace_draft(input.into());
        let prior = scenario(&state).unwrap().clone();
        let messages = state.views[&Some(FIX.into())].messages.clone();
        let next = candidate(&mut state, input).unwrap().unwrap();
        // A trigger rejects writes without changing the previously committed snapshot.
        let database = rusqlite::Connection::open(root.0.join("work-story.sqlite3")).unwrap();
        database
            .execute_batch(
                "CREATE TRIGGER reject_demo_update BEFORE UPDATE ON scenario
             BEGIN SELECT RAISE(FAIL, 'simulated storage failure'); END;",
            )
            .unwrap();
        let (store, saved, result) = persist(store, next).await.unwrap();
        let error = result
            .and_then(|()| committed(&mut state, saved, input))
            .unwrap_err();
        state.notice = format!("{error:#}");
        state.notice_kind = NoticeKind::Error;
        assert!(draw(&mut state, 132, 48).contains("simulated storage failure"));
        assert_eq!(scenario(&state).unwrap(), &prior);
        assert_eq!(state.editor_view_mut().draft, input);
        assert_eq!(state.views[&Some(FIX.into())].messages, messages);
        drop(store);
        drop(database);
        let (store, saved) = Store::open(&root.0, false).unwrap();
        assert_eq!(saved, prior);
        drop(store);
    }
}
