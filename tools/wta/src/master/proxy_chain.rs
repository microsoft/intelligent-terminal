//! Canonical ACP proxy-chain construction for one pooled agent instance.

use agent_client_protocol as acp;
use agent_client_protocol_conductor::{ConductorImpl, ProxiesAndAgent};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Direction {
    ClientToAgent,
    AgentToClient,
}

impl Direction {
    const fn as_str(self) -> &'static str {
        match self {
            Self::ClientToAgent => "client_to_agent",
            Self::AgentToClient => "agent_to_client",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MessageKind {
    Request,
    Response,
    Notification,
}

impl MessageKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Response => "response",
            Self::Notification => "notification",
        }
    }
}

trait DispatchObserver: Clone + Send + Sync + 'static {
    fn observe(&self, direction: Direction, kind: MessageKind, method: &str);
}

#[derive(Clone, Copy)]
struct LogDispatch;

impl DispatchObserver for LogDispatch {
    fn observe(&self, direction: Direction, kind: MessageKind, method: &str) {
        tracing::trace!(
            target: "proxy_chain",
            direction = direction.as_str(),
            kind = kind.as_str(),
            method,
            "forwarding ACP dispatch"
        );
    }
}

struct TracingProxy<O> {
    observer: O,
}

impl<O> TracingProxy<O> {
    const fn new(observer: O) -> Self {
        Self { observer }
    }
}

impl<O: DispatchObserver> acp::ConnectTo<acp::Conductor> for TracingProxy<O> {
    async fn connect_to(self, client: impl acp::ConnectTo<acp::Proxy>) -> Result<(), acp::Error> {
        let initialize_observer = self.observer.clone();
        acp::Proxy
            .builder()
            .name("wta-tracing-proxy")
            .on_receive_request_from(
                acp::Client,
                async move |request: acp::schema::InitializeProxyRequest, responder, cx| {
                    initialize_observer.observe(
                        Direction::ClientToAgent,
                        MessageKind::Request,
                        responder.method(),
                    );
                    cx.send_request_to(acp::Agent, request.initialize)
                        .forward_response_to(responder)
                },
                acp::on_receive_request!(),
            )
            .with_handler(TraceAndForward {
                observer: self.observer,
            })
            .connect_to(client)
            .await
    }
}

struct TraceAndForward<O> {
    observer: O,
}

impl<O: DispatchObserver> acp::HandleDispatchFrom<acp::Conductor> for TraceAndForward<O> {
    async fn handle_dispatch_from(
        &mut self,
        message: acp::Dispatch,
        connection: acp::ConnectionTo<acp::Conductor>,
    ) -> Result<acp::Handled<acp::Dispatch>, acp::Error> {
        use acp::util::MatchDispatchFrom;

        MatchDispatchFrom::new(message, &connection)
            .if_message_from(acp::Client, async |message: acp::Dispatch| {
                self.observe(Direction::ClientToAgent, &message);
                connection.send_proxied_message_to(acp::Agent, message)?;
                Ok(acp::Handled::Yes)
            })
            .await
            .if_message_from(acp::Agent, async |message: acp::Dispatch| {
                self.observe(Direction::AgentToClient, &message);
                connection.send_proxied_message_to(acp::Client, message)?;
                Ok(acp::Handled::Yes)
            })
            .await
            .done()
    }

    fn describe_chain(&self) -> impl std::fmt::Debug {
        "TraceAndForward"
    }
}

impl<O: DispatchObserver> TraceAndForward<O> {
    fn observe(&self, direction: Direction, message: &acp::Dispatch) {
        let kind = match message {
            acp::Dispatch::Request(..) => MessageKind::Request,
            acp::Dispatch::Response(..) => MessageKind::Response,
            acp::Dispatch::Notification(..) => MessageKind::Notification,
        };
        self.observer.observe(direction, kind, message.method());
    }
}

/// The ACP conductor isolates spawned component task completion. Mirror the
/// final component's lifetime so WTA can still reap a dead pooled agent.
struct FinalAgentWatch<C> {
    component: C,
    terminated: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
}

