// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

use super::*;
use crate::shell::wt_channel::{CliChannel, WtChannel};

fn commandline(executable: &str, work: &str) -> Result<String> {
    let work = uuid::Uuid::parse_str(work).context("Invalid task ID")?;
    anyhow::ensure!(
        !executable.is_empty() && !executable.contains('\0'),
        "Invalid executable path"
    );
    Ok(format!(
        "{} ui --work {}",
        crate::coordinator::quote_windows_commandline_arg(executable),
        work
    ))
}

fn require_host(host: Option<&str>) -> Result<()> {
    anyhow::ensure!(
        host.is_some_and(|value| !value.is_empty()),
        "{}",
        t!("agent_center.task_tab_unsupported")
    );
    Ok(())
}

pub(super) async fn open(work: &str) -> Result<Value> {
    require_host(std::env::var("WT_COM_CLSID").ok().as_deref())?;
    let executable = std::env::current_exe().context("Locating Agent Center executable")?;
    let commandline = commandline(
        executable.to_str().context("Invalid executable path")?,
        work,
    )?;
    let channel = CliChannel::connect().await?;
    let result = channel
        .request(
            "create_tab",
            json!({
                "commandline":commandline, "title":t!("agent_center.tasks_title").into_owned()
            }),
        )
        .await?;
    anyhow::ensure!(
        result.get("session_id").is_some() || result.get("tab_id").is_some(),
        "{}",
        t!("agent_center.invalid_response")
    );
    Ok(json!({"status":"ok","data":result}))
}

impl State {
    pub(super) fn open_work(&mut self, data: &Value) -> Result<()> {
        let context = &data["context"];
        let id = context["selectedWorkId"]
            .as_str()
            .context("work.open selectedWorkId")?;
        let conversation = context["conversationId"]
            .as_str()
            .context("work.open conversationId")?;
        let console = context["consoleSessionId"]
            .as_str()
            .context("work.open consoleSessionId")?;
        let version = context["contextVersion"]
            .as_u64()
            .context("work.open contextVersion")?;
        let project = context["projectId"]
            .as_str()
            .context("work.open projectId")?;
        anyhow::ensure!(
            data["workView"]["work"]["id"] == id && data["conversation"]["id"] == conversation,
            "{}",
            t!("agent_center.invalid_response")
        );
        if self.context.work_id.as_deref() != Some(id) {
            self.return_work = Some(self.selection());
        }
        if self.context.global_conversation {
            self.global_selection = self.selection();
        }
        self.remember_work(&data["workView"]);
        self.context.global_conversation = false;
        self.select(Some(id.into()));
        self.context.project_id = Some(project.into());
        self.context.conversation_id = conversation.into();
        self.context.console_session_id = console.into();
        self.context.context_version = version;
        self.conversations
            .insert((Some(project.into()), Some(id.into())), conversation.into());
        self.conversation_consoles
            .insert(conversation.into(), console.into());
        self.conversation_targets
            .insert(conversation.into(), Some(id.into()));
        self.sync_view_context();
        self.observe(&json!({"data":data["workView"]}));
        self.append(Some(id.into()), &data["conversation"]);
        self.dashboard = false;
        self.dashboard_work = false;
        self.task_list = false;
        Ok(())
    }

    pub(super) fn can_claim_executor(&self) -> bool {
        !self.context.global_conversation
            && self
                .selected_view()
                .is_some_and(|view| view["continuation"]["canClaimExecutor"] == true)
    }

