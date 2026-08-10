//! Long-lived Remote Agent client runtime shared by CLI and future local IPC.

use std::time::Duration;

use ahp::{Client, ClientConfig, ClientEventStream, SessionSubscription, SubscriptionEvent};
use ahp_types::actions::ActionEnvelope;
use ahp_types::commands::{ListSessionsParams, ListSessionsResult, ReconnectResult};
use ahp_types::notifications::AuthRequiredParams;
use ahp_types::state::SessionSummary;
use anyhow::{Context, Result};
use serde::Serialize;

use super::mirror::RemoteClientMirror;
use super::relay::LocalFramedTransport;

const ROOT: &str = ahp_types::ROOT_RESOURCE_URI;

#[derive(Clone, Debug)]
pub(crate) struct RemoteClientConfig {
    pub(crate) address: std::net::SocketAddr,
    pub(crate) client_id: String,
    pub(crate) auth_token: String,
    pub(crate) reconnect_initial_delay: Duration,
    pub(crate) reconnect_max_delay: Duration,
    pub(crate) event_limit: Option<usize>,
}

impl Default for RemoteClientConfig {
    fn default() -> Self {
        Self {
            address: "127.0.0.1:8787"
                .parse()
                .expect("static loopback address is valid"),
            client_id: "wta-remote-client".to_string(),
            auth_token: String::new(),
            reconnect_initial_delay: Duration::from_secs(1),
            reconnect_max_delay: Duration::from_secs(30),
            event_limit: None,
        }
    }
}

