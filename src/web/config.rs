use crate::config::ConfigManager;
use crate::database::config::get_config;
use crate::models::config::{AddConfigEntry, ConfigEntry};
use crate::plugins::device_heartbeat::validate_heartbeat_key_value;
use portfu::prelude::*;
use portfu_core::Json;
use portfu_macros::{delete, get, post};
use sqlx::SqlitePool;
use std::io::{Error, ErrorKind};
use tokio::sync::RwLock;

#[get("/config", output = "json", eoutput = "bytes")]
pub async fn configs(db: State<SqlitePool>) -> Result<Vec<ConfigEntry>, Error> {
    get_config(db.as_ref()).await
}

#[get("/config/{key}", output = "json", eoutput = "bytes")]
pub async fn config_entry(
    key: Path,
    state: State<RwLock<ConfigManager>>,
) -> Result<Option<ConfigEntry>, Error> {
    Ok(state.0.read().await.get(&key.inner()).await)
}

#[post("/config/{key}", output = "json", eoutput = "bytes")]
pub async fn upload_config(
    db: State<SqlitePool>,
    key: Path,
    body: Json<Option<AddConfigEntry>>,
    state: State<RwLock<ConfigManager>>,
) -> Result<Option<ConfigEntry>, Error> {
    match body.inner() {
        Some(mut body) => {
            let key = key.inner();
            if body.key != key {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "Config body key must match URL key",
                ));
            }
            if key == "heartbeat.device_id" {
                return Err(Error::new(
                    ErrorKind::PermissionDenied,
                    "heartbeat.device_id is managed by the device and cannot be changed",
                ));
            }
            validate_heartbeat_key_value(&key, &body.value)?;
            body.system = 0;
            state
                .0
                .write()
                .await
                .set(&key, body, Some(db.as_ref()))
                .await
        }
        None => Err(Error::new(
            ErrorKind::InvalidInput,
            "The provided config is Invalid",
        )),
    }
}

#[delete("/config/{key}", output = "json", eoutput = "bytes")]
pub async fn del_config(
    db: State<SqlitePool>,
    state: State<RwLock<ConfigManager>>,
    key: Path,
) -> Result<bool, Error> {
    let key = key.inner();
    if key == "heartbeat.device_id" {
        return Err(Error::new(
            ErrorKind::PermissionDenied,
            "heartbeat.device_id is managed by the device and cannot be deleted",
        ));
    }
    let mut config = state.0.write().await;
    let existed = config.get(&key).await.is_some();
    config.delete(&key, db.as_ref()).await?;
    Ok(existed)
}
