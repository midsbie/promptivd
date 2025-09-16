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
pub struct InsertTextRequest {
    pub schema_version: String,
    pub source: SourceInfo,
    pub text: String,
    pub placement: Option<Placement>,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Placement {
    TOP,
    BOTTOM,
    CURSOR,
}

impl InsertTextRequest {
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

        if self.text.trim().is_empty() {
            return Err(crate::error::ValidationError::EmptySnippet);
        }

        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SinkConnection {
    pub id: Uuid,
    pub registered_at: DateTime<Utc>,
    pub capabilities: Vec<String>,
    pub version: String,
}

impl SinkConnection {
    pub fn new(capabilities: Vec<String>, version: String) -> Self {
        Self {
            id: Uuid::new_v4(),
            registered_at: Utc::now(),
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
    fn test_insert_text_request_validation() {
        let mut request = InsertTextRequest {
            schema_version: "1.0".to_string(),
            source: SourceInfo {
                client: "test".to_string(),
                label: None,
                path: None,
            },
            text: "test content".to_string(),
            placement: None,
            metadata: serde_json::json!({}),
        };

        assert!(request.validate().is_ok());

        request.text = "".to_string();
        assert!(request.validate().is_err());
    }
}