struct ConnectedClient {
    client: Client,
    events: ClientEventStream,
    _root_subscription: SessionSubscription,
    recovery: &'static str,
    server_seq: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub(crate) enum RemoteClientEvent {
    Connected {
        address: String,
        recovery: &'static str,
        server_seq: u64,
        sessions: Vec<SessionSummary>,
    },
    Action {
        envelope: ActionEnvelope,
    },
    CatalogueChanged {
        reason: &'static str,
        sessions: Vec<SessionSummary>,
    },
    AuthRequired {
        channel: String,
        params: AuthRequiredParams,
    },
    Disconnected {
        address: String,
        retry_in_ms: u64,
        error: String,
    },
}

pub(crate) async fn list_catalogue(
    address: std::net::SocketAddr,
    client_id: &str,
    auth_token: &str,
) -> Result<Vec<SessionSummary>> {
    let connection = connect(
        address,
        client_id,
        auth_token,
        None,
        &mut RemoteClientMirror::default(),
    )
    .await?;
    let sessions = fetch_catalogue(&connection.client).await?;
    connection.client.shutdown().await;
    Ok(sessions)
}

pub(crate) async fn watch<F>(config: RemoteClientConfig, mut emit: F) -> Result<()>
where
    F: FnMut(&RemoteClientEvent) -> Result<()>,
{
    anyhow::ensure!(
        !config.client_id.trim().is_empty(),
        "remote client ID must not be empty"
    );
    anyhow::ensure!(
        !config.auth_token.is_empty(),
        "remote client authentication token must not be empty"
    );
    anyhow::ensure!(
        config.event_limit.is_none_or(|limit| limit > 0),
        "remote client event limit must be greater than zero"
    );
    anyhow::ensure!(
        config.reconnect_initial_delay <= config.reconnect_max_delay,
        "remote client reconnect initial delay must not exceed its maximum delay"
    );

    let mut mirror = RemoteClientMirror::default();
    let mut emitted = 0_usize;
    let mut initialized = false;
    let mut reconnect_delay = config.reconnect_initial_delay;

    loop {
        let last_seen = initialized.then_some(mirror.last_seen_server_seq);
        let connection = connect(
            config.address,
            &config.client_id,
            &config.auth_token,
            last_seen,
            &mut mirror,
        )
        .await;
        let mut connection = match connection {
            Ok(connection) => connection,
            Err(error) => {
                tracing::warn!(
                    target = "remote_agent_client",
                    address = %config.address,
                    ?error,
                    "remote client connection failed"
                );
                if emit_event(
                    &mut emit,
                    &RemoteClientEvent::Disconnected {
                        address: config.address.to_string(),
                        retry_in_ms: reconnect_delay.as_millis() as u64,
                        error: format!("{error:#}"),
                    },
                    &mut emitted,
                    config.event_limit,
                )? {
                    return Ok(());
                }
                tokio::time::sleep(reconnect_delay).await;
                reconnect_delay = next_reconnect_delay(reconnect_delay, config.reconnect_max_delay);
                continue;
            }
        };
        initialized = true;
        reconnect_delay = config.reconnect_initial_delay;

        let sessions = match fetch_catalogue(&connection.client).await {
            Ok(sessions) => sessions,
            Err(error) => {
                connection.client.shutdown().await;
                if emit_disconnected(&mut emit, &config, reconnect_delay, error, &mut emitted)? {
                    return Ok(());
                }
                tokio::time::sleep(reconnect_delay).await;
                reconnect_delay = next_reconnect_delay(reconnect_delay, config.reconnect_max_delay);
                continue;
            }
        };
        mirror.refetch_catalogue(sessions.clone());
        if emit_event(
            &mut emit,
            &RemoteClientEvent::Connected {
                address: config.address.to_string(),
                recovery: connection.recovery,
                server_seq: connection.server_seq,
                sessions,
            },
            &mut emitted,
            config.event_limit,
        )? {
            connection.client.shutdown().await;
            return Ok(());
        }

        let mut reconnect_error = "remote host closed the transport".to_string();
        loop {
            let event = tokio::select! {
                event = connection.events.recv() => event,
                signal = tokio::signal::ctrl_c() => {
                    signal.context("listen for remote client shutdown signal")?;
                    connection.client.shutdown().await;
                    return Ok(());
                }
            };
            let Some(event) = event else {
                break;
            };

            match event.event {
                SubscriptionEvent::Action(envelope) => {
                    if emit_event(
                        &mut emit,
                        &RemoteClientEvent::Action { envelope },
                        &mut emitted,
                        config.event_limit,
                    )? {
                        connection.client.shutdown().await;
                        return Ok(());
                    }
                }
                SubscriptionEvent::SessionAdded(_) => {
                    match refresh_and_emit(
                        &connection.client,
                        &mut mirror,
                        "sessionAdded",
                        &mut emit,
                        &mut emitted,
                        config.event_limit,
                    )
                    .await
                    {
                        Ok(true) => {
                            connection.client.shutdown().await;
                            return Ok(());
                        }
                        Ok(false) => {}
                        Err(error) => {
                            reconnect_error = format!("{error:#}");
                            break;
                        }
                    }
                }
                SubscriptionEvent::SessionRemoved(_) => {
                    match refresh_and_emit(
                        &connection.client,
                        &mut mirror,
                        "sessionRemoved",
                        &mut emit,
                        &mut emitted,
                        config.event_limit,
                    )
                    .await
                    {
                        Ok(true) => {
                            connection.client.shutdown().await;
                            return Ok(());
                        }
                        Ok(false) => {}
                        Err(error) => {
                            reconnect_error = format!("{error:#}");
                            break;
                        }
                    }
                }
                SubscriptionEvent::SessionSummaryChanged(_) => {
                    match refresh_and_emit(
                        &connection.client,
                        &mut mirror,
                        "sessionSummaryChanged",
                        &mut emit,
                        &mut emitted,
                        config.event_limit,
                    )
                    .await
                    {
                        Ok(true) => {
                            connection.client.shutdown().await;
                            return Ok(());
                        }
                        Ok(false) => {}
                        Err(error) => {
                            reconnect_error = format!("{error:#}");
                            break;
                        }
                    }
                }
                SubscriptionEvent::AuthRequired(params) => {
                    tracing::warn!(
                        target = "remote_agent_client",
                        channel = %event.channel,
                        "remote host requested resource authentication"
                    );
                    if emit_event(
                        &mut emit,
                        &RemoteClientEvent::AuthRequired {
                            channel: event.channel,
                            params,
                        },
                        &mut emitted,
                        config.event_limit,
                    )? {
                        connection.client.shutdown().await;
                        return Ok(());
                    }
                }
                _ => {}
            }
        }

        connection.client.shutdown().await;
        if emit_event(
            &mut emit,
            &RemoteClientEvent::Disconnected {
                address: config.address.to_string(),
                retry_in_ms: reconnect_delay.as_millis() as u64,
                error: reconnect_error,
            },
            &mut emitted,
            config.event_limit,
        )? {
            return Ok(());
        }
        tokio::time::sleep(reconnect_delay).await;
        reconnect_delay = next_reconnect_delay(reconnect_delay, config.reconnect_max_delay);
    }
}

async fn connect(
    address: std::net::SocketAddr,
    client_id: &str,
    auth_token: &str,
    last_seen_server_seq: Option<u64>,
    mirror: &mut RemoteClientMirror,
) -> Result<ConnectedClient> {
    let transport = LocalFramedTransport::connect(address, auth_token).await?;
    let client = Client::connect(
        transport,
        ClientConfig {
            subscription_buffer: 256,
            ..ClientConfig::default()
        },
    )
    .await
    .context("start Remote Agent client")?;
    // Register the receiver before initialize/reconnect so no live event can
    // fall into the response-to-receiver creation window.
    let events = client.events();

    let (recovery, server_seq) = if let Some(last_seen_server_seq) = last_seen_server_seq {
        let result = client
            .reconnect(
                client_id.to_string(),
                i64::try_from(last_seen_server_seq).unwrap_or(i64::MAX),
                vec![ROOT.to_string()],
            )
            .await
            .context("reconnect Remote Agent client")?;
        let recovery = match result {
            ReconnectResult::Replay(result) => {
                for action in &result.actions {
                    let _ = mirror.apply_action(action);
                }
                "replay"
            }
            ReconnectResult::Snapshot(result) => {
                for snapshot in &result.snapshots {
                    mirror.apply_snapshot(snapshot);
                }
                "snapshot"
            }
        };
        (recovery, mirror.last_seen_server_seq)
    } else {
        let result = client
            .initialize(
                client_id.to_string(),
                vec![ahp_types::PROTOCOL_VERSION.to_string()],
                vec![ROOT.to_string()],
            )
            .await
            .context("initialize Remote Agent client")?;
        for snapshot in &result.snapshots {
            mirror.apply_snapshot(snapshot);
        }
        (
            "initialize",
            u64::try_from(result.server_seq).unwrap_or_default(),
        )
    };

    let root_subscription = client.attach_subscription(ROOT).await;
    Ok(ConnectedClient {
        client,
        events,
        _root_subscription: root_subscription,
        recovery,
        server_seq,
    })
}

async fn fetch_catalogue(client: &Client) -> Result<Vec<SessionSummary>> {
    let mut sessions = Vec::new();
    let mut cursor = None;
    loop {
        let result: ListSessionsResult = client
            .request(
                "listSessions",
                ListSessionsParams {
                    channel: ROOT.to_string(),
                    limit: Some(100),
                    cursor,
                },
            )
            .await
            .context("list remote sessions")?;
        sessions.extend(result.items);
        let Some(next_cursor) = result.next_cursor else {
            break;
        };
        cursor = Some(next_cursor);
    }
    Ok(sessions)
}

async fn refresh_and_emit<F>(
    client: &Client,
    mirror: &mut RemoteClientMirror,
    reason: &'static str,
    emit: &mut F,
    emitted: &mut usize,
    event_limit: Option<usize>,
) -> Result<bool>
where
    F: FnMut(&RemoteClientEvent) -> Result<()>,
{
    let sessions = fetch_catalogue(client).await?;
    mirror.refetch_catalogue(sessions.clone());
    emit_event(
        emit,
        &RemoteClientEvent::CatalogueChanged { reason, sessions },
        emitted,
        event_limit,
    )
}

fn emit_event<F>(
    emit: &mut F,
    event: &RemoteClientEvent,
    emitted: &mut usize,
    event_limit: Option<usize>,
) -> Result<bool>
where
    F: FnMut(&RemoteClientEvent) -> Result<()>,
{
    emit(event)?;
    *emitted = emitted.saturating_add(1);
    Ok(event_limit.is_some_and(|limit| *emitted >= limit))
}

fn emit_disconnected<F>(
    emit: &mut F,
    config: &RemoteClientConfig,
    reconnect_delay: Duration,
    error: anyhow::Error,
    emitted: &mut usize,
) -> Result<bool>
where
    F: FnMut(&RemoteClientEvent) -> Result<()>,
{
    tracing::warn!(
        target = "remote_agent_client",
        address = %config.address,
        ?error,
        "remote client connection lost"
    );
    emit_event(
        emit,
        &RemoteClientEvent::Disconnected {
            address: config.address.to_string(),
            retry_in_ms: reconnect_delay.as_millis() as u64,
            error: format!("{error:#}"),
        },
        emitted,
        config.event_limit,
    )
}

fn next_reconnect_delay(current: Duration, maximum: Duration) -> Duration {
    current.saturating_mul(2).min(maximum)
}
