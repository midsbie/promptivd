use std::time::Duration;

use clap::{Parser, ValueEnum};
use futures_util::{SinkExt, StreamExt};
use tokio::time::sleep;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use tracing::{error, info, warn};

use promptivd::websocket::{AckStatus, RelayMessage, SinkMessage};

const SCHEMA_VERSION: &str = "1.0";
const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Parser)]
#[command(name = "promptivs")]
#[command(about = "Example sink client for promptivd")]
#[command(version)]
struct Cli {
    /// WebSocket URL for the relay sink endpoint
    #[arg(long, default_value = "ws://127.0.0.1:8787/v1/sink/ws")]
    server: String,

    /// Ack behaviour for incoming jobs
    #[arg(long, value_enum, default_value_t = AckMode::Ok)]
    ack_mode: AckMode,

    /// Optional tab url to include in ACK responses
    #[arg(long)]
    tab_url: Option<String>,

    /// Artificial processing delay before sending ACK (milliseconds)
    #[arg(long, default_value_t = 0u64)]
    ack_delay_ms: u64,

    /// Set logging verbosity (trace, debug, info, warn, error)
    #[arg(long, default_value = "info")]
    log_level: String,

    /// Capabilities to advertise (may be passed multiple times)
    #[arg(long = "capability", value_name = "NAME", default_values_t = vec![String::from("append")])]
    capabilities: Vec<String>,
}

#[derive(Debug, Copy, Clone, ValueEnum)]
enum AckMode {
    Ok,
    Retry,
    Failed,
}

impl std::fmt::Display for AckMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AckMode::Ok => write!(f, "ok"),
            AckMode::Retry => write!(f, "retry"),
            AckMode::Failed => write!(f, "failed"),
        }
    }
}

impl From<AckMode> for AckStatus {
    fn from(value: AckMode) -> Self {
        match value {
            AckMode::Ok => AckStatus::Ok,
            AckMode::Retry => AckStatus::Retry,
            AckMode::Failed => AckStatus::Failed,
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    init_logging(&cli.log_level)?;

    info!(target: "promptivs", version = CLIENT_VERSION, "Starting sink client");
    connect_and_run(cli).await
}

async fn connect_and_run(cli: Cli) -> anyhow::Result<()> {
    let (ws_stream, _) = connect_async(cli.server.as_str()).await?;
    info!(server = %cli.server, "Connected");

    let (mut ws_sender, mut ws_receiver) = ws_stream.split();

    let register = SinkMessage::Register {
        schema_version: SCHEMA_VERSION.to_string(),
        version: CLIENT_VERSION.to_string(),
        capabilities: cli.capabilities.clone(),
    };

    ws_sender
        .send(Message::Text(serde_json::to_string(&register)?))
        .await?;
    info!("Sent REGISTER message");

    while let Some(msg) = ws_receiver.next().await {
        match msg {
            Ok(Message::Text(text)) => match serde_json::from_str::<RelayMessage>(&text) {
                Ok(RelayMessage::Ping { .. }) => {
                    info!("Received PING");
                    let pong = SinkMessage::Pong {
                        schema_version: SCHEMA_VERSION.to_string(),
                    };
                    ws_sender
                        .send(Message::Text(serde_json::to_string(&pong)?))
                        .await?;
                    info!("Sent PONG");
                }
                Ok(RelayMessage::Policy {
                    supersede_on_register,
                    max_job_bytes,
                    ..
                }) => {
                    info!(
                        "Received POLICY: supersede_on_register={}, max_job_bytes={}",
                        supersede_on_register, max_job_bytes
                    );
                }
                Ok(RelayMessage::InsertText { id, payload, .. }) => {
                    info!(
                        job_id = id,
                        text = %payload.text,
                        placement = ?payload.placement,
                        source = ?payload.source,
                        metadata = ?payload.metadata,
                        "Received INSERT_TEXT"
                    );

                    if cli.ack_delay_ms > 0 {
                        sleep(Duration::from_millis(cli.ack_delay_ms)).await;
                    }

                    let status: AckStatus = cli.ack_mode.into();
                    let error = match status {
                        AckStatus::Ok => None,
                        AckStatus::Retry => Some("Simulated retry".to_string()),
                        AckStatus::Failed => Some("Simulated failure".to_string()),
                    };
                    let status_for_log = status.clone();
                    let ack = SinkMessage::Ack {
                        schema_version: SCHEMA_VERSION.to_string(),
                        id,
                        status,
                        error,
                    };

                    ws_sender
                        .send(Message::Text(serde_json::to_string(&ack)?))
                        .await?;
                    info!("Sent ACK with status {:?}", status_for_log);
                }
                Err(err) => {
                    warn!("Failed to parse relay message: {}", err);
                }
            },
            Ok(Message::Ping(payload)) => {
                info!("Received websocket ping");
                ws_sender.send(Message::Pong(payload)).await?;
            }
            Ok(Message::Close(frame)) => {
                info!("WebSocket closed: {:?}", frame);
                let _ = ws_sender.send(Message::Close(frame)).await;
                break;
            }
            Ok(Message::Binary(bytes)) => {
                warn!(
                    "Ignoring binary frame of {} bytes (unsupported by protocol)",
                    bytes.len()
                );
            }
            Ok(other) => warn!("Ignoring unsupported frame: {:?}", other),
            Err(err) => {
                error!("WebSocket error: {}", err);
                break;
            }
        }
    }

    info!("Sink loop terminated");
    Ok(())
}

fn init_logging(level: &str) -> anyhow::Result<()> {
    use tracing::level_filters::LevelFilter;
    let level_filter = level.parse::<LevelFilter>()?;
    tracing_subscriber::fmt()
        .with_max_level(level_filter)
        .with_target(true)
        .with_thread_ids(false)
        .init();
    Ok(())
}
