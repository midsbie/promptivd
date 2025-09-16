use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot, RwLock};
use tokio::time::{interval, Instant};
use tracing::{error, info, warn};

use crate::config::ServerConfig;
use crate::error::{AppError, AppResult};
use crate::models::{Placement, SinkConnection, SourceInfo, TargetSpec};

const SCHEMA_VERSION: &str = "1.0";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SinkMessage {
    Register {
        schema_version: String,
        version: String,
        capabilities: Vec<String>,
        providers: Vec<String>,
    },
    Ack {
        schema_version: String,
        id: String,
        status: AckStatus,
        error: Option<String>,
    },
    Pong {
        schema_version: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RelayMessage {
    InsertText {
        schema_version: String,
        id: String,
        payload: InsertTextPayload,
    },
    Ping {
        schema_version: String,
    },
    Policy {
        schema_version: String,
        supersede_on_register: bool,
        max_job_bytes: usize,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InsertTextPayload {
    pub text: String,
    pub placement: Option<Placement>,
    pub source: SourceInfo,
    pub target: Option<TargetSpec>,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AckStatus {
    Ok,
    Retry,
    Failed,
}

impl std::fmt::Display for AckStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AckStatus::Ok => write!(f, "ok"),
            AckStatus::Retry => write!(f, "retry"),
            AckStatus::Failed => write!(f, "failed"),
        }
    }
}

#[derive(Debug)]
pub struct SinkManager {
    active_sink: Arc<RwLock<Option<ActiveSink>>>,
    config: ServerConfig,
    connected: Arc<AtomicBool>,
}

#[derive(Debug)]
struct ActiveSink {
    connection: SinkConnection,
    message_sender: mpsc::UnboundedSender<RelayMessage>,
    ack_waiters: Arc<RwLock<HashMap<String, oneshot::Sender<AckResponse>>>>,
}

#[derive(Debug, Clone)]
pub struct AckResponse {
    pub status: AckStatus,
    pub error: Option<String>,
}

impl SinkManager {
    pub fn new(config: ServerConfig) -> Self {
        Self {
            active_sink: Arc::new(RwLock::new(None)),
            config,
            connected: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn has_active_sink(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    pub async fn dispatch_job(
        &self,
        job_id: String,
        text: String,
        placement: Option<Placement>,
        source: SourceInfo,
        target: Option<TargetSpec>,
        metadata: serde_json::Value,
    ) -> AppResult<AckResponse> {
        let sink_guard = self.active_sink.read().await;
        let sink = match sink_guard.as_ref() {
            Some(sink) => sink,
            None => return Err(AppError::NoSink),
        };

        let (response_tx, response_rx) = oneshot::channel();

        {
            let mut waiters = sink.ack_waiters.write().await;
            waiters.insert(job_id.clone(), response_tx);
        }

        let job_msg = RelayMessage::InsertText {
            schema_version: SCHEMA_VERSION.to_string(),
            id: job_id.clone(),
            payload: InsertTextPayload {
                text,
                placement,
                source,
                target,
                metadata,
            },
        };

        if sink.message_sender.send(job_msg).is_err() {
            let mut waiters = sink.ack_waiters.write().await;
            waiters.remove(&job_id);
            return Err(AppError::NoSink);
        }

        let timeout = self.config.dispatch_timeout;
        drop(sink_guard);

        match tokio::time::timeout(timeout, response_rx).await {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(_)) => Err(AppError::NoSink),
            Err(_) => {
                if let Some(active) = self.active_sink.read().await.as_ref() {
                    let mut waiters = active.ack_waiters.write().await;
                    waiters.remove(&job_id);
                }
                Err(AppError::DispatchTimeout {
                    timeout_ms: timeout.as_millis() as u64,
                })
            }
        }
    }

    pub async fn handle_websocket(&self, socket: WebSocket) -> AppResult<()> {
        let (mut sink_tx, mut sink_rx) = socket.split();
        let (message_tx, mut message_rx) = mpsc::unbounded_channel::<RelayMessage>();

        // Handle incoming messages from sink
        let active_sink_clone = Arc::clone(&self.active_sink);
        let config = self.config.clone();
        let connected = Arc::clone(&self.connected);

        let receive_task = tokio::spawn(async move {
            let mut ping_interval = interval(config.websocket_ping_interval);
            let mut missed_pings = 0u32;
            let mut registered = false;
            let mut awaiting_pong = false;
            let mut last_ping: Option<Instant> = None;

            loop {
                tokio::select! {
                    // Handle incoming WebSocket messages
                    msg = sink_rx.next() => {
                        match msg {
                            Some(Ok(Message::Text(text))) => {
                                match serde_json::from_str::<SinkMessage>(&text) {
                                    Ok(sink_msg) => {
                                        match Self::handle_sink_message(
                                            sink_msg,
                                            &active_sink_clone,
                                            &message_tx,
                                            &config,
                                            &mut registered,
                                            &mut missed_pings,
                                            &mut awaiting_pong,
                                        ).await {
                                            Ok(()) => {
                                                if registered {
                                                    connected.store(true, Ordering::Relaxed);
                                                }
                                                // Treat any inbound valid message as liveness if awaiting and within timeout
                                                if awaiting_pong {
                                                    if let Some(lp) = last_ping {
                                                        if lp.elapsed() <= config.websocket_pong_timeout {
                                                            awaiting_pong = false;
                                                            missed_pings = 0;
                                                        }
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                error!("Failed to handle sink message: {}", e);
                                                break;
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        warn!("Invalid message from sink: {}", e);
                                    }
                                }
                            }
                            Some(Ok(Message::Close(_))) => {
                                info!("Sink closed connection");
                                break;
                            }
                            Some(Err(e)) => {
                                error!("WebSocket error: {}", e);
                                break;
                            }
                            None => {
                                info!("Sink disconnected");
                                break;
                            }
                            _ => {
                                // Ignore other message types (binary, ping, pong)
                            }
                        }
                    }

                    // Send ping messages
                    _ = ping_interval.tick() => {
                        if registered {
                            // If awaiting pong, check timeout and possibly count as missed
                            if awaiting_pong {
                                if let Some(lp) = last_ping {
                                    if lp.elapsed() >= config.websocket_pong_timeout {
                                        missed_pings += 1;
                                        warn!("PONG timeout, missed pings: {}", missed_pings);
                                        if missed_pings >= config.websocket_max_missed_pings {
                                            warn!("Sink missed {} pings, disconnecting", missed_pings);
                                            break;
                                        }
                                        // Allow sending next ping below
                                        awaiting_pong = false;
                                    } else {
                                        // Still waiting within timeout; do not send another ping
                                        continue;
                                    }
                                }
                            }

                            // Send a new ping only when not awaiting
                            if !awaiting_pong {
                                let ping_msg = RelayMessage::Ping { schema_version: SCHEMA_VERSION.to_string() };
                                if message_tx.send(ping_msg).is_err() { break; }
                                awaiting_pong = true;
                                last_ping = Some(Instant::now());
                            }
                        }
                    }

                    // No separate sleep_until timeout branch; timeout checked on tick
                }
            }

            // Cleanup on disconnect
            let mut active_sink = active_sink_clone.write().await;
            if let Some(sink) = active_sink.take() {
                // Drain any pending waiters with Retry so dispatchers can react
                sink.drain_waiters(AckStatus::Retry, "Sink disconnected")
                    .await;
                info!("Cleaned up sink connection: {}", sink.connection.id);
            }
            connected.store(false, Ordering::Relaxed);
        });

        // Handle outgoing messages to sink
        let send_task = tokio::spawn(async move {
            while let Some(msg) = message_rx.recv().await {
                match serde_json::to_string(&msg) {
                    Ok(json) => {
                        if sink_tx.send(Message::Text(json)).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        error!("Failed to serialize message: {}", e);
                        break;
                    }
                }
            }
        });

        // Wait for either task to complete
        tokio::select! {
            _ = receive_task => {},
            _ = send_task => {},
        }

        Ok(())
    }

    async fn handle_sink_message(
        message: SinkMessage,
        active_sink: &Arc<RwLock<Option<ActiveSink>>>,
        message_tx: &mpsc::UnboundedSender<RelayMessage>,
        config: &ServerConfig,
        registered: &mut bool,
        missed_pings: &mut u32,
        awaiting_pong: &mut bool,
    ) -> AppResult<()> {
        match message {
            SinkMessage::Register {
                schema_version,
                version,
                capabilities,
                providers,
            } => {
                if schema_version != SCHEMA_VERSION {
                    return Err(AppError::SinkRegistrationFailed {
                        reason: format!("Unsupported schema version: {}", schema_version),
                    });
                }

                let connection = SinkConnection::new(capabilities, providers, version);

                let sink = ActiveSink {
                    connection,
                    message_sender: message_tx.clone(),
                    ack_waiters: Arc::new(RwLock::new(HashMap::new())),
                };

                // Send policy message first; only publish sink after success
                let policy_msg = RelayMessage::Policy {
                    schema_version: SCHEMA_VERSION.to_string(),
                    supersede_on_register: config.supersede_on_register,
                    max_job_bytes: config.max_job_bytes,
                };
                message_tx
                    .send(policy_msg)
                    .map_err(|_| AppError::SinkRegistrationFailed {
                        reason: "Failed to deliver policy".into(),
                    })?;

                let mut active = active_sink.write().await;
                if active.is_some() && !config.supersede_on_register {
                    return Err(AppError::SinkRegistrationFailed {
                        reason: "A sink is already registered".to_string(),
                    });
                }

                // Drain existing waiters if superseding
                if let Some(existing) = active.take() {
                    existing
                        .drain_waiters(AckStatus::Retry, "Superseded by new sink")
                        .await;
                    info!("Superseded existing sink: {}", existing.connection.id);
                }

                *active = Some(sink);

                info!("Registered new sink");

                *registered = true;
            }

            SinkMessage::Ack {
                id, status, error, ..
            } => {
                let response = AckResponse { status, error };

                if let Some(sink) = active_sink.read().await.as_ref() {
                    let mut waiters = sink.ack_waiters.write().await;
                    if let Some(sender) = waiters.remove(&id) {
                        let _ = sender.send(response);
                    }
                }
            }

            SinkMessage::Pong { .. } => {
                // Pong received - reset missed pings and clear awaiting state
                *missed_pings = 0;
                *awaiting_pong = false;
                info!("Received PONG from sink, reset missed ping counter");
            }
        }

        Ok(())
    }
}

impl ActiveSink {
    async fn drain_waiters(&self, status: AckStatus, reason: &str) {
        let mut waiters = self.ack_waiters.write().await;
        let entries: Vec<_> = waiters.drain().collect();
        drop(waiters);
        for (_, sender) in entries {
            let _ = sender.send(AckResponse {
                status: status.clone(),
                error: Some(reason.to_string()),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{SessionDirective, SourceInfo, TargetSpec};

    #[test]
    fn test_sink_message_serialization() {
        let register_msg = SinkMessage::Register {
            schema_version: "1.0".to_string(),
            version: "1.0.0".to_string(),
            capabilities: vec!["insert".to_string()],
            providers: vec!["chatgpt".to_string(), "claude".to_string()],
        };

        let json = serde_json::to_string(&register_msg).unwrap();
        let deserialized: SinkMessage = serde_json::from_str(&json).unwrap();

        match deserialized {
            SinkMessage::Register {
                version, providers, ..
            } => {
                assert_eq!(version, "1.0.0");
                assert_eq!(providers, vec!["chatgpt", "claude"]);
            }
            _ => panic!("Wrong message type"),
        }
    }

    #[test]
    fn test_relay_message_serialization() {
        let job_msg = RelayMessage::InsertText {
            schema_version: "1.0".to_string(),
            id: "test-job".to_string(),
            payload: InsertTextPayload {
                text: "test content".to_string(),
                placement: Some(Placement::Bottom),
                source: SourceInfo {
                    client: "cli".to_string(),
                    label: Some("CLI".to_string()),
                    path: Some("/tmp/file".to_string()),
                },
                target: Some(TargetSpec {
                    provider: Some("chatgpt".to_string()),
                    session_directive: Some(SessionDirective::ReuseOrCreate),
                }),
                metadata: serde_json::json!({"key": "value"}),
            },
        };

        let json = serde_json::to_string(&job_msg).unwrap();
        let deserialized: RelayMessage = serde_json::from_str(&json).unwrap();

        match deserialized {
            RelayMessage::InsertText { id, payload, .. } => {
                assert_eq!(id, "test-job");
                assert_eq!(payload.placement, Some(Placement::Bottom));
                assert_eq!(payload.source.client, "cli");
                assert_eq!(
                    payload.target.as_ref().and_then(|t| t.provider.clone()),
                    Some("chatgpt".to_string())
                );
                assert_eq!(payload.metadata, serde_json::json!({"key": "value"}));
            }
            _ => panic!("Wrong message type"),
        }
    }
}
