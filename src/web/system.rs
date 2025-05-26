use crate::database::config::get_config_key;
use crate::utils::has_internet_connection;
use crate::version;
use dg_edge_updater::{fetch_manifest, UPDATER_SERVICE_NAME};
use dg_logger::DruidGardenLogger;
use dg_network_manager::dbus_api::devices::Device;
use dg_network_manager::{
    connect_to_access_point, create_hotspot, delete_active_connection, delete_connection,
    find_active_hotspots, find_all_hotspots, reset_active_connection, scan_all_ssids,
    wireless_device, wireless_devices,
};
use log::{debug, error, info, Level};
use portfu::prelude::tokio_tungstenite::tungstenite::Message;
use portfu::prelude::{serde_json, Path, State, WebSocket};
use portfu_core::Json;
use portfu_macros::{get, post, websocket};
use reqwest::Client;
use semver::Version;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::io::{Error, ErrorKind};
use std::str::FromStr;
use std::time::Duration;
use tokio::process::Command;

#[get("/system/device")]
pub async fn find_device() -> Result<String, Error> {
    Ok("GardenOS".to_string())
}

#[derive(Serialize)]
pub struct UpdateInfo {
    pub remote_version: Version,
    pub local_version: Version,
    pub has_update: bool,
}

#[get("/system/updates", output = "json", eoutput = "bytes")]
pub async fn find_updates() -> Result<UpdateInfo, Error> {
    let client = Client::new();
    let manifest = fetch_manifest(&client).await?;
    let remote_version =
        Version::parse(&manifest.version).map_err(|e| Error::new(ErrorKind::Other, e))?;
    info!("Found Remote version: {}", remote_version);
    let local_version = Version::parse(version()).map_err(|e| Error::new(ErrorKind::Other, e))?;
    Ok(UpdateInfo {
        has_update: remote_version > local_version,
        remote_version,
        local_version,
    })
}

#[post("/system/updates")]
pub async fn do_updates() -> Result<String, Error> {
    let client = Client::new();
    let manifest = fetch_manifest(&client).await?;
    let remote_version =
        Version::parse(&manifest.version).map_err(|e| Error::new(ErrorKind::Other, e))?;
    info!("Found Remote version: {}", remote_version);
    let local_version = Version::parse(version()).map_err(|e| Error::new(ErrorKind::Other, e))?;
    if remote_version > local_version {
        run_update_service().await?;
        Ok("true".to_owned())
    } else {
        Ok("false".to_owned())
    }
}

async fn run_update_service() -> Result<(), Error> {
    let status = Command::new("systemctl")
        .arg("restart")
        .arg(UPDATER_SERVICE_NAME)
        .status()
        .await?;
    if !status.success() {
        return Err(Error::new(
            ErrorKind::Other,
            format!("Failed to Start Updater: {:?}", status),
        ));
    }
    Ok(())
}

#[post("/system/is_online", output = "json", eoutput = "bytes")]
pub async fn is_online() -> Result<bool, Error> {
    Ok(has_internet_connection().await)
}

#[derive(Serialize)]
pub struct AccessPoint {
    pub ssid: String,
    pub strength: u8,
}

#[post("/system/wifi/scan", output = "json", eoutput = "bytes")]
pub async fn wifi_scan(pool: State<SqlitePool>) -> Result<Vec<AccessPoint>, Error> {
    let wireless_device_name = get_config_key(pool.0.as_ref(), "wifi_device")
        .await?
        .map(|c| c.value)
        .unwrap_or_default();
    let mut found_wireless_device: Option<Device> = None;
    for device in wireless_devices().await? {
        let wireless_device = match &device {
            Device::Wireless(w) => w,
            _ => unreachable!(),
        };
        match wireless_device.interface().await {
            Ok(v) => {
                if v == wireless_device_name {
                    found_wireless_device = Some(device);
                    break;
                }
            }
            Err(e) => {
                error!("Failure when Checking Device Interface Name: {e:?}");
            }
        }
    }
    match found_wireless_device {
        Some(device) => scan_all_ssids(device).await.map(|v| {
            v.into_iter()
                .map(|v| AccessPoint {
                    ssid: v.ssid,
                    strength: v.strength,
                })
                .collect()
        }),
        None => Err(Error::new(
            ErrorKind::NotFound,
            format!("Failed to find device with name: {}", wireless_device_name),
        )),
    }
}

#[derive(Deserialize)]
pub struct ConnectPayload {
    pub ssid: String,
    pub password: Option<String>,
}

