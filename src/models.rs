use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceInfo {
    pub client: String,
    pub label: Option<String>,
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppendRequest {
    pub schema_version: String,
    pub source: SourceInfo,
    pub mode: String,
    pub snippet: String,
    pub cursor_hint: Option<String>,
    pub max_chars: Option<usize>,
    pub metadata: serde_json::Value,
}

impl AppendRequest {
    pub fn validate(&self) -> crate::error::ValidationResult<()> {
        if self.schema_version != "1.0" {
            return Err(crate::error::ValidationError::InvalidSchemaVersion {
                version: self.schema_version.clone(),
            });
        }

        if self.source.client.is_empty() {
            return Err(crate::error::ValidationError::MissingField {
                field: "source.client".to_string(),
            });
        }

        if self.snippet.trim().is_empty() {
            return Err(crate::error::ValidationError::EmptySnippet);
        }

        if self.mode != "append" {
            return Err(crate::error::ValidationError::MissingField {
                field: "mode".to_string(),
            });
        }

        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SinkConnection {
    pub id: Uuid,
    pub registered_at: DateTime<Utc>,
    pub profile_id: Option<String>,
    pub capabilities: Vec<String>,
    pub version: String,
}

impl SinkConnection {
    pub fn new(profile_id: Option<String>, capabilities: Vec<String>, version: String) -> Self {
        Self {
            id: Uuid::new_v4(),
            registered_at: Utc::now(),
            profile_id,
            capabilities,
            version,
        }
    }

    pub fn has_capability(&self, capability: &str) -> bool {
        self.capabilities.contains(&capability.to_string())
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HealthResponse {
    pub ok: bool,
    pub timestamp: DateTime<Utc>,
    pub version: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_append_request_validation() {
        let mut request = AppendRequest {
            schema_version: "1.0".to_string(),
            source: SourceInfo {
                client: "test".to_string(),
                label: None,
                path: None,
            },
            mode: "append".to_string(),
            snippet: "test content".to_string(),
            cursor_hint: None,
            max_chars: None,
            metadata: serde_json::json!({}),
        };

        assert!(request.validate().is_ok());

        request.snippet = "".to_string();
        assert!(request.validate().is_err());
    }
}