impl<C> acp::ConnectTo<acp::Client> for FinalAgentWatch<C>
where
    C: acp::ConnectTo<acp::Client>,
{
    async fn connect_to(self, client: impl acp::ConnectTo<acp::Agent>) -> Result<(), acp::Error> {
        let result = self.component.connect_to(client).await;
        let _ = self.terminated.send(());
        let _ = self.release.await;
        result
    }
}

pub(super) struct ProxyChain {
    conductor: ConductorImpl<acp::Agent>,
    final_agent_terminated: tokio::sync::oneshot::Receiver<()>,
    release_final_agent: tokio::sync::oneshot::Sender<()>,
}

impl acp::ConnectTo<acp::Client> for ProxyChain {
    async fn connect_to(self, client: impl acp::ConnectTo<acp::Agent>) -> Result<(), acp::Error> {
        let Self {
            conductor,
            mut final_agent_terminated,
            release_final_agent,
        } = self;
        let conductor = acp::ConnectTo::<acp::Client>::connect_to(conductor, client);
        tokio::pin!(conductor);

        tokio::select! {
            biased;
            result = &mut conductor => result,
            _ = &mut final_agent_terminated => {
                tracing::debug!(
                    target: "proxy_chain",
                    "final agent component terminated; draining proxy chain"
                );
                // The SDK queues final-agent dispatches separately from component
                // completion. Keep driving the conductor briefly so a response
                // written immediately before EOF reaches its waiting caller.
                let result = match tokio::time::timeout(
                    std::time::Duration::from_millis(100),
                    &mut conductor,
                )
                .await
                {
                    Ok(result) => result,
                    Err(_) => Err(acp::Error::internal_error()),
                };
                let _ = release_final_agent.send(());
                result
            }
        }
    }
}

/// Build the canonical chain with an explicit wire-observing pass-through proxy.
///
/// The proxy records only routing metadata (direction, message kind, and method);
/// message parameters, results, and metadata are forwarded unchanged and are
/// never written to the trace.
pub(super) fn traced(final_agent: impl acp::ConnectTo<acp::Client> + 'static) -> ProxyChain {
    with_observer(final_agent, LogDispatch)
}

