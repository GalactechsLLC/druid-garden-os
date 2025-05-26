use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use time::OffsetDateTime;

#[derive(FromRow, Debug, Clone, Serialize, Deserialize)]
pub struct AddConfigEntry {
    pub key: String,
    pub value: String,
    pub last_value: String,
    pub category: String,
    pub system: i64,
}

#[derive(FromRow, Debug, Clone, Serialize, Deserialize)]
pub struct ConfigEntry {
    pub key: String,
    pub value: String,
    pub last_value: String,
    pub category: String,
    pub system: i64,
    pub created: OffsetDateTime,
    pub modified: OffsetDateTime,
}
