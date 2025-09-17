use std::sync::Arc;

use axum::{
    extract::DefaultBodyLimit,
    http::{HeaderValue, Method},
    routing::{get, post},
    Router,
};
use clap::Parser;
use tokio::signal;
use tower_http::{
    cors::CorsLayer,
    timeout::TimeoutLayer,
    trace::{DefaultMakeSpan, DefaultOnResponse, TraceLayer},
};
use tracing::{error, info, level_filters::LevelFilter};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use promptivd::config::{AppConfig, ConfigError, LogFormat};
use promptivd::error::{AppError, AppResult};
use promptivd::handlers::AppState;
use promptivd::websocket::SinkManager;

#[derive(Parser)]
#[command(name = "promptivd")]
#[command(about = "A Rust daemon that relays insert-text jobs to a browser extension")]
#[command(version)]
struct Cli {
    /// Configuration file path
    #[arg(short, long, value_name = "FILE")]
    config: Option<std::path::PathBuf>,

    /// Log level (trace, debug, info, warn, error)
    #[arg(short, long, value_name = "LEVEL")]
    log_level: Option<String>,

    /// Bind address
    #[arg(short, long, value_name = "ADDR")]
    bind: Option<String>,

    /// Generate default configuration file
    #[arg(long)]
    init_config: bool,

    /// Validate configuration and exit
    #[arg(long)]
    validate: bool,
}

#[tokio::main]
async fn main() -> AppResult<()> {
    let cli = Cli::parse();

    // Handle init-config command
    if cli.init_config {
        return handle_init_config().await;
    }

    // Load configuration
    let mut config = AppConfig::from_file(cli.config.as_ref()).map_err(AppError::Config)?;

    // Override config with CLI arguments
    if let Some(log_level) = cli.log_level {
        config.log_level = log_level;
    }

    if let Some(bind_addr) = cli.bind {
        config.server.bind_addr = bind_addr.parse().map_err(|e| {
            AppError::Config(ConfigError::Message(format!("Invalid bind address: {}", e)))
        })?;
    }

    // Validate configuration
    config.validate().map_err(AppError::Config)?;

    if cli.validate {
        println!("Configuration is valid");
        return Ok(());
    }

    // Initialize logging
    init_logging(&config)?;

    info!("Starting promptivd version {}", env!("CARGO_PKG_VERSION"));
    info!("Configuration loaded from: {:?}", cli.config);
    info!("Server binding to: {}", config.server.bind_addr);

    // Initialize components
    let sink_manager = Arc::new(SinkManager::new(config.server.clone()));

    // Create application state
    let state = AppState {
        sink_manager: Arc::clone(&sink_manager),
        config: config.server.clone(),
    };

    // Create router
    let app = create_router(state, &config);

    // Create server
    let listener = tokio::net::TcpListener::bind(&config.server.bind_addr)
        .await
        .map_err(AppError::Io)?;

    info!("Server started on {}", config.server.bind_addr);

    // Start server with graceful shutdown
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(AppError::Io)?;

    info!("Server shutdown complete");
    Ok(())
}

fn create_router(state: AppState, config: &AppConfig) -> Router {
    Router::new()
        // API routes
        .route("/v1/health", get(promptivd::handlers::health))
        .route("/v1/providers", get(promptivd::handlers::list_providers))
        .route("/v1/insert", post(promptivd::handlers::insert_job))
        // WebSocket route for sink connections
        .route("/v1/sink/ws", get(promptivd::handlers::websocket_handler))
        .with_state(state)
        // Request size limit
        .layer(DefaultBodyLimit::max(config.server.max_job_bytes))
        // Request timeout
        .layer(TimeoutLayer::new(std::time::Duration::from_secs(30)))
        // CORS
        .layer(create_cors_layer())
        // Tracing
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::new().level(tracing::Level::INFO))
                .on_response(DefaultOnResponse::new().level(tracing::Level::INFO)),
        )
}

fn create_cors_layer() -> CorsLayer {
    CorsLayer::new()
        .allow_origin("http://localhost:3000".parse::<HeaderValue>().unwrap())
        .allow_origin("http://127.0.0.1:3000".parse::<HeaderValue>().unwrap())
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([
            axum::http::header::CONTENT_TYPE,
            axum::http::header::AUTHORIZATION,
        ])
        .max_age(std::time::Duration::from_secs(86400))
}

fn init_logging(config: &AppConfig) -> AppResult<()> {
    let log_level = config.log_level.parse::<LevelFilter>().map_err(|e| {
        promptivd::error::AppError::Config(ConfigError::Message(format!(
            "Invalid log level '{}': {}",
            config.log_level, e
        )))
    })?;

    let env_filter = EnvFilter::builder()
        .with_default_directive(log_level.into())
        .from_env()
        .map_err(|e| {
            promptivd::error::AppError::Config(ConfigError::Message(format!(
                "Failed to parse log filter: {}",
                e
            )))
        })?;

    match config.log_format {
        LogFormat::Json => {
            tracing_subscriber::registry()
                .with(env_filter)
                .with(tracing_subscriber::fmt::layer().json())
                .init();
        }
        LogFormat::Pretty => {
            tracing_subscriber::registry()
                .with(env_filter)
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_target(false)
                        .compact(),
                )
                .init();
        }
    }

    Ok(())
}

async fn handle_init_config() -> AppResult<()> {
    match AppConfig::create_default_config_file() {
        Ok(path) => {
            println!("Created default configuration file at: {}", path.display());
            Ok(())
        }
        Err(e) => {
            error!("Failed to create configuration file: {}", e);
            Err(promptivd::error::AppError::Io(e))
        }
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    tokio::select! {
        _ = ctrl_c => {
            info!("Received Ctrl+C, starting graceful shutdown");
        },
        _ = terminate => {
            info!("Received SIGTERM, starting graceful shutdown");
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;
    use tower::ServiceExt;

    fn create_test_config() -> AppConfig {
        AppConfig::default()
    }

    fn create_test_state() -> AppState {
        let config = create_test_config();
        let sink_manager = Arc::new(SinkManager::new(config.server.clone()));

        AppState {
            sink_manager,
            config: config.server,
        }
    }

    #[tokio::test]
    async fn test_health_endpoint() {
        let config = create_test_config();
        let state = create_test_state();
        let app = create_router(state, &config);

        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/v1/health")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_providers_endpoint_no_sink() {
        let config = create_test_config();
        let state = create_test_state();
        let app = create_router(state, &config);

        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/v1/providers")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn test_config_validation() {
        let config = create_test_config();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_log_level_parsing() {
        let mut config = create_test_config();
        config.log_level = "debug".to_string();

        let level = config.log_level.parse::<LevelFilter>().unwrap();
        assert_eq!(level, LevelFilter::DEBUG);
    }
}
