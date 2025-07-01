use crate::database::config::{create_config_entry, get_config_key};
use crate::database::stats::{
    get_farmer_stats_range, has_farmer_stats, prune_farmer_stats, save_farmer_stats,
};
use crate::models::config::{AddConfigEntry, ConfigEntry};
use dg_fast_farmer::farmer::config::{Config, MetricsConfig};
use dg_fast_farmer::routes::FarmerPublicState;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::protocols::farmer::FarmerStats;
use log::{debug, error, info, warn, Level};
use portfu::client::new_websocket;
use portfu::prelude::{serde_json, State, WebSocket};
use portfu_core::signal::await_termination;
use portfu_macros::interval;
use reqwest::{Client, Url};
use semver::Version;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::env;
use std::io::{Error, ErrorKind};
use std::path::Path;
use std::process::Stdio;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use time::OffsetDateTime;
use tokio::fs::{copy, metadata, remove_file, rename, set_permissions, File};
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, RwLock};

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

#[derive(Debug, Serialize, Deserialize)]
pub enum UpdateChannel {
    Release,
    Beta,
}

const FAST_FARMER_MANIFEST_URL: &str = "https://builds.druid.garden/manifest.yaml";
const BIN_PATH: &str = "/usr/bin/fast_farmer_gh.app";
const BACKUP_PATH: &str = "/usr/bin/fast_farmer_gh.app.bak";
const TMP_PATH: &str = "/tmp/fast_farmer_gh.app";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FastFarmerManifest {
    pub current_version: Version,
    pub beta_version: Option<Version>,
    pub date: Option<String>,
    pub author: Option<String>,
}

pub struct FarmerManager {
    instance: Arc<RwLock<Option<Child>>>,
    install_mutex: Mutex<()>,
    database: SqlitePool,
    client: Client,
}
impl FarmerManager {
    pub async fn new(database: SqlitePool) -> Result<FarmerManager, Error> {
        let client = Client::new();
        let bin_path = Path::new(BIN_PATH);
        let current_version = Self::get_binary_version(bin_path).await;
        match current_version {
            Some(current_version) => {
                info!("Found Binary Version: {}", &current_version);
            }
            None => {
                info!("No Binary Version Installed");
            }
        }
        match Self::fetch_manifest(&client).await {
            Ok(manifest) => {
                info!("Found Farmer Manifest");
                info!("Remote Version: {}", manifest.current_version);
                match manifest.beta_version {
                    Some(beta_version) => {
                        if beta_version != manifest.current_version {
                            info!("Remote Beta Version: {beta_version}");
                        } else {
                            info!("No Beta Version Available");
                        }
                    }
                    None => {
                        info!("No Beta Version Available");
                    }
                }
            }
            Err(e) => {
                error!("Failed to fetch Farmer Manifest: {e:?}");
            }
        }
        Ok(Self {
            client,
            instance: Arc::new(RwLock::new(None)),
            install_mutex: Mutex::new(()),
            database,
        })
    }
    pub async fn is_running(&self) -> bool {
        self.instance.read().await.is_some()
    }
    pub async fn ensure_installed(&self) -> Result<(), Error> {
        let bin_path = Path::new(BIN_PATH);
        if !bin_path.exists() {
            self.update_farmer().await
        } else {
            Ok(())
        }
    }
    pub async fn update_farmer(&self) -> Result<(), Error> {
        info!("Checking for Farmer Installation");
        let install_mutex = self.install_mutex.lock().await;
        let bin_path = Path::new(BIN_PATH);
        info!("Fetching Remote Manifest");
        let current_manifest = Self::fetch_manifest(&self.client).await?;
        let channel = match get_config_key(&self.database, "farmer_update_channel").await {
            Ok(Some(channel_entry)) => match channel_entry.value.to_ascii_lowercase().as_str() {
                "beta" => UpdateChannel::Beta,
                "release" => UpdateChannel::Release,
                _ => {
                    warn!("Invalid Update Channel: {}", channel_entry.value);
                    UpdateChannel::Release
                }
            },
            _ => UpdateChannel::Release,
        };
        let mut install = true;
        if bin_path.exists() {
            info!("Checking Current Binary Version");
            if let Some(bin_version) = Self::get_binary_version(bin_path).await {
                match &channel {
                    UpdateChannel::Release => {
                        install = bin_version >= current_manifest.current_version;
                    }
                    UpdateChannel::Beta => {
                        install = bin_version
                            >= *current_manifest
                                .beta_version
                                .as_ref()
                                .unwrap_or(&current_manifest.current_version);
                    }
                }
            }
        }
        if install {
            info!("Installing Farmer - Using {channel:?} Channel...");
            let version = match &channel {
                UpdateChannel::Release => current_manifest.current_version,
                UpdateChannel::Beta => current_manifest
                    .beta_version
                    .unwrap_or(current_manifest.current_version),
            };
            let download_url = Self::get_download_url(&version.to_string())?;
            Self::download_file(&self.client, TMP_PATH, &download_url).await?;
            Self::set_executable_bit(TMP_PATH).await?;

            // Verify downloaded binary
            let downloaded_version = Self::get_binary_version(TMP_PATH)
                .await
                .ok_or(Error::other("Failed to read downloaded binary version"))?;
            if downloaded_version != version {
                return Err(Error::other("Downloaded binary version mismatch"));
            }
            Self::swap_binaries().await?;
        }
        drop(install_mutex);
        Ok(())
    }

