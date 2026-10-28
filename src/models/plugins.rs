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
            "builtin" => Self::BuiltIn,
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
            run_command: Some(val.run_command.unwrap_or_default()),
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

/// Names become directory, unit, and container identifiers; never paths/options.
pub fn validate_plugin_name(name: &str) -> Result<(), std::io::Error> {
    if name.is_empty()
        || name.len() > 48
        || !name.as_bytes()[0].is_ascii_alphanumeric()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "Plugin names must be 1 to 48 ASCII letters, digits, underscores, or hyphens and start with a letter or digit"));
    }
    if matches!(
        name,
        "file_manager" | "disk_manager" | "system_monitor" | "farmer_manager"
    ) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "Built-in plugins cannot be modified",
        ));
    }
    Ok(())
}
impl Plugin {
    pub fn validate_external(&self) -> Result<(), std::io::Error> {
        validate_plugin_name(&self.name)?;
        if !matches!(self.plugin_type, PluginType::Docker | PluginType::File) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "Only isolated Docker and File plugins are supported",
            ));
        }
        if self
            .run_command
            .as_ref()
            .is_some_and(|command| !command.is_empty())
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "Custom plugin run commands are not supported",
            ));
        }
        if self.label.len() > 256
            || self.repo.len() > 2048
            || self.source.len() > 512
            || self.tag.len() > 128
            || self.version.len() > 128
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Plugin metadata exceeds limit",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plugin_identifiers_cannot_escape_their_namespace() {
        for name in [
            "",
            "../escape",
            "/root",
            "--option",
            "name.service",
            "file_manager",
            "disk_manager",
            "bad name",
        ] {
            assert!(validate_plugin_name(name).is_err());
        }
        assert!(validate_plugin_name("my_plugin-2").is_ok());
        assert!(matches!(
            PluginType::from("builtin".to_string()),
            PluginType::BuiltIn
        ));
    }
}
