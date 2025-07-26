use crate::config::{
    DEFAULT_FULLNODE_RPC_HOST, DEFAULT_FULLNODE_RPC_PORT, DEFAULT_FULLNODE_WS_HOST,
    DEFAULT_FULLNODE_WS_PORT,
};
use crate::legacy::PreloadConfig;
use crate::plugins::farmer::{
    load_farmer_config, save_farmer_config, FarmerManager, FarmerStatus, HarvesterConfig,
};
use crate::plugins::system_monitor::SystemMonitorPlugin;
use dg_fast_farmer::cli::commands::{generate_config_from_mnemonic, GenerateConfig};
use dg_fast_farmer::farmer::config::{Config, MetricsConfig};
use dg_fast_farmer::routes::FarmerPublicState;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::protocols::farmer::FarmerStats;
use log::{info, warn, Level};
use portfu::prelude::{Path, State, WebSocket};
use portfu_core::Json;
use portfu_macros::{get, post, websocket};
use serde::Deserialize;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::io::{Error, ErrorKind};
use std::path::PathBuf;
use std::str::FromStr;
use time::OffsetDateTime;

#[get("/farmer/config/ready", output = "json", eoutput = "bytes")]
pub async fn is_config_ready(pool: State<SqlitePool>) -> Result<bool, Error> {
    let config: Config<HarvesterConfig> = load_farmer_config(pool.0.as_ref()).await?;
    Ok(config.is_ready())
}

#[get("/farmer/config", output = "json", eoutput = "bytes")]
pub async fn get_config(pool: State<SqlitePool>) -> Result<Config<HarvesterConfig>, Error> {
    let config = load_farmer_config(pool.0.as_ref()).await?;
    Ok(config)
}

#[get("/farmer/metrics", output = "json", eoutput = "bytes")]
pub async fn get_farmer_metrics(farmer_manager: State<FarmerManager>) -> Result<String, Error> {
    farmer_manager.0.farmer_metrics().await
}

#[get("/farmer/stats", output = "json", eoutput = "bytes")]
pub async fn get_farmer_stats(
    farmer_manager: State<FarmerManager>,
) -> Result<Vec<FarmerStats>, Error> {
    farmer_manager.0.recent_farmer_stats().await
}

#[get("/farmer/state", output = "json", eoutput = "bytes")]
pub async fn get_farmer_state(
    farmer_manager: State<FarmerManager>,
) -> Result<FarmerPublicState, Error> {
    farmer_manager.0.farmer_state().await
}

#[derive(Deserialize)]
pub struct RangePayload {
    pub start: i64,
    pub end: i64,
}

#[post("/farmer/stats", output = "json", eoutput = "bytes")]
pub async fn get_farmer_stats_range(
    farmer_manager: State<FarmerManager>,
    payload: Json<Option<RangePayload>>,
) -> Result<HashMap<(Bytes32, Bytes32), FarmerStats>, Error> {
    match payload.inner() {
        Some(payload) => {
            let start = OffsetDateTime::from_unix_timestamp(payload.start).map_err(|e| {
                Error::new(
                    ErrorKind::InvalidInput,
                    format!("Failed to parse start: {e}"),
                )
            })?;
            let end = OffsetDateTime::from_unix_timestamp(payload.end).map_err(|e| {
                Error::new(ErrorKind::InvalidInput, format!("Failed to parse end: {e}"))
            })?;
            farmer_manager.0.farmer_stats_range(start, end).await
        }
        None => Err(Error::new(ErrorKind::InvalidInput, "Invalid Range Payload")),
    }
}

#[websocket("/farmer/log_stream/{level}")]
pub async fn farmer_log_stream(
    socket: WebSocket,
    level: Path,
    farmer_manager: State<FarmerManager>,
) -> Result<(), Error> {
    let level = level.inner();
    let level = Level::from_str(level.as_str()).map_err(|e| {
        Error::new(
            ErrorKind::InvalidInput,
            format!("{level} is not a valid Log Level: {e:?}"),
        )
    })?;
    farmer_manager.0.farmer_log_stream(level, socket).await
}

#[post("/farmer/config", output = "json", eoutput = "bytes")]
pub async fn update_config(
    pool: State<SqlitePool>,
    payload: Json<Option<Config<HarvesterConfig>>>,
) -> Result<Config<HarvesterConfig>, Error> {
    match payload.inner() {
        Some(config) => {
            save_farmer_config(pool.0.as_ref(), &config).await?;
            Ok(config)
        }
        None => Err(Error::new(
            ErrorKind::InvalidInput,
            "Invalid Config Payload",
        ))?,
    }
}