    pub(super) fn continue_operation(&self, restart: bool) -> Result<Operation> {
        let id = self.context.work_id.as_ref().context("No selected task")?;
        let view = self
            .works
            .iter()
            .find(|view| view["work"]["id"] == *id)
            .context("Missing selected task")?;
        let state = view["continuation"]["state"].as_str().unwrap_or("");
        let claim = view["continuation"]["canClaimExecutor"] == true;
        anyhow::ensure!(
            view["work"]["lifecycle"] == "Active"
                && if restart {
                    state == "Unavailable" && view["continuation"]["canRestartSession"] == true
                } else {
                    claim
                        || matches!(
                            state,
                            "Running" | "WaitingForInput" | "Paused" | "NeedsRecovery" | "Ready"
                        )
                },
            "{}",
            t!("agent_center.status_conflict")
        );
        let version = view["work"]["version"]
            .as_u64()
            .context("Missing task version")?;
        let mut operation = Operation::read(
            if claim {
                "work.claim_executor"
            } else {
                "work.continue"
            },
            json!({"workId":id}),
        );
        operation.mutation = true;
        operation.confirmation = claim;
        operation.if_match = vec![json!({"kind":"Work","id":id,"version":version})];
        if restart {
            operation.params["restartSession"] = json!(true);
            operation.confirmation = true;
        }
        Ok(operation)
    }

    pub(super) fn tasks(&self) -> Vec<&Value> {
        self.works
            .iter()
            .filter(|view| {
                self.home_screen == HomeScreen::Overview
                    || matches!(
                        view["continuation"]["state"].as_str(),
                        Some("WaitingForInput" | "Paused" | "NeedsRecovery" | "Unavailable")
                    )
                    || self
                        .inbox
                        .iter()
                        .any(|item| item["workId"] == view["work"]["id"] && attention_open(item))
                    || view["work"]["lifecycle"] == "Draft"
            })
            .collect()
    }
}

