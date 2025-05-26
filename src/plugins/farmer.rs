use crate::database::config::{create_config_entry, get_config_key};
use crate::database::stats::{get_farmer_stats_range, has_farmer_stats, save_farmer_stats};
use crate::models::config::{AddConfigEntry, ConfigEntry};
use crate::models::ServerSettings;
use dg_fast_farmer::farmer::config::{Config, MetricsConfig};
use dg_fast_farmer::routes::FarmerPublicState;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::protocols::farmer::FarmerStats;
use log::{debug, error, info, Level};
use portfu::client::new_websocket;
use portfu::prelude::{serde_json, WebSocket};
use portfu_core::signal::await_termination;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::fs::Permissions;
use std::io::{Error, ErrorKind};
use std::ops::AddAssign;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use time::OffsetDateTime;
use tokio::fs::File;
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;
use tokio::time::Instant;

const fn default_parallel_read() -> bool {
    true
}
const fn default_plot_search_depth() -> i64 {
    0
}
const fn default_max_cpu_cores() -> i32 {
    -1
}
const fn default_max_cuda_devices() -> i32 {
    -1
}
const fn default_max_opencl_devices() -> i32 {
    -1
}
const fn default_recompute() -> u16 {
    0
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarvesterConfig {
    #[serde(default = "Vec::new")]
    pub plot_directories: Vec<String>,
    #[serde(default = "default_parallel_read")]
    pub parallel_read: bool,
    #[serde(default = "default_plot_search_depth")]
    pub plot_search_depth: i64,
    #[serde(default = "default_max_cpu_cores")]
    pub max_cpu_cores: i32,
    #[serde(default = "default_max_cuda_devices")]
    pub max_cuda_devices: i32,
    #[serde(default = "default_max_opencl_devices")]
    pub max_opencl_devices: i32,
    #[serde(default = "Vec::new")]
    pub cuda_device_list: Vec<u8>,
    #[serde(default = "Vec::new")]
    pub opencl_device_list: Vec<u8>,
    #[serde(default = "String::new")]
    pub recompute_host: String,
    #[serde(default = "default_recompute")]
    pub recompute_port: u16,
}
impl Default for HarvesterConfig {
    fn default() -> Self {
        Self {
            plot_directories: vec!["/mnt".to_string()],
            parallel_read: true,
            plot_search_depth: 2,
            max_cpu_cores: -1,
            max_cuda_devices: -1,
            max_opencl_devices: -1,
            cuda_device_list: vec![],
            opencl_device_list: vec![],
            recompute_host: String::new(),
            recompute_port: 0,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub enum FarmerStatus {
    Unknown,
    Running,
    Stopped,
    Exited(i32),
}

pub async fn load_farmer_config(pool: &SqlitePool) -> Result<Config<HarvesterConfig>, Error> {
    match get_config_key(pool, "farmer_config").await? {
        Some(farmer_config) => {
            debug!("Found Config:{:#?}", farmer_config.value);
            serde_json::from_str(&farmer_config.value).map_err(|e| {
                Error::new(
                    ErrorKind::InvalidData,
                    format!("Failed to Parse Existing Config: {e:?}"),
                )
            })
        }
        None => {
            let config = Config {
                fullnode_ws_host: "druid.garden".to_owned(),
                fullnode_ws_port: 443,
                fullnode_rpc_host: "druid.garden".to_owned(),
                fullnode_rpc_port: 443,
                metrics: Some(MetricsConfig {
                    enabled: true,
                    port: 9090,
                }),
                ..Default::default()
            };
            Ok(config)
        }
    }
}

pub async fn save_farmer_config(
    pool: &SqlitePool,
    config: &Config<HarvesterConfig>,
) -> Result<Option<ConfigEntry>, Error> {
    create_config_entry(
        pool,
        &AddConfigEntry {
            key: "farmer_config".to_string(),
            value: serde_json::to_string(config)?,
            last_value: "".to_string(),
            category: "farmer".to_string(),
            system: 1,
        },
    )
    .await
}

#[cfg(target_arch = "x86_64")]
const FAST_FARMER_BIN_URL: &str = "https://builds.druid.garden/linux_amd64/fast_farmer_gh.app";
#[cfg(target_arch = "aarch64")]
const FAST_FARMER_BIN_URL: &str = "https://builds.druid.garden/linux_arm64/fast_farmer_gh.app";

pub struct FarmerManager {
    instance: Arc<RwLock<Option<Child>>>,
    stats_handle: Option<JoinHandle<Result<(), Error>>>,
    plugin_path: PathBuf,
    install_mutex: Mutex<()>,
    database: SqlitePool,
}
impl FarmerManager {
    pub async fn new(
        server_settings: &ServerSettings,
        database: SqlitePool,
    ) -> Result<FarmerManager, Error> {
        let p_path = Path::new(&server_settings.plugin_path).join("farmer");
        tokio::fs::create_dir_all(&p_path).await?;
        let background_task_pool = database.clone();
        Ok(Self {
            instance: Arc::new(RwLock::new(None)),
            stats_handle: Some(tokio::spawn(async move {
                update_local_stats(&background_task_pool).await
            })),
            plugin_path: p_path.canonicalize()?,
            install_mutex: Mutex::new(()),
            database,
        })
    }
    pub async fn ensure_installed(&self) -> Result<(), Error> {
        info!("Checking for Farmer Installiation ");
        let install_mutex = self.install_mutex.lock().await;
        tokio::fs::create_dir_all(&self.plugin_path).await?;
        let bin_path = &self.plugin_path.join("fast_farmer_gh.app");
        if bin_path.exists() {
            info!("Farmer is up to date");
            drop(install_mutex);
            Ok(())
        } else {
            info!("No Farmer Found, Installing...");
            let file = reqwest::get(FAST_FARMER_BIN_URL).await.map_err(|e| {
                info!("Farmer Install Failed: {e:?}");
                Error::new(
                    ErrorKind::Other,
                    format!("Failed to download farmer bin file: {}", e),
                )
            })?;
            tokio::fs::write(
                &bin_path,
                &file.bytes().await.map_err(|e| {
                    info!("Farmer Install Failed: {e:?}");
                    Error::new(
                        ErrorKind::Other,
                        format!("Failed to download farmer bin file: {}", e),
                    )
                })?,
            )
            .await?;
            let permissions = Permissions::from_mode(0o755);
            drop(install_mutex);
            info!("Farmer Install Complete.");
            tokio::fs::set_permissions(bin_path, permissions).await
        }
    }
    pub async fn start_farmer(&self, config: Config<HarvesterConfig>) -> Result<(), Error> {
        info!("Farmer Starting");
        self.ensure_installed().await?;
        let mut instance = self.instance.write().await;
        match &*instance {
            Some(_) => Err(Error::new(
                ErrorKind::InvalidInput,
                "Farmer Already Started",
            )),
            None => {
                let mut tmp_file = File::create("/tmp/fast_farmer_config.yaml").await?;
                tmp_file
                    .write_all(
                        serde_yaml::to_string(&config)
                            .map_err(|e| Error::new(ErrorKind::Other, e))?
                            .as_bytes(),
                    )
                    .await?;
                let child = Command::new("./fast_farmer_gh.app")
                    .stdout(Stdio::null())
                    .stdin(Stdio::null())
                    .stderr(Stdio::null())
                    .arg("-c")
                    .arg("/tmp/fast_farmer_config.yaml")
                    .arg("run")
                    .arg("-m")
                    .arg("cli")
                    .current_dir(&self.plugin_path)
                    .kill_on_drop(true)
                    .spawn()?;
                *instance = Some(child);
                Ok(())
            }
        }
    }
    pub async fn stop_farmer(&self) -> Result<(), Error> {
        info!("Farmer Stopping");
        let mut instance = self.instance.write().await;
        match instance.take() {
            Some(mut handle) => {
                let _ = handle.kill().await;
                tokio::fs::remove_file("/tmp/fast_farmer_config.yaml").await?;
                Ok(())
            }
            None => Ok(()),
        }
    }
    pub async fn farmer_status(&self) -> FarmerStatus {
        let mut instance = self.instance.write().await;
        match &mut *instance {
            Some(handle) => match handle.try_wait() {
                Ok(None) => FarmerStatus::Running,
                Ok(Some(exit_code)) => FarmerStatus::Exited(exit_code.code().unwrap_or_default()),
                Err(e) => {
                    error!("Failed to check farmer status: {e:?}");
                    FarmerStatus::Unknown
                }
            },
            None => FarmerStatus::Stopped,
        }
    }
    async fn farmer_url(database: &SqlitePool) -> Result<Url, Error> {
        let config = load_farmer_config(database).await?;
        match config.metrics {
            None => Err(Error::new(
                ErrorKind::InvalidInput,
                "Invalid Config Metrics Disabled",
            )),
            Some(host_config) => Url::parse(&format!("http://localhost:{}", host_config.port))
                .map_err(|e| {
                    Error::new(ErrorKind::InvalidInput, format!("Invalid Config URL: {e}"))
                }),
        }
    }

    pub async fn farmer_metrics(&self) -> Result<String, Error> {
        let client = reqwest::Client::new();
        let mut url = Self::farmer_url(&self.database).await?;
        url.set_path("/metrics");
        client
            .get(url)
            .send()
            .await
            .map_err(|e| {
                Error::new(
                    ErrorKind::InvalidInput,
                    format!("Failed to Connect to Farmer: {e}"),
                )
            })?
            .text()
            .await
            .map_err(|e| {
                Error::new(
                    ErrorKind::InvalidInput,
                    format!("Failed to Read Response: {e}"),
                )
            })
    }
    pub async fn farmer_state(&self) -> Result<FarmerPublicState, Error> {
        let client = reqwest::Client::new();
        let mut url = Self::farmer_url(&self.database).await?;
        url.set_path("/state");
        client
            .get(url)
            .send()
            .await
            .map_err(|e| {
                Error::new(
                    ErrorKind::InvalidInput,
                    format!("Failed to Connect to Farmer: {e}"),
                )
            })?
            .json()
            .await
            .map_err(|e| {
                Error::new(
                    ErrorKind::InvalidInput,
                    format!("Failed to Read Response: {e}"),
                )
            })
    }
    pub async fn recent_farmer_stats(
        database: &SqlitePool,
    ) -> Result<HashMap<(Bytes32, Bytes32), FarmerStats>, Error> {
        let client = reqwest::Client::new();
        let mut url = Self::farmer_url(database).await?;
        url.set_path("/stats");
        client
            .get(url)
            .send()
            .await
            .map_err(|e| {
                Error::new(
                    ErrorKind::InvalidInput,
                    format!("Failed to Connect to Farmer: {e}"),
                )
            })?
            .json()
            .await
            .map_err(|e| {
                Error::new(
                    ErrorKind::InvalidInput,
                    format!("Failed to Read Response: {e}"),
                )
            })
    }
    pub async fn farmer_stats_range(
        &self,
        start: OffsetDateTime,
        end: OffsetDateTime,
    ) -> Result<HashMap<(Bytes32, Bytes32), FarmerStats>, Error> {
        get_farmer_stats_range(&self.database, start, end).await
    }
    pub async fn farmer_log_stream(
        &self,
        level: Level,
        client_socket: WebSocket,
    ) -> Result<(), Error> {
        let mut url = Self::farmer_url(&self.database).await?;
        url.set_path(&format!("/log_stream/{level}"));
        let upstream_socket = new_websocket(url.as_str(), None).await?;
        let mut err = None;
        loop {
            tokio::select! {
                upstream_message = upstream_socket.next() => {
                    match upstream_message {
                        Ok(Some(msg)) => {
                            client_socket.send(msg).await?;
                        }
                        Ok(None) => {
                            tokio::time::sleep(Duration::from_millis(1)).await;
                        },
                        Err(e) => {
                            err = Some(e);
                            break
                        },
                    }
                }
                client_message = client_socket.next() => {
                    match client_message {
                        Ok(Some(msg)) => {
                            upstream_socket.send(msg).await?;
                        }
                        Ok(None) => {
                            tokio::time::sleep(Duration::from_millis(1)).await;
                        },
                        Err(e) => {
                            err = Some(e);
                            break
                        },
                    }
                }
                _ = await_termination() => {
                    break;
                }
            }
        }
        match err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

impl Drop for FarmerManager {
    fn drop(&mut self) {
        let instance = self.instance.clone();
        let handle = self.stats_handle.take();
        tokio::spawn(async move {
            if let Some(mut v) = instance.write().await.take() {
                println!("Killing Farmer");
                let _ = v.kill().await;
                println!("Farmer Killed");
                drop(v);
            }
            if let Some(v) = handle {
                v.abort();
                let _ = v.await;
            }
        });
    }
}

pub async fn update_local_stats(database: &SqlitePool) -> Result<(), Error> {
    let mut run = false;
    let mut last_update = Instant::now();
    while run {
        tokio::select! {
            _ = await_termination() => {
                run = false;
            }
            res = async move {
                if last_update.elapsed().as_secs() > 30 {
                    last_update.add_assign(last_update.elapsed());
                }
                let mut url = FarmerManager::farmer_url(database).await?;
                url.set_path("/stats");
                let stats = FarmerManager::recent_farmer_stats(database).await?;
                for ((challenge_hash, sp_hash), farmer_stats) in stats {
                    if !has_farmer_stats(database, challenge_hash, sp_hash).await? {
                        save_farmer_stats(database, farmer_stats).await?;
                    }
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
                Ok::<(), Error>(())
            } => {
                if let Err(e) = res {
                    error!("Error in Update Farmer Stats: {e}");
                }
            }
        }
    }
    Ok(())
}
