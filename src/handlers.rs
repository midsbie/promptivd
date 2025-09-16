use std::sync::Arc;

use axum::extract::ws::WebSocketUpgrade;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use axum::{response::IntoResponse, Json};
use chrono::Utc;
use tracing::{info, warn};
use uuid::Uuid;

use crate::config::ServerConfig;
use crate::error::AppError;
use crate::models::{HealthResponse, InsertTextRequest};
use crate::websocket::{AckResponse, AckStatus, SinkManager};

#[derive(Clone)]
pub struct AppState {
    pub sink_manager: Arc<SinkManager>,
    pub config: ServerConfig,
}

pub async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        ok: true,
        timestamp: Utc::now(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

pub async fn append_job(
    State(state): State<AppState>,
    Json(payload): Json<InsertTextRequest>,
) -> Result<impl IntoResponse, AppError> {
    // Validate payload size
    let payload_size = serde_json::to_string(&payload)?.len();
    if payload_size > state.config.max_job_bytes {
        return Err(AppError::PayloadTooLarge {
            size: payload_size,
            max: state.config.max_job_bytes,
        });
    }

    // Validate the request
    payload.validate().map_err(|e| AppError::InvalidRequest {
        reason: format!("Validation error: {:?}", e),
    })?;

    // Check if sink is required and available
    if state.config.require_sink && !state.sink_manager.has_active_sink() {
        warn!("Job rejected: no sink available and require_sink is true");
        return Err(AppError::NoSink);
    }

    let job_id = Uuid::new_v4().to_string();
    let ack = state
        .sink_manager
        .dispatch_job(
            job_id.clone(),
            payload.text.clone(),
            payload.placement.clone(),
            payload.metadata.clone(),
        )
        .await?;

    let AckResponse { status, error } = ack;

    match status {
        AckStatus::Ok => {
            info!(job_id = %job_id, "Job delivered successfully");
            let response = serde_json::json!({
                "job_id": job_id,
                "status": "ok",
            });
            Ok((StatusCode::OK, Json(response)))
        }
        AckStatus::Retry | AckStatus::Failed => {
            warn!(job_id = %job_id, status = ?status, error = ?error, "Sink reported failure");
            let response = serde_json::json!({
                "job_id": job_id,
                "status": status.to_string(),
                "error": error,
            });
            Ok((StatusCode::BAD_GATEWAY, Json(response)))
        }
    }
}

pub async fn websocket_handler(State(state): State<AppState>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| async move {
        if let Err(e) = state.sink_manager.handle_websocket(socket).await {
            warn!("WebSocket error: {}", e);
        }
    })
}
// Error handling for HTTP responses
impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            AppError::NoSink => (StatusCode::SERVICE_UNAVAILABLE, self.to_string()),
            AppError::InvalidRequest { .. } => (StatusCode::BAD_REQUEST, self.to_string()),
            AppError::PayloadTooLarge { .. } => (StatusCode::PAYLOAD_TOO_LARGE, self.to_string()),
            AppError::Serialization(_) => (StatusCode::BAD_REQUEST, "Invalid JSON".to_string()),
            AppError::Config(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Configuration error".to_string(),
            ),
            AppError::DispatchTimeout { .. } => (StatusCode::GATEWAY_TIMEOUT, self.to_string()),
            _ => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal server error".to_string(),
            ),
        };

        let body = serde_json::json!({
            "error": message,
            "timestamp": Utc::now(),
        });

        (status, Json(body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::SourceInfo;

    fn create_test_state() -> AppState {
        let config = ServerConfig::default();
        let sink_manager = Arc::new(SinkManager::new(config.clone()));

        AppState {
            sink_manager,
            config,
        }
    }

    fn create_test_request() -> InsertTextRequest {
        InsertTextRequest {
            schema_version: "1.0".to_string(),
            source: SourceInfo {
                client: "test".to_string(),
                label: Some("Test Client".to_string()),
                path: Some("/test/file.txt".to_string()),
            },
            text: "Test content".to_string(),
            placement: None,
            metadata: serde_json::json!({"test": "data"}),
        }
    }

    #[tokio::test]
    async fn test_health_endpoint() {
        let response = health().await;
        assert!(response.0.ok);
    }

    #[tokio::test]
    async fn test_append_job_no_sink() {
        let state = create_test_state();
        let request = create_test_request();

        let result = append_job(State(state), Json(request)).await;

        assert!(matches!(result, Err(AppError::NoSink)));
    }

    #[tokio::test]
    async fn test_payload_too_large() {
        let mut state = create_test_state();
        state.config.max_job_bytes = 10; // Very small limit

        let request = create_test_request();

        let result = append_job(State(state), Json(request)).await;

        assert!(matches!(result, Err(AppError::PayloadTooLarge { .. })));
    }
}