#[post("/system/wifi/connect", output = "json", eoutput = "bytes")]
pub async fn wifi_connect(
    payload: Json<Option<ConnectPayload>>,
    pool: State<SqlitePool>,
) -> Result<(), Error> {
    match payload.inner() {
        Some(payload) => {
            let wireless_device_name = get_config_key(pool.0.as_ref(), "wifi_device")
                .await?
                .map(|c| c.value)
                .unwrap_or_default();
            let mut found_wireless_device: Option<Device> = None;
            for device in wireless_devices().await? {
                let wireless_device = match &device {
                    Device::Wireless(w) => w,
                    _ => unreachable!(),
                };
                match wireless_device.interface().await {
                    Ok(v) => {
                        if v == wireless_device_name {
                            found_wireless_device = Some(device);
                            break;
                        }
                    }
                    Err(e) => {
                        error!("Failure when Checking Device Interface Name: {e:?}");
                    }
                }
            }
            match found_wireless_device {
                Some(device) => {
                    connect_to_access_point(device, payload.ssid, payload.password, None).await
                }
                None => Err(Error::new(
                    ErrorKind::NotFound,
                    format!("Failed to find device with name: {}", wireless_device_name),
                )),
            }
        }
        None => Err(Error::new(ErrorKind::InvalidInput, "Invalid payload"))?,
    }
}

#[post("/system/hotspot/active", output = "json", eoutput = "bytes")]
pub async fn hotspot_active() -> Result<bool, Error> {
    find_active_hotspots().await.map(|v| !v.is_empty())
}

#[post("/system/hotspot/clean", output = "json", eoutput = "bytes")]
pub async fn hotspot_clean() -> Result<bool, Error> {
    for con in find_all_hotspots().await? {
        delete_connection(con).await?;
    }
    Ok(true)
}

#[post("/system/hotspot/stop", output = "json", eoutput = "bytes")]
pub async fn hotspot_stop() -> Result<bool, Error> {
    for con in find_active_hotspots().await? {
        delete_active_connection(con).await?;
    }
    Ok(true)
}

#[derive(Deserialize)]
pub struct HotspotPayload {
    pub device: Option<String>,
    pub ssid: String,
    pub password: Option<String>,
}

#[post("/system/hotspot/start", output = "json", eoutput = "bytes")]
pub async fn hotspot_start(
    payload: Json<Option<HotspotPayload>>,
    pool: State<SqlitePool>,
) -> Result<bool, Error> {
    match payload.inner() {
        Some(payload) => {
            let device = match payload.device {
                Some(name) => wireless_device(&name).await?.ok_or(Error::new(
                    ErrorKind::NotFound,
                    format!("Device not found: {name}"),
                ))?,
                None => match get_config_key(pool.0.as_ref(), "wifi_device").await? {
                    Some(config_device) => {
                        wireless_device(&config_device.value)
                            .await?
                            .ok_or(Error::new(
                                ErrorKind::NotFound,
                                format!("Device not found: {}", config_device.value),
                            ))?
                    }
                    None => wireless_devices()
                        .await?
                        .into_iter()
                        .next()
                        .ok_or(Error::new(ErrorKind::NotFound, "No Wireless Devices found"))?,
                },
            };
            for con in find_active_hotspots().await? {
                delete_active_connection(con).await?;
            }
            create_hotspot(device, None, payload.ssid.clone(), payload.password.clone()).await?;
            Ok(true)
        }
        None => Err(Error::new(ErrorKind::InvalidInput, "Invalid payload"))?,
    }
}

#[post("/system/hotspot/restart", output = "json", eoutput = "bytes")]
pub async fn hotspot_restart() -> Result<bool, Error> {
    let active_hotspots = find_active_hotspots().await?;
    if active_hotspots.is_empty() {
        return Err(Error::new(ErrorKind::NotFound, "No Active Hotspot Found"))?;
    } else {
        for con in active_hotspots {
            reset_active_connection(con).await?;
        }
    }
    Ok(true)
}

#[websocket("/api/system/log/{level}")]
pub async fn log_stream(
    socket: WebSocket,
    level: Path,
    logger: State<DruidGardenLogger>,
) -> Result<(), Error> {
    let mut err = None;
    let level = level.inner();
    let level = Level::from_str(level.as_str()).map_err(|e| {
        Error::new(
            ErrorKind::InvalidInput,
            format!("{} is not a valid Log Level: {e:?}", level),
        )
    })?;
    let mut receiver = logger.0.subscribe();
    loop {
        tokio::select! {
            result = receiver.recv() => {
                match result {
                    Ok(log_entry) => {
                        if log_entry.level <= level {
                            let as_json = serde_json::to_string(&log_entry)?;
                            if let Err(e) = socket.send(Message::Text(as_json.into())).await {
                                debug!("Failed to send log entry: {e:?}");
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        error!("Failed to read message from log channel: {e:?}");
                        break;
                    }
                }
            }
            result = socket.next() => {
                match result {
                    Ok(Some(msg)) => {
                        match msg {
                            Message::Ping(ping_data) => {
                                socket.send(Message::Pong(ping_data)).await?;
                            }
                            Message::Pong(_) | Message::Frame(_) |
                            Message::Binary(_) | Message::Text(_) => {
                                //Ignore Client Messages
                                continue;
                            }
                            Message::Close(_close_msg) => {
                                info!("MPC Stream received Close");
                                break;
                            }
                        }
                    }
                    Ok(None) => {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    },
                    Err(e) => {
                        err = Some(e);
                        break
                    },
                }
            }
        }
    }
    match err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}