    async fn get_binary_version<P: AsRef<Path>>(path: P) -> Option<Version> {
        let path = path.as_ref();
        let output = Command::new(path).arg("--version").output().await.ok()?;
        if !output.status.success() {
            return None;
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut sections = stdout.split_whitespace();
        if let Some(v) = sections.next() {
            let maybe_version = Version::parse(v).ok();
            if maybe_version.is_some() {
                return maybe_version;
            }
        }
        if let Some(v) = sections.next() {
            Version::parse(v).ok()
        } else {
            None
        }
    }
    async fn fetch_manifest(client: &Client) -> Result<FastFarmerManifest, Error> {
        let response = client
            .get(FAST_FARMER_MANIFEST_URL)
            .send()
            .await
            .map_err(Error::other)?
            .error_for_status()
            .map_err(Error::other)?
            .text()
            .await
            .map_err(Error::other)?;
        serde_yaml::from_str(&response).map_err(Error::other)
    }
    async fn swap_binaries() -> Result<(), Error> {
        if Path::new(BACKUP_PATH).exists() {
            let _ = remove_file(BACKUP_PATH).await;
        }
        rename(BIN_PATH, BACKUP_PATH).await?;
        copy(TMP_PATH, BIN_PATH).await.map(|_| ())
    }
    async fn set_executable_bit<P: AsRef<Path>>(path: P) -> Result<(), Error> {
        let path = path.as_ref();
        let mut perms = metadata(path).await?.permissions();
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
        set_permissions(path, perms).await
    }
    async fn download_file(client: &Client, path: &str, download_url: &str) -> Result<(), Error> {
        info!("Downloading from {download_url} to {path}");
        let mut resp = client
            .get(download_url)
            .send()
            .await
            .map_err(Error::other)?
            .error_for_status()
            .map_err(Error::other)?;
        let mut out = File::create(path).await.map_err(Error::other)?;
        while let Some(chunk) = resp.chunk().await.map_err(Error::other)? {
            out.write_all(&chunk).await?;
        }
        Ok(())
    }
    fn get_download_url(version: &str) -> Result<String, Error> {
        let arch = env::consts::ARCH;
        Ok(format!(
            "https://builds.druid.garden/{}/{}/ff_giga",
            version,
            if arch == "x86_64" {
                "amd64"
            } else if arch == "aarch64" {
                arch
            } else {
                return Err(Error::new(
                    ErrorKind::Unsupported,
                    "Unsupported Platform for Auto Updates",
                ));
            }
        ))
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
                            .map_err(Error::other)?
                            .as_bytes(),
                    )
                    .await?;
                let child = Command::new("fast_farmer_gh.app")
                    .stdout(Stdio::null())
                    .stdin(Stdio::null())
                    .stderr(Stdio::null())
                    .arg("-c")
                    .arg("/tmp/fast_farmer_config.yaml")
                    .arg("run")
                    .arg("-m")
                    .arg("cli")
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
    pub async fn recent_farmer_stats(&self) -> Result<Vec<FarmerStats>, Error> {
        let mut url = Self::farmer_url(&self.database).await?;
        url.set_path("/stats");
        self.client
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
        url.set_scheme(if url.scheme() == "https" { "wss" } else { "ws" })
            .map_err(|_| Error::new(ErrorKind::InvalidInput, "Invalid URL"))?;
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
        tokio::spawn(async move {
            if let Some(mut v) = instance.write().await.take() {
                println!("Killing Farmer");
                let _ = v.kill().await;
                println!("Farmer Killed");
                drop(v);
            }
        });
    }
}

#[interval(10_000)]
pub async fn update_local_stats(
    database: State<SqlitePool>,
    farmer_manager: State<FarmerManager>,
) -> Result<(), Error> {
    if farmer_manager.0.is_running().await {
        let mut url = FarmerManager::farmer_url(&database).await?;
        url.set_path("/stats");
        let stats = farmer_manager.0.recent_farmer_stats().await?;
        for farmer_stats in stats {
            if !has_farmer_stats(&database, farmer_stats.challenge_hash, farmer_stats.sp_hash)
                .await?
            {
                save_farmer_stats(&database, farmer_stats).await?;
            }
        }
        let mut older_than_timestamp = OffsetDateTime::now_utc();
        let stat_days_to_keep = get_config_key(&database, "stats_days_saved")
            .await?
            .map(|c| u64::from_str(&c.value).unwrap_or(30))
            .unwrap_or(30);
        older_than_timestamp -= Duration::new(stat_days_to_keep * 24 * 60 * 60, 0);
        prune_farmer_stats(&database, older_than_timestamp).await?;
    }
    Ok(())
}
