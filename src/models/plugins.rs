use serde::{Deserialize, Serialize};
use sqlx::{FromRow, Type};
use time::OffsetDateTime;

#[derive(Type, Debug, Clone, Serialize, Deserialize)]
pub enum PluginType {
    BuiltIn,
    Docker,
    File,
    RustProject,
    Invalid,
}
impl From<String> for PluginType {
    fn from(s: String) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "builtin" => Self::Docker,
            "docker" => Self::Docker,
            "rustproject" => Self::RustProject,
            "file" => Self::File,
            _ => Self::Invalid,
        }
    }
}

#[derive(FromRow, Debug, Clone, Serialize, Deserialize)]
pub struct AddPlugin {
    pub label: String,
    pub name: String,
    pub enabled: i64,
    pub plugin_type: PluginType,
    pub repo: String,
    pub tag: String,
    pub version: String,
    pub source: String,
    pub run_command: Option<String>,
}
impl From<AddPlugin> for Plugin {
    fn from(val: AddPlugin) -> Self {
        Plugin {
            id: None,
            name: val.name,
            label: val.label,
            enabled: val.enabled,
            plugin_type: val.plugin_type,
            repo: val.repo,
            tag: val.tag,
            source: val.source,
            run_command: val.run_command,
            version: val.version,
            added: OffsetDateTime::now_utc(),
            updated: OffsetDateTime::now_utc(),
        }
    }
}

#[derive(FromRow, Debug, Clone, Serialize, Deserialize)]
pub struct Plugin {
    pub id: Option<i64>,
    pub label: String,
    pub name: String,
    pub enabled: i64,
    pub plugin_type: PluginType,
    pub repo: String,
    pub tag: String,
    pub source: String,
    pub run_command: Option<String>,
    pub version: String,
    pub added: OffsetDateTime,
    pub updated: OffsetDateTime,
}

#[derive(FromRow, Debug, Clone, Serialize, Deserialize)]
pub struct PluginEnvironmentEntry {
    pub plugin_id: i64,
    pub key: String,
    pub value: String,
    pub added: OffsetDateTime,
    pub updated: OffsetDateTime,
}
