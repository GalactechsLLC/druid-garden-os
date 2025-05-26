use crate::database::plugins::{
    create_plugin_environment_entry, delete_plugin_environment_entry, get_all_plugins, get_plugin,
    get_plugin_environment_entries, get_plugin_environment_entry,
};
use crate::models::plugins::{AddPlugin, Plugin, PluginEnvironmentEntry};
use crate::plugins::{PluginManager, PluginStatus, PluginUpdates, StorePlugin};
use portfu::prelude::*;
use portfu_core::Json;
use portfu_macros::{delete, get, post, put};
use sqlx::SqlitePool;
use std::io::{Error, ErrorKind};
use tokio::sync::RwLock;

#[post("/api/plugins/available", output = "json", eoutput = "bytes")]
pub async fn available_plugins(
    state: State<RwLock<PluginManager>>,
) -> Result<Vec<StorePlugin>, Error> {
    Ok(state.0.read().await.available_plugins().await)
}

#[post("/api/plugins/updates", output = "json", eoutput = "bytes")]
pub async fn plugin_updates(
    state: State<RwLock<PluginManager>>,
) -> Result<Vec<PluginUpdates>, Error> {
    state.0.read().await.plugin_updates().await
}

#[post("/api/plugins/refresh", output = "json", eoutput = "bytes")]
pub async fn refresh_plugins(
    state: State<RwLock<PluginManager>>,
) -> Result<Vec<StorePlugin>, Error> {
    state.0.write().await.update_plugin_store().await?;
    Ok(state.0.read().await.available_plugins().await)
}

#[get("/api/plugins", output = "json", eoutput = "bytes")]
pub async fn all_plugins(db: State<SqlitePool>) -> Result<Vec<Plugin>, Error> {
    get_all_plugins(db.as_ref()).await
}

#[get("/api/plugins/{name}", output = "json", eoutput = "bytes")]
pub async fn plugin(db: State<SqlitePool>, name: Path) -> Result<Option<Plugin>, Error> {
    get_plugin(db.as_ref(), &name.inner()).await
}

#[post("/api/plugins", output = "json", eoutput = "bytes")]
pub async fn add_plugin(
    db: State<SqlitePool>,
    body: Json<Option<AddPlugin>>,
    state: State<RwLock<PluginManager>>,
) -> Result<Plugin, Error> {
    match body.inner() {
        Some(body) => state.0.write().await.add(body, db.as_ref()).await,
        None => Err(Error::new(
            ErrorKind::InvalidInput,
            "The provided plugin is Invalid",
        )),
    }
}

#[put("/api/plugins", output = "json", eoutput = "bytes")]
pub async fn update_plugin(
    db: State<SqlitePool>,
    body: Json<Option<AddPlugin>>,
    state: State<RwLock<PluginManager>>,
) -> Result<Plugin, Error> {
    match body.inner() {
        Some(body) => state.0.write().await.update_plugin(body, db.as_ref()).await,
        None => Err(Error::new(
            ErrorKind::InvalidInput,
            "The provided plugin is Invalid",
        )),
    }
}

#[post("/api/plugins/{name}/start", output = "json", eoutput = "bytes")]
pub async fn start_plugin(
    db: State<SqlitePool>,
    state: State<RwLock<PluginManager>>,
    name: Path,
) -> Result<bool, Error> {
    match get_plugin(db.as_ref(), &name.inner()).await? {
        Some(p) => {
            let rw_lock = state.0.clone();
            let mut plugin_manager = rw_lock.write().await;
            let started = plugin_manager.start(p).await?;
            Ok(started)
        }
        None => Err(Error::new(
            ErrorKind::NotFound,
            "The provided plugin is Invalid",
        )),
    }
}

#[post("/api/plugins/{name}/stop", output = "json", eoutput = "bytes")]
pub async fn stop_plugin(
    db: State<SqlitePool>,
    state: State<RwLock<PluginManager>>,
    name: Path,
) -> Result<bool, Error> {
    match get_plugin(db.as_ref(), &name.inner()).await? {
        Some(p) => {
            let rw_lock = state.0.clone();
            let mut plugin_manager = rw_lock.write().await;
            let stopped = plugin_manager.stop(p).await?;
            Ok(stopped)
        }
        None => Err(Error::new(
            ErrorKind::NotFound,
            "The provided plugin is Invalid",
        )),
    }
}

#[get("/api/plugins/{name}/status", output = "json", eoutput = "bytes")]
pub async fn plugin_status(
    db: State<SqlitePool>,
    state: State<RwLock<PluginManager>>,
    name: Path,
) -> Result<PluginStatus, Error> {
    match get_plugin(db.as_ref(), &name.inner()).await? {
        Some(p) => {
            let rw_lock = state.0.clone();
            let plugin_manager = rw_lock.read().await;
            let status = plugin_manager.status(p).await?;
            Ok(status)
        }
        None => Err(Error::new(
            ErrorKind::NotFound,
            "The provided plugin is Invalid",
        )),
    }
}

#[delete("/api/plugins/{name}", output = "json", eoutput = "bytes")]
pub async fn del_plugin(
    db: State<SqlitePool>,
    state: State<RwLock<PluginManager>>,
    name: Path,
) -> Result<bool, Error> {
    match get_plugin(db.as_ref(), &name.inner()).await? {
        Some(p) => state.0.write().await.uninstall(p, db.as_ref()).await,
        None => Err(Error::new(
            ErrorKind::NotFound,
            "The provided plugin is Invalid",
        )),
    }
}

#[get("/api/plugins/{name}/env", output = "json", eoutput = "bytes")]
pub async fn get_plugin_environment(
    db: State<SqlitePool>,
    name: Path,
) -> Result<Vec<PluginEnvironmentEntry>, Error> {
    get_plugin_environment_entries(db.as_ref(), name.inner().as_ref()).await
}

#[get("/api/plugins/{name}/env/{key}", output = "json", eoutput = "bytes")]
pub async fn get_plugin_environment_value(
    db: State<SqlitePool>,
    name: Path,
    key: Path,
) -> Result<Option<PluginEnvironmentEntry>, Error> {
    get_plugin_environment_entry(db.as_ref(), &name.inner(), &key.inner()).await
}

#[post("/api/plugins/{name}/env", output = "json", eoutput = "bytes")]
pub async fn set_plugin_environment_value(
    db: State<SqlitePool>,
    body: Json<Option<PluginEnvironmentEntry>>,
) -> Result<Option<PluginEnvironmentEntry>, Error> {
    match body.inner() {
        Some(body) => create_plugin_environment_entry(db.as_ref(), &body).await,
        None => Err(Error::new(
            ErrorKind::InvalidInput,
            "The provided plugin environment value is Invalid",
        )),
    }
}

#[delete("/api/plugins/{name}/env/{key}", output = "json", eoutput = "bytes")]
pub async fn del_plugin_environment_value(
    db: State<SqlitePool>,
    name: Path,
    key: Path,
) -> Result<bool, Error> {
    delete_plugin_environment_entry(db.as_ref(), &name.inner(), &key.inner())
        .await
        .map(|v| v > 0)
}