#[post("/farmer/config/scan", output = "json", eoutput = "bytes")]
pub async fn scan_for_legacy_configs(
    pool: State<SqlitePool>,
    system_monitor: State<SystemMonitorPlugin>,
) -> Result<Config<HarvesterConfig>, Error> {
    system_monitor.0.reload_disks().await?;
    let disk_info = system_monitor.0.get_disk_info().await?;
    let mut mounted_devices: Vec<String> = disk_info
        .iter()
        .filter_map(|v| v.mount_path.clone())
        .collect();
    for disk in disk_info {
        let mounted_partitions: Vec<String> = disk
            .partitions
            .iter()
            .filter_map(|v| {
                v.mount_path
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .clone()
            })
            .collect();
        mounted_devices.extend(mounted_partitions);
    }
    //Scan Mounted Devices for preload.pconf if found parse and generate a config,
    //we are only searching 1 level deep, pconfs should be at the root of the drive
    let mut current_config = load_farmer_config(pool.0.as_ref()).await?;
    let mut pconfs = vec![];
    for mount_path in mounted_devices {
        let mount_path = PathBuf::from(mount_path);
        for entry in mount_path.read_dir()? {
            let preload_file = match entry {
                Ok(entry) => {
                    if entry.file_name() == "preload.pconf" {
                        entry
                    } else {
                        continue;
                    }
                }
                Err(err) => {
                    warn!("Error when Reading File: {err:?}");
                    continue;
                }
            };
            let preload_file_path = preload_file.path();
            let parsed = PreloadConfig::try_from(preload_file_path.as_path())?;
            pconfs.push(parsed);
        }
    }
    if pconfs.is_empty() {
        Ok(current_config)
    } else {
        for pre_config in pconfs {
            let pre_launcher_id = Bytes32::from_str(&pre_config.launcher_id)?;
            if current_config.farmer_info.iter().any(|i| {
                if let Some(launcher_id) = &i.launcher_id {
                    *launcher_id == pre_launcher_id
                } else {
                    false
                }
            }) {
                info!("Skipping existing launcher ID");
                continue;
            } else {
                info!("Found new PreConfig for launcher ID {pre_launcher_id}");
                let generated = generate_config_from_mnemonic::<HarvesterConfig>(
                    GenerateConfig {
                        output_path: None,
                        mnemonic_file: None,
                        mnemonic_string: Some(pre_config.mnemonic),
                        fullnode_ws_host: Some(current_config.fullnode_ws_host.clone()),
                        fullnode_ws_port: Some(current_config.fullnode_ws_port),
                        fullnode_rpc_host: Some(current_config.fullnode_rpc_host.clone()),
                        fullnode_rpc_port: Some(current_config.fullnode_rpc_port),
                        fullnode_ssl: current_config.ssl_root_path.clone(),
                        network: Some(current_config.selected_network.clone()),
                        launcher_id: Some(pre_launcher_id),
                        payout_address: Some(current_config.payout_address.clone()),
                        plot_directories: Some(vec![]),
                        additional_headers: None,
                    },
                    false,
                )
                .await?;
                current_config.merge(generated);
            }
        }
        if current_config.harvester_configs.custom_config.is_none() {
            current_config.harvester_configs.custom_config = Some(HarvesterConfig::default());
        }
        save_farmer_config(pool.0.as_ref(), &current_config).await?;
        Ok(current_config)
    }
}

#[derive(Deserialize)]
pub struct GenerateMnemonicRequest {
    mnemonic: String,
}

#[post("/farmer/config/mnemonic", output = "json", eoutput = "bytes")]
pub async fn generate_from_mnemonic(
    pool: State<SqlitePool>,
    payload: Json<Option<GenerateMnemonicRequest>>,
) -> Result<Config<HarvesterConfig>, Error> {
    match payload.inner() {
        Some(config) => {
            let mut generated = generate_config_from_mnemonic::<HarvesterConfig>(
                GenerateConfig {
                    output_path: None,
                    mnemonic_file: None,
                    mnemonic_string: Some(config.mnemonic),
                    fullnode_ws_host: Some(DEFAULT_FULLNODE_WS_HOST.to_string()),
                    fullnode_ws_port: Some(DEFAULT_FULLNODE_WS_PORT),
                    fullnode_rpc_host: Some(DEFAULT_FULLNODE_RPC_HOST.to_string()),
                    fullnode_rpc_port: Some(DEFAULT_FULLNODE_RPC_PORT),
                    fullnode_ssl: None,
                    network: None,
                    launcher_id: None,
                    payout_address: None,
                    plot_directories: None,
                    additional_headers: None,
                },
                false,
            )
            .await?;
            generated.harvester_configs.custom_config = Some(HarvesterConfig::default());
            generated.metrics = Some(MetricsConfig {
                enabled: true,
                port: 9090,
            });
            save_farmer_config(pool.0.as_ref(), &generated).await?;
            Ok(generated)
        }
        None => Err(Error::new(
            ErrorKind::InvalidInput,
            "Invalid Config Payload",
        ))?,
    }
}

#[get("/farmer/status", output = "json", eoutput = "bytes")]
pub async fn farmer_status(farmer_manager: State<FarmerManager>) -> Result<FarmerStatus, Error> {
    Ok(farmer_manager.0.farmer_status().await)
}

#[post("/farmer/start", output = "none", eoutput = "bytes")]
pub async fn start_farmer(
    pool: State<SqlitePool>,
    farmer_manager: State<FarmerManager>,
) -> Result<(), Error> {
    let config = load_farmer_config(pool.0.as_ref()).await?;
    if config.is_ready() {
        info!("Farmer is ready");
        farmer_manager.0.start_farmer(config).await
    } else {
        Err(Error::new(
            ErrorKind::InvalidInput,
            "Config Not Ready for Farming",
        ))?
    }
}

#[post("/farmer/stop", output = "none", eoutput = "bytes")]
pub async fn stop_farmer(farmer_manager: State<FarmerManager>) -> Result<(), Error> {
    info!("Stopping Farmer");
    farmer_manager.0.stop_farmer().await
}

#[post("/farmer/restart", output = "none", eoutput = "bytes")]
pub async fn restart_farmer(
    pool: State<SqlitePool>,
    farmer_manager: State<FarmerManager>,
) -> Result<(), Error> {
    farmer_manager.0.stop_farmer().await?;
    let config = load_farmer_config(pool.0.as_ref()).await?;
    if config.is_ready() {
        farmer_manager.0.start_farmer(config).await
    } else {
        Err(Error::new(
            ErrorKind::InvalidInput,
            "Config Not Ready for Farming",
        ))?
    }
}