pub(super) fn continuation_label(view: &Value) -> String {
    match view["continuation"]["state"].as_str() {
        Some("Running") => t!("agent_center.task_state_running"),
        Some("WaitingForInput") => t!("agent_center.task_state_waiting"),
        Some("Paused") => t!("agent_center.task_state_paused"),
        Some("NeedsRecovery") => t!("agent_center.task_state_recovery"),
        Some("Ready") => t!("agent_center.task_state_ready"),
        Some("Completed") => t!("agent_center.task_state_completed"),
        Some("Cancelled") => t!("agent_center.task_state_cancelled"),
        Some("Unavailable") => t!("agent_center.task_state_unavailable"),
        _ => return projection::status(&view["work"]["lifecycle"]),
    }
    .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_tab_uses_same_executable_and_validated_uuid_without_a_shell() {
        let id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
        assert_eq!(
            commandline(r"C:\Program Files\IT & tools\wta.exe", id).unwrap(),
            format!(r#""C:\Program Files\IT & tools\wta.exe" ui --work {id}"#)
        );
        assert!(commandline("wta.exe", "id & calc.exe").is_err());
        assert!(require_host(None).is_err());
        assert!(require_host(Some("")).is_err());
        assert!(require_host(Some("host")).is_ok());
    }

    fn opened(id: &str, conversation: &str, state: &str) -> Value {
        json!({
            "workView":{"work":{"kind":"Work","id":id,"projectId":"project","version":7,
                "lifecycle":"Active"},"spec":{"goal":format!("Task {id}")},"continuation":{"state":state}},
            "conversation":{"kind":"Conversation","id":conversation,
                "messages":[{"kind":"ConversationItem","id":format!("message-{id}"),
                    "role":"assistant","version":1,"text":format!("History {id}")}]},
            "context":{"selectedWorkId":id,"projectId":"project","conversationId":conversation,
                "consoleSessionId":format!("console-{id}"),"contextVersion":3},
            "continuation":{"state":state}
        })
    }

    #[test]
    fn historical_streaming_never_drives_task_thinking_without_a_live_response() {
        let mut state = State::new();
        let mut task = opened("a", "conversation-a", "NeedsRecovery");
        task["conversation"]["messages"][0]["status"] = json!("Streaming");
        state.open_work(&task).unwrap();
        state
            .editor_view_mut()
            .replace_draft("Keep this draft".into());
        assert!(!state.thinking());
        for (message, deadline, expected) in [
            ("message-a", "2999-01-01T00:00:00Z", true),
            ("other-reply", "2999-01-01T00:00:00Z", false),
            ("message-a", "2000-01-01T00:00:00Z", false),
            ("message-a", "invalid", false),
        ] {
            state.works[0]["continuation"]["activeResponses"] =
                json!([{"messageId":message,"deadlineUtc":deadline}]);
            assert_eq!(state.thinking(), expected);
        }
        state.works[0]["continuation"]["state"] = json!("Running");
        state.works[0]["continuation"]["activeResponses"] = json!([]);
        assert!(
            !state.thinking(),
            "A running worker is not an assistant reply"
        );
        assert_eq!(
            state.editor_view().unwrap().messages[0]["status"],
            "Streaming"
        );
        assert_eq!(state.editor_view().unwrap().draft, "Keep this draft");
    }

    #[test]
    fn recovery_receipt_and_failure_remain_scoped_without_implying_success() {
        let _locale = crate::test_support::lock_locale();
        let mut state = State::new();
        let mut a = opened("a", "conversation-a", "NeedsRecovery");
        let failure = json!({"code":"OUTCOME_UNKNOWN","message":"Old execution is not settled"});
        a["workView"]["continuation"]["recoveryFailure"] = failure.clone();
        state.open_work(&a).unwrap();
        state.set_response_notice(&json!({"status":"ok","data":a}));
        assert_eq!(state.notice, t!("agent_center.task_state_recovery"));
        assert!(matches!(state.notice_kind, NoticeKind::Attention));
        assert_eq!(state.recovery_failure(), Some(&failure));
        state
            .open_work(&opened("b", "conversation-b", "Ready"))
            .unwrap();
        assert!(state.recovery_failure().is_none());
        state.open_work(&a).unwrap();
        assert_eq!(state.recovery_failure(), Some(&failure));
        assert!(!state.thinking());
        state.task_list = true;
        assert!(state.recovery_failure().is_none());
    }

    #[test]
    fn accepted_continuation_receipts_report_projected_state_not_generic_success() {
        let _locale = crate::test_support::lock_locale();
        let mut state = State::new();
        for status in ["ok", "pending", "needs_input"] {
            for continuation in [
                "NeedsRecovery",
                "WaitingForInput",
                "Paused",
                "Unavailable",
                "Running",
                "Ready",
                "Completed",
                "Cancelled",
            ] {
                let view = json!({"continuation":{"state":continuation}});
                for data in [view.clone(), json!({"workView":view})] {
                    state.set_response_notice(&json!({"status":status,"data":data}));
                    assert_eq!(state.notice, continuation_label(&view));
                    assert_ne!(state.notice, t!("agent_center.status_ok"));
                    assert_eq!(
                        matches!(state.notice_kind, NoticeKind::Attention),
                        matches!(
                            continuation,
                            "NeedsRecovery" | "WaitingForInput" | "Paused" | "Unavailable"
                        )
                    );
                }
            }
        }
        for status in ["error", "conflict", "unsupported"] {
            let response = json!({"status":status,
                "data":{"continuation":{"state":"Running"}}});
            state.set_response_notice(&response);
            assert_eq!(state.notice, status_label(&response));
            assert!(matches!(state.notice_kind, NoticeKind::Error));
        }
    }

    #[test]
    fn default_is_tasks_with_one_footer_navigation_legend() {
        let _locale = crate::test_support::lock_locale();
        let mut state = State::new();
        assert!(state.task_list);
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|frame| renderer::render(frame, &mut state))
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains(t!("agent_center.tasks_title").as_ref()));
        assert_eq!(text.matches("F4").count(), 1);
        assert!(!text.contains("Next responsibility"));
        assert!(!state.composer_focused());
    }

    #[test]
    fn opening_hydrates_binding_and_preserves_independent_drafts_and_history() {
        let mut state = State::new();
        let a = opened("a", "conversation-a", "Paused");
        let b = opened("b", "conversation-b", "Running");
        state.open_work(&a).unwrap();
        state.editor_view_mut().replace_draft("Draft A".into());
        state.view_mut().scroll = 7;
        state.view_mut().follow = false;
        state.open_work(&b).unwrap();
        state.editor_view_mut().replace_draft("Draft B".into());
        state.open_work(&a).unwrap();
        assert_eq!(state.context.conversation_id, "conversation-a");
        assert_eq!(state.context.console_session_id, "console-a");
        assert_eq!(state.context.context_version, 3);
        assert_eq!(state.editor_view().unwrap().draft, "Draft A");
        assert_eq!(state.view().unwrap().messages.len(), 1);
        assert_eq!(state.view().unwrap().scroll, 7);
        assert_eq!(state.views[&Some("b".into())].draft, "Draft B");
        state.append(Some("a".into()), &json!({"kind":"ConversationItem",
            "id":"foreign","conversationId":"conversation-b","role":"assistant","text":"Other task"}));
        assert_eq!(
            state.view().unwrap().messages.len(),
            1,
            "Foreign conversation events cannot enter task history"
        );
        state.append(
            Some("a".into()),
            &json!({"kind":"IntakeRequest",
            "id":"foreign-question","conversationId":"conversation-b","status":"Open",
            "question":"Other task question","version":1}),
        );
        assert!(!state
            .view()
            .unwrap()
            .records
            .contains_key("foreign-question"));
        let operation = commands::conversation("Follow up".into(), &state.context, false).unwrap();
        assert_eq!(operation.params["conversationId"], "conversation-a");
        assert_eq!(operation.params["context"]["selectedWorkId"], "a");
        assert_ne!(operation.params["context"]["scope"], "Global");
    }

    #[test]
    fn a_question_opened_from_tasks_accepts_paste_without_submitting_or_stealing_a_draft() {
        let mut state = State::new();
        state.chat.replace_draft("Preserved new-task draft".into());
        let original = state.selection();
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        state
            .activate(
                workflow::Choice::Question(
                    json!({
                        "kind":"DecisionRequest","id":"question-a","version":8,"status":"Open",
                        "question":"Explain the decision","responseSchema":{"type":"string"}
                    }),
                    Some("a".into()),
                ),
                &jobs,
            )
            .unwrap();
        handle_paste(&mut state, "First line\r\nSecond line");
        let form = state.form.as_ref().unwrap();
        assert_eq!(form.work.as_deref(), Some("a"));
        assert_eq!(form.fields[0].draft, "First line\nSecond line");
        assert_eq!(
            form.operation().unwrap().if_match,
            vec![json!({
                "kind":"DecisionRequest","id":"question-a","version":8
            })]
        );
        assert_eq!(state.chat.draft, "Preserved new-task draft");
        assert_eq!(state.selection(), original);
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn background_submission_reply_and_question_stay_with_their_bound_task() {
        for revise_draft in [false, true] {
            let mut state = State::new();
            state
                .open_work(&opened("a", "conversation-a", "Ready"))
                .unwrap();
            let (jobs, mut receiver) = mpsc::unbounded_channel();
            state.editor_view_mut().replace_draft("Question A".into());
            submit(&mut state, &jobs).unwrap();
            let job = receiver.try_recv().unwrap();
            let JobKind::Send(operation) = job.kind else {
                panic!("expected conversation submission")
            };
            if revise_draft {
                state
                    .editor_view_mut()
                    .replace_draft("Changed A draft".into());
            }
            state
                .open_work(&opened("b", "conversation-b", "Ready"))
                .unwrap();
            state
                .editor_view_mut()
                .replace_draft("Unsent B draft".into());
            let selected_b = state.selection();
            state.event(json!({"eventId":"a-reply","workId":"a","changes":[{
                "subject":{"kind":"ConversationItem","id":"a-answer","version":1},
                "view":{"kind":"ConversationItem","id":"a-answer","version":1,
                    "conversationId":"conversation-a","role":"assistant","text":"Answer for A"}
            },{
                "subject":{"kind":"IntakeRequest","id":"a-question","version":1},
                "view":{"kind":"IntakeRequest","id":"a-question","version":1,
                    "conversationId":"conversation-a","status":"Open","question":"A clarification"}
            }]}));
            state.receive(Update {
                work: job.work,
                input: job.input,
                conversation: job.conversation,
                result: Ok(Outcome::Mutation {
                    command_id: operation.command_id,
                    result: Ok(json!({"status":"pending","data":{"intakeTurnId":"a-turn"}})),
                }),
            });
            assert_eq!(state.selection(), selected_b);
            assert_eq!(state.editor_view().unwrap().draft, "Unsent B draft");
            assert_eq!(state.view().unwrap().messages.len(), 1);
            assert!(!state.view().unwrap().records.contains_key("a-question"));
            let a = &state.views[&Some("a".into())];
            assert_eq!(a.messages.last().unwrap()["text"], "Answer for A");
            assert!(a.records.contains_key("a-question"));
            assert_eq!(a.draft, if revise_draft { "Changed A draft" } else { "" });
            assert!(state.actions().items.iter().any(
                |(_, choice)| matches!(choice, workflow::Choice::Question(question, Some(owner))
                    if owner == "a" && question["id"] == "a-question")
            ));
        }
    }

    #[test]
    fn continue_is_service_operation_without_a_fake_assistant_message() {
        let mut state = State::new();
        state
            .open_work(&opened("a", "conversation-a", "Paused"))
            .unwrap();
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        for text in ["Continue work", "继续任务"] {
            state.editor_view_mut().replace_draft(text.into());
            submit(&mut state, &jobs).unwrap();
            let JobKind::Send(operation) = receiver.try_recv().unwrap().kind else {
                panic!("expected send")
            };
            assert_eq!(operation.method, "work.continue");
            assert_eq!(
                operation.if_match,
                vec![json!({"kind":"Work","id":"a","version":7})]
            );
            assert!(operation.params.get("restartSession").is_none());
            assert!(!state.thinking());
            assert_eq!(state.view().unwrap().messages.len(), 1);
            state.settle_mutation(&operation.command_id, &Ok(json!({"status":"pending"})));
        }
        state
            .editor_view_mut()
            .replace_draft("Please add a chart".into());
        submit(&mut state, &jobs).unwrap();
        let JobKind::Send(operation) = receiver.try_recv().unwrap().kind else {
            panic!("expected send")
        };
        assert_eq!(operation.method, "conversation.submit");
        assert_eq!(operation.params["context"]["selectedWorkId"], "a");
    }

    #[test]
    fn legacy_chat_requires_executor_claim_confirmation_and_preserves_unsent_input() {
        let _locale = crate::test_support::lock_locale();
        let mut state = State::new();
        let mut legacy = opened("a", "conversation-a", "Ready");
        legacy["workView"]["continuation"]["canClaimExecutor"] = json!(true);
        state.open_work(&legacy).unwrap();
        state
            .editor_view_mut()
            .replace_draft("Start the project server".into());
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        submit(&mut state, &jobs).unwrap();
        assert!(
            receiver.try_recv().is_err(),
            "Migration is never implicitly approved"
        );
        assert_eq!(
            state.editor_view().unwrap().draft,
            "Start the project server"
        );
        let pending = state.pending.as_ref().unwrap();
        assert_eq!(pending.operation.method, "work.claim_executor");
        assert_eq!(
            pending.operation.if_match,
            vec![json!({"kind":"Work","id":"a","version":7})]
        );
        assert!(pending.operation.params.get("restartSession").is_none());
        let preview =
            projection::confirmation(&pending.preview, &pending.operation, &pending.target_label);
        assert!(preview.contains(t!("agent_center.task_claim_executor_confirm").as_ref()));
        handle_key(&mut state, KeyCode::Enter, KeyModifiers::NONE, &jobs).unwrap();
        assert!(
            receiver.try_recv().is_err(),
            "Plain Enter is not migration consent"
        );
        handle_key(&mut state, KeyCode::Enter, KeyModifiers::CONTROL, &jobs).unwrap();
        let JobKind::Send(operation) = receiver.try_recv().unwrap().kind else {
            panic!("expected confirmed claim")
        };
        assert_eq!(operation.method, "work.claim_executor");
        let mut claimed = opened("a", "conversation-a", "Ready");
        claimed["workView"]["work"]["version"] = json!(8);
        claimed["workView"]["work"]["executionMode"] = json!("WorkExecutor");
        claimed["workView"]["continuation"]["canClaimExecutor"] = json!(false);
        state.response(
            Some("a".into()),
            String::new(),
            Ok(json!({"status":"ok","data":claimed})),
            Some(operation.command_id),
        );
        assert_eq!(
            state.editor_view().unwrap().draft,
            "Start the project server"
        );
        submit(&mut state, &jobs).unwrap();
        let JobKind::Send(operation) = receiver.try_recv().unwrap().kind else {
            panic!("expected work message")
        };
        assert_eq!(operation.method, "conversation.submit");
        assert_eq!(operation.params["context"]["selectedWorkId"], "a");
        assert_eq!(operation.params["conversationId"], "conversation-a");
    }

    #[test]
    fn unknown_open_reconciles_the_original_command_then_hydrates() {
        let mut state = State::new();
        let id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        state
            .activate(workflow::Choice::SelectWork(id.into()), &jobs)
            .unwrap();
        let job = receiver.try_recv().unwrap();
        let JobKind::Send(operation) = job.kind else {
            panic!("expected open")
        };
        assert_eq!(operation.method, "work.open");
        assert!(operation.if_match.is_empty());
        state.response(
            Some(id.into()),
            String::new(),
            Err(anyhow::anyhow!("lost receipt")),
            Some(operation.command_id.clone()),
        );
        assert!(state.task_list);
        state
            .activate(
                workflow::Choice::Reconcile(operation.command_id.clone()),
                &jobs,
            )
            .unwrap();
        let JobKind::Send(retry) = receiver.try_recv().unwrap().kind else {
            panic!("expected retry")
        };
        assert_eq!(operation_preview(&retry), operation_preview(&operation));
        state.response(
            Some(id.into()),
            String::new(),
            Ok(json!({"status":"ok","data":opened(id,"durable-chat","Paused")})),
            Some(operation.command_id),
        );
        assert_eq!(state.context.conversation_id, "durable-chat");
        assert!(!state.task_list);
        assert!(state.mutations.is_empty());
    }

    #[test]
    fn unavailable_requires_explicit_restart_confirmation_and_terminal_states_never_resume() {
        let mut state = State::new();
        state
            .open_work(&opened("a", "conversation-a", "Unavailable"))
            .unwrap();
        assert!(state.continue_operation(false).is_err());
        assert!(
            state.continue_operation(true).is_err(),
            "Unavailable alone does not authorize reconstruction"
        );
        let mut recovery = opened("a", "conversation-a", "Unavailable");
        recovery["workView"]["continuation"]["canRestartSession"] = json!(true);
        state.open_work(&recovery).unwrap();
        let operation = state.continue_operation(true).unwrap();
        assert_eq!(operation.params["restartSession"], true);
        assert!(operation.confirmation);
        let (jobs, mut receiver) = mpsc::unbounded_channel();
        state
            .activate(workflow::Choice::Control(operation), &jobs)
            .unwrap();
        assert!(state.pending.is_some());
        assert!(receiver.try_recv().is_err());
        for lifecycle in ["Draft", "Completed", "Cancelled"] {
            let mut data = opened("a", "conversation-a", "Ready");
            data["workView"]["work"]["lifecycle"] = json!(lifecycle);
            data["workView"]["work"]["version"] = json!(8);
            state.open_work(&data).unwrap();
            assert!(state.continue_operation(false).is_err());
            assert!(state.continue_operation(true).is_err());
        }
    }
}