fn with_observer(
    final_agent: impl acp::ConnectTo<acp::Client> + 'static,
    observer: impl DispatchObserver,
) -> ProxyChain {
    let (terminated, final_agent_terminated) = tokio::sync::oneshot::channel();
    let (release_final_agent, release) = tokio::sync::oneshot::channel();
    let final_agent = FinalAgentWatch {
        component: final_agent,
        terminated,
        release,
    };
    let conductor = ConductorImpl::new_agent(
        "wta-master",
        ProxiesAndAgent::new(final_agent).proxy(TracingProxy::new(observer)),
    );
    ProxyChain {
        conductor,
        final_agent_terminated,
        release_final_agent,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use acp::schema::v1::{
        AgentCapabilities, ContentChunk, InitializeRequest, InitializeResponse, Meta,
        NewSessionRequest, NewSessionResponse, PermissionOption, PermissionOptionId,
        PermissionOptionKind, RequestPermissionOutcome, RequestPermissionRequest,
        RequestPermissionResponse, SelectedPermissionOutcome, SessionId, SessionNotification,
        SessionUpdate, ToolCallId, ToolCallUpdate, ToolCallUpdateFields,
    };
    use serde_json::Value;

    use super::*;
    use crate::protocol::acp::conn;

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct TraceEvent {
        direction: Direction,
        kind: MessageKind,
        method: String,
    }

    #[derive(Clone, Default)]
    struct RecordingObserver(Arc<Mutex<Vec<TraceEvent>>>);

    impl DispatchObserver for RecordingObserver {
        fn observe(&self, direction: Direction, kind: MessageKind, method: &str) {
            self.0.lock().unwrap().push(TraceEvent {
                direction,
                kind,
                method: method.to_string(),
            });
        }
    }

    struct RecordingAgent {
        new_session_meta: Arc<Mutex<Option<Meta>>>,
        permission_selected: Arc<Mutex<bool>>,
    }

    impl acp::ConnectTo<acp::Client> for RecordingAgent {
        async fn connect_to(
            self,
            client: impl acp::ConnectTo<acp::Agent>,
        ) -> Result<(), acp::Error> {
            let new_session_meta = self.new_session_meta;
            let permission_selected = self.permission_selected;

            acp::Agent
                .builder()
                .name("recording-final-agent")
                .on_receive_request(
                    async |request: InitializeRequest, responder, _cx| {
                        responder.respond(
                            InitializeResponse::new(request.protocol_version)
                                .agent_capabilities(AgentCapabilities::new()),
                        )
                    },
                    acp::on_receive_request!(),
                )
                .on_receive_request(
                    move |request: NewSessionRequest,
                          responder: acp::Responder<NewSessionResponse>,
                          cx: acp::ConnectionTo<acp::Client>| {
                        let new_session_meta = Arc::clone(&new_session_meta);
                        let permission_selected = Arc::clone(&permission_selected);
                        async move {
                            *new_session_meta.lock().unwrap() = request.meta;
                            let session_id = SessionId::new("proxy-test-session");
                            cx.send_notification(SessionNotification::new(
                                session_id.clone(),
                                SessionUpdate::AgentMessageChunk(ContentChunk::new("hello".into())),
                            ))?;

                            tokio::task::spawn_local(async move {
                                let permission = RequestPermissionRequest::new(
                                    session_id.clone(),
                                    ToolCallUpdate::new(
                                        ToolCallId::new("proxy-test-tool"),
                                        ToolCallUpdateFields::new().title("Run test"),
                                    ),
                                    vec![PermissionOption::new(
                                        PermissionOptionId::new("allow-once"),
                                        "Allow once",
                                        PermissionOptionKind::AllowOnce,
                                    )],
                                );
                                let selected =
                                    cx.send_request(permission).block_task().await.is_ok_and(
                                        |response| {
                                            matches!(
                                                response.outcome,
                                                RequestPermissionOutcome::Selected(_)
                                            )
                                        },
                                    );
                                *permission_selected.lock().unwrap() = selected;
                                responder.respond(NewSessionResponse::new(session_id))
                            });
                            Ok(())
                        }
                    },
                    acp::on_receive_request!(),
                )
                .connect_to(client)
                .await
        }
    }

    #[test]
    fn tracing_proxy_forwards_both_directions_and_preserves_meta() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        local.block_on(&rt, async {
            let observer = RecordingObserver::default();
            let events = Arc::clone(&observer.0);
            let new_session_meta = Arc::new(Mutex::new(None));
            let permission_selected = Arc::new(Mutex::new(false));
            let notification_seen = Arc::new(Mutex::new(false));
            let final_agent = RecordingAgent {
                new_session_meta: Arc::clone(&new_session_meta),
                permission_selected: Arc::clone(&permission_selected),
            };
            let conductor = with_observer(final_agent, observer);

            let builder = acp::Client
                .builder()
                .name("proxy-test-client")
                .on_receive_request(
                    async |_request: RequestPermissionRequest, responder, _cx| {
                        responder.respond(RequestPermissionResponse::new(
                            RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                                PermissionOptionId::new("allow-once"),
                            )),
                        ))
                    },
                    acp::on_receive_request!(),
                )
                .on_receive_notification(
                    {
                        let notification_seen = Arc::clone(&notification_seen);
                        move |_notification: SessionNotification, _cx| {
                            let notification_seen = Arc::clone(&notification_seen);
                            async move {
                                *notification_seen.lock().unwrap() = true;
                                Ok(())
                            }
                        }
                    },
                    acp::on_receive_notification!(),
                );
            let (link, handle_io) = conn::spawn_client_component(builder, conductor);
            let io_task = tokio::task::spawn_local(handle_io);

            link.initialize(InitializeRequest::new(acp::schema::ProtocolVersion::V1))
                .await
                .expect("proxy chain should initialize");

            let mut expected_meta = Meta::new();
            expected_meta.insert(
                "traceparent".into(),
                Value::String("00-test-trace-context-01".into()),
            );
            link.new_session(NewSessionRequest::new("/tmp").meta(expected_meta.clone()))
                .await
                .expect("session/new should cross the proxy");

            assert_eq!(*new_session_meta.lock().unwrap(), Some(expected_meta));
            assert!(*notification_seen.lock().unwrap());
            assert!(*permission_selected.lock().unwrap());

            let events = events.lock().unwrap();
            for expected in [
                (
                    Direction::ClientToAgent,
                    MessageKind::Request,
                    "_proxy/initialize",
                ),
                (
                    Direction::AgentToClient,
                    MessageKind::Response,
                    "initialize",
                ),
                (
                    Direction::ClientToAgent,
                    MessageKind::Request,
                    "session/new",
                ),
                (
                    Direction::AgentToClient,
                    MessageKind::Notification,
                    "session/update",
                ),
                (
                    Direction::AgentToClient,
                    MessageKind::Request,
                    "session/request_permission",
                ),
                (
                    Direction::ClientToAgent,
                    MessageKind::Response,
                    "session/request_permission",
                ),
                (
                    Direction::AgentToClient,
                    MessageKind::Response,
                    "session/new",
                ),
            ] {
                assert!(
                    events.iter().any(|event| {
                        event.direction == expected.0
                            && event.kind == expected.1
                            && event.method == expected.2
                    }),
                    "missing trace event {expected:?}; observed {events:?}"
                );
            }

            io_task.abort();
        });
    }

    #[test]
    fn tracing_proxy_chain_resolves_when_final_agent_dies() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        local.block_on(&rt, async {
            use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

            let (near, far) = tokio::io::duplex(64 * 1024);
            let (near_r, near_w) = tokio::io::split(near);
            let final_agent = conn::byte_streams(near_w.compat_write(), near_r.compat());
            let conductor = traced(final_agent);
            let builder = acp::Client
                .builder()
                .name("proxy-lifecycle-client")
                .on_receive_request(
                    |_request: acp::schema::v1::AgentRequest,
                     responder: acp::Responder<serde_json::Value>,
                     _cx| async move {
                        responder.respond_with_error(acp::Error::method_not_found())
                    },
                    acp::on_receive_request!(),
                );
            let (link, handle_io) = conn::spawn_client_component(builder, conductor);
            let handle_task = tokio::task::spawn_local(handle_io);
            let (far_r, far_w) = tokio::io::split(far);
            let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
            let shutdown_tx = Arc::new(Mutex::new(Some(shutdown_tx)));
            let agent_task = tokio::task::spawn_local(async move {
                acp::Agent
                    .builder()
                    .name("proxy-lifecycle-final-agent")
                    .on_receive_request(
                        |request: InitializeRequest,
                         responder: acp::Responder<InitializeResponse>,
                         _cx| async move {
                            responder.respond(InitializeResponse::new(request.protocol_version))
                        },
                        acp::on_receive_request!(),
                    )
                    .on_receive_request(
                        {
                            let shutdown_tx = Arc::clone(&shutdown_tx);
                            move |_request: NewSessionRequest,
                                  responder: acp::Responder<NewSessionResponse>,
                                  _cx| {
                                let shutdown_tx = Arc::clone(&shutdown_tx);
                                async move {
                                    responder.respond(NewSessionResponse::new(SessionId::new(
                                        "last-response",
                                    )))?;
                                    if let Some(shutdown_tx) = shutdown_tx.lock().unwrap().take() {
                                        tokio::task::spawn_local(async move {
                                            tokio::time::sleep(std::time::Duration::from_millis(
                                                10,
                                            ))
                                            .await;
                                            let _ = shutdown_tx.send(());
                                        });
                                    }
                                    Ok(())
                                }
                            }
                        },
                        acp::on_receive_request!(),
                    )
                    .connect_with(
                        acp::ByteStreams::new(far_w.compat_write(), far_r.compat()),
                        async move |_cx| {
                            let _ = shutdown_rx.await;
                            Ok(())
                        },
                    )
                    .await
            });

            link.initialize(InitializeRequest::new(acp::schema::ProtocolVersion::V1))
                .await
                .expect("proxy chain should initialize the final agent");
            link.new_session(NewSessionRequest::new("/tmp"))
                .await
                .expect("the response immediately before EOF must be delivered");
            agent_task
                .await
                .expect("final-agent task should not panic")
                .expect("final-agent task should close cleanly");

            let result = tokio::time::timeout(std::time::Duration::from_secs(3), handle_task)
                .await
                .expect("final-agent EOF must resolve the tracing proxy chain")
                .expect("proxy-chain task should not panic");
            assert!(result.is_err(), "final-agent EOF must fail the proxy chain");
        });
    }
}
