use super::*;
use crate::linux_hooks::{Request, Response};

#[derive(Default)]
pub(super) struct LinuxHooksView {
    pub pipe: Option<String>,
    next_request: u64,
    pending: HashMap<String, (u64, tokio::task::AbortHandle)>,
    last_poll: Option<std::time::Instant>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_session_view_does_not_poll_hooks_or_alter_chat() {
        let _locale = crate::test_support::lock_locale();
        let mut app = crate::app::tests::test_app();
        app.linux_hooks_view.pipe = Some("unused-test-pipe".into());
        app.current_tab_mut().current_view = View::Chat;
        app.poll_linux_hooks();
        assert!(app.linux_hooks_view.pending.is_empty());
    }

    #[tokio::test]
    async fn closing_view_rejects_late_hook_results() {
        let _locale = crate::test_support::lock_locale();
        let mut app = crate::app::tests::test_app();
        let task = tokio::spawn(std::future::pending::<()>());
        app.linux_hooks_view
            .pending
            .insert(DEFAULT_TAB_ID.into(), (1, task.abort_handle()));
        app.cancel_linux_hooks(DEFAULT_TAB_ID);
        app.handle_linux_hooks_snapshot(
            DEFAULT_TAB_ID,
            1,
            "copilot",
            Ok(Response {
                enabled: true,
                targets: vec![crate::linux_hooks::TargetStatus {
                    target: crate::linux_hooks::Target::Wsl {
                        distro: "Ubuntu".into(),
                        user: "alice".into(),
                    },
                    providers: Vec::new(),
                }],
            }),
        );
        assert!(app.current_tab().agents_view.linux_hooks.is_empty());
        assert_eq!(app.current_tab().current_view, View::Chat);
    }
}

impl App {
    pub(super) fn poll_linux_hooks(&mut self) {
        if self
            .linux_hooks_view
            .last_poll
            .is_some_and(|last| last.elapsed() < std::time::Duration::from_secs(2))
        {
            return;
        }
        self.linux_hooks_view.last_poll = Some(std::time::Instant::now());
        let tabs: Vec<_> = self
            .tab_sessions
            .iter()
            .filter(|(_, tab)| tab.current_view == View::Agents && tab.pane_open)
            .map(|(id, _)| id.clone())
            .collect();
        for tab in tabs {
            self.request_linux_hooks(&tab);
        }
    }

    pub(super) fn request_linux_hooks(&mut self, tab_id: &str) {
        if self.linux_hooks_view.pending.contains_key(tab_id)
            || !crate::linux_hooks::PROVIDERS.contains(&self.current_agent_id.as_str())
        {
            return;
        }
        let (Some(pipe), Some(tx)) = (self.linux_hooks_view.pipe.clone(), self.event_tx.clone())
        else {
            return;
        };
        self.linux_hooks_view.next_request = self.linux_hooks_view.next_request.wrapping_add(1);
        let request_id = self.linux_hooks_view.next_request;
        let tab = tab_id.to_owned();
        let cli = self.current_agent_id.clone();
        let pane_id = self
            .tab_sessions
            .get(tab_id)
            .and_then(|tab| tab.agents_view.linux_hooks_source_pane.clone());
        let task = tokio::task::spawn_local(async move {
            let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                let request = crate::linux_hooks::build_request(&Request::Snapshot {
                    tab_id: tab.clone(),
                    cli: cli.clone(),
                    pane_id,
                })?;
                let result = crate::cli::sessions::request_from_master(Some(pipe), request).await?;
                serde_json::from_str::<Response>(result.0.get()).map_err(anyhow::Error::from)
            })
            .await
            .map_err(|error| error.to_string())
            .and_then(|result| result.map_err(|error| error.to_string()));
            if tx
                .send(AppEvent::LinuxHooksSnapshot {
                    tab_id: tab,
                    request_id,
                    cli,
                    result,
                })
                .is_err()
            {
                tracing::debug!(target: "linux_hooks", "Session view closed before hook status arrived");
            }
        });
        self.linux_hooks_view
            .pending
            .insert(tab_id.to_owned(), (request_id, task.abort_handle()));
    }

    pub(super) fn cancel_linux_hooks(&mut self, tab_id: &str) {
        if let Some((_, task)) = self.linux_hooks_view.pending.remove(tab_id) {
            task.abort();
        }
        if let Some(tab) = self.tab_sessions.get_mut(tab_id) {
            tab.agents_view.linux_hooks.clear();
        }
    }

    pub(super) fn handle_linux_hooks_snapshot(
        &mut self,
        tab_id: &str,
        request_id: u64,
        cli: &str,
        result: std::result::Result<Response, String>,
    ) {
        if !self
            .linux_hooks_view
            .pending
            .get(tab_id)
            .is_some_and(|(id, _)| *id == request_id)
        {
            return;
        }
        self.linux_hooks_view.pending.remove(tab_id);
        let Some(tab) = self.tab_sessions.get_mut(tab_id) else {
            return;
        };
        if tab.current_view != View::Agents || self.current_agent_id != cli {
            return;
        }
        match result {
            Ok(response) => {
                tab.agents_view.linux_hooks = if response.enabled {
                    response.targets
                } else {
                    Vec::new()
                };
            }
            Err(error) => {
                tracing::warn!(target: "linux_hooks", %error, "Could not read session hook installation state");
                tab.agents_view.linux_hooks.clear();
            }
        }
    }
}
