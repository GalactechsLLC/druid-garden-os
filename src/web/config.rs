use crate::config::ConfigManager;
use crate::database::config::{delete_config_entry, get_config};
use crate::models::config::{AddConfigEntry, ConfigEntry};
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
            body.system = 0;
            state
                .0
                .write()
                .await
                .set(&key.inner(), body, Some(db.as_ref()))
                .await
        }
        None => Err(Error::new(
            ErrorKind::InvalidInput,
            "The provided config is Invalid",
        )),
    }
}

#[delete("/config/{key}", output = "json", eoutput = "bytes")]
pub async fn del_config(db: State<SqlitePool>, key: Path) -> Result<bool, Error> {
    delete_config_entry(db.as_ref(), &key.inner())
        .await
        .map(|v| v > 0)
}
