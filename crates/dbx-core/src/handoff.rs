use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::sql_safety::{OperationClass, RiskLevel};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum HandoffStatus {
    Queued,
    Shown,
    Approved,
    Rejected,
    Executed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HandoffItem {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
    #[serde(default)]
    pub connection_id: String,
    pub connection_name: String,
    pub database: Option<String>,
    pub title: String,
    pub description: Option<String>,
    pub sql: String,
    pub operation_class: OperationClass,
    pub risk_level: RiskLevel,
    pub is_production: bool,
    pub status: HandoffStatus,
    pub result_summary: Option<String>,
    pub error: Option<String>,
}

impl HandoffItem {
    pub fn queued(
        connection_id: String,
        connection_name: String,
        database: Option<String>,
        title: String,
        description: Option<String>,
        sql: String,
        operation_class: OperationClass,
        risk_level: RiskLevel,
        is_production: bool,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            created_at: Utc::now(),
            created_by: "dbx-cli".to_string(),
            connection_id,
            connection_name,
            database,
            title,
            description,
            sql,
            operation_class,
            risk_level,
            is_production,
            status: HandoffStatus::Queued,
            result_summary: None,
            error: None,
        }
    }
}
