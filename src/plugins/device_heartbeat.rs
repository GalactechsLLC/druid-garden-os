use crate::config::{ConfigManager, DEFAULT_FULLNODE_WS_HOST};
use crate::models::config::AddConfigEntry;
use crate::plugins::device_identity::{self, DeviceIdentity};
use crate::plugins::farmer::{FarmerManager, FarmerStatus};
use crate::plugins::system_monitor::{NetworkInfo, SystemMonitorPlugin};
use log::{info, warn};
use portfu::prelude::serde_json;
use portfu::prelude::State;
use portfu_macros::{get, interval};
use rand::RngCore;
use reqwest::{Client, StatusCode, Url};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::io::{Error, ErrorKind};
use std::net::{IpAddr, Ipv4Addr};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex, RwLock};

const DEFAULT_INTERVAL_SECONDS: u64 = 300;
const DEFAULT_TIMEOUT_SECONDS: u64 = 10;
const MIN_INTERVAL_SECONDS: u64 = 60;
const MIN_TIMEOUT_SECONDS: u64 = 3;
const MAX_INTERVAL_SECONDS: u64 = 600;
const MAX_TIMEOUT_SECONDS: u64 = 60;
const MAX_RESPONSE_BYTES: usize = 16 * 1024;
const TOKEN_WARNING_COOLDOWN_SECONDS: u64 = 30 * 60;

#[derive(Clone)]
pub struct HeartbeatConfig {
    pub enabled: bool,
    pub base_url: Url,
    pub interval_seconds: u64,
    pub timeout_seconds: u64,
    pub device_token: Option<String>,
    pub name: String,
}

#[derive(Default, Debug, Clone, Serialize)]
pub struct HeartbeatHealth {
    pub device_id: String,
    pub firmware_version: String,
    pub legacy_device_id: Option<String>,
    pub claimed: bool,
    pub token_status: String,
    pub telemetry_recorded: bool,
    pub last_success_unix: Option<u64>,
    pub consecutive_failures: u64,
    pub success_total: u64,
    pub failure_total: u64,
    pub last_latency_ms: Option<u128>,
}

#[derive(Debug)]
struct HeartbeatRuntimeState {
    next_attempt_at: Instant,
    current_backoff_seconds: u64,
    last_token_warning_at: Option<Instant>,
    health: HeartbeatHealth,
}

#[derive(Serialize, Clone)]
pub struct HeartbeatPayload {
    pub protocol_version: u32,
    pub identity_secret: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub telemetry: Option<DeviceTelemetry>,
    pub device_id: String,
    pub private_ip: String,
    pub name: String,
    pub claimed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_token: Option<String>,
}

#[derive(Deserialize, Debug)]
struct HeartbeatResponse {
    protocol_version: u32,
    claimed: bool,
    token_status: String,
    telemetry_recorded: bool,
    ok: bool,
    public_ip: Option<String>,
}

pub struct DeviceHeartbeatPlugin {
    client: Client,
    device_id: String,
    identity: DeviceIdentity,
    legacy_device_id: Option<String>,
    runtime: Mutex<HeartbeatRuntimeState>,
    in_flight: Mutex<()>,
}

impl DeviceHeartbeatPlugin {
    pub async fn new(db: &SqlitePool, config: &RwLock<ConfigManager>) -> Result<Self, Error> {
        let identity =
            device_identity::load_or_create(std::path::Path::new(device_identity::IDENTITY_PATH))?;
        let device_id = identity.device_id();
        let legacy_device_id = migrate_device_id(db, config, &device_id).await?;
        info!("Heartbeat initialized with device_id={device_id}");
        let startup_delay = startup_jitter_seconds();
        Ok(Self {
            client: Client::builder()
                .https_only(true)
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(Error::other)?,
            in_flight: Mutex::new(()),
            device_id,
            identity,
            legacy_device_id,
            runtime: Mutex::new(HeartbeatRuntimeState {
                next_attempt_at: Instant::now() + Duration::from_secs(startup_delay),
                current_backoff_seconds: 0,
                last_token_warning_at: None,
                health: HeartbeatHealth::default(),
            }),
        })
    }

    pub(crate) fn identity_proof(&self) -> String {
        self.identity.proof()
    }

    pub async fn current_health(&self) -> HeartbeatHealth {
        let mut health = self.runtime.lock().await.health.clone();
        health.device_id = self.device_id.clone();
        health.legacy_device_id = self.legacy_device_id.clone();
        health.firmware_version = env!("CARGO_PKG_VERSION").to_string();
        health
    }

    async fn tick(
        &self,
        config: &RwLock<ConfigManager>,
        system_monitor: &SystemMonitorPlugin,
        farmer: &FarmerManager,
    ) -> Result<(), Error> {
        let Ok(_in_flight) = self.in_flight.try_lock() else {
            return Ok(());
        };
        let now = Instant::now();
        let mut runtime = self.runtime.lock().await;
        if now < runtime.next_attempt_at {
            return Ok(());
        }
        let heartbeat_config = load_heartbeat_config(config).await?;
        if !heartbeat_config.enabled {
            runtime.next_attempt_at = now + Duration::from_secs(DEFAULT_INTERVAL_SECONDS);
            return Ok(());
        }
        let private_ip = resolve_private_ip(system_monitor).await;
        let mut payload = build_payload(
            self.device_id.clone(),
            private_ip,
            heartbeat_config.name.clone(),
            heartbeat_config.device_token.clone(),
        );
        payload.identity_secret = self.identity.proof();
        let telemetry_enabled = config
            .read()
            .await
            .get("heartbeat.telemetry_enabled")
            .await
            .is_some_and(|v| v.value == "true");
        if telemetry_enabled && payload.device_token.is_some() {
            payload.telemetry = tokio::time::timeout(
                Duration::from_secs(5),
                collect_telemetry(system_monitor, farmer),
            )
            .await
            .ok()
            .and_then(Result::ok);
        }
        let token_present = payload.device_token.is_some();
        info!(
            "Heartbeat attempt started device_id={} token_present={}",
            payload.device_id, token_present
        );
        drop(runtime);
        let started_at = Instant::now();
        let result = send_heartbeat(&self.client, &heartbeat_config, &payload).await;
        let latency_ms = started_at.elapsed().as_millis();
        let now = Instant::now();
        let mut runtime = self.runtime.lock().await;
        match result {
            Ok(response) => {
                runtime.health.claimed = response.claimed;
                runtime.health.token_status = response.token_status;
                runtime.health.telemetry_recorded = response.telemetry_recorded;
                runtime.current_backoff_seconds = 0;
                runtime.health.success_total += 1;
                runtime.health.consecutive_failures = 0;
                runtime.health.last_latency_ms = Some(latency_ms);
                runtime.health.last_success_unix = Some(now_unix());
                let next_delay = jittered_interval_seconds(heartbeat_config.interval_seconds);
                runtime.next_attempt_at = now + Duration::from_secs(next_delay);
                info!(
                    "Heartbeat success device_id={} public_ip={} latency_ms={}",
                    self.device_id,
                    response.public_ip.unwrap_or_default(),
                    latency_ms
                );
            }
            Err(HeartbeatError::HttpStatus { status }) => {
                runtime.health.claimed = false;
                runtime.health.token_status = "unconfirmed".into();
                runtime.health.telemetry_recorded = false;
                runtime.health.failure_total += 1;
                runtime.health.consecutive_failures += 1;
                let retry_in_seconds = next_backoff_seconds(runtime.current_backoff_seconds);
                runtime.current_backoff_seconds = retry_in_seconds;
                runtime.next_attempt_at = now + Duration::from_secs(retry_in_seconds);
                if token_present
                    && token_warning_allowed(runtime.last_token_warning_at, now)
                    && is_token_invalid_hint(Some(status), "")
                {
                    warn!(
                        "Heartbeat token appears invalid/revoked; continuing without stopping heartbeats"
                    );
                    runtime.last_token_warning_at = Some(now);
                }
                warn!(
                    "Heartbeat failure status={} retry_in_seconds={}",
                    status, retry_in_seconds
                );
            }
            Err(HeartbeatError::Transport(message)) => {
                runtime.health.claimed = false;
                runtime.health.token_status = "unconfirmed".into();
                runtime.health.telemetry_recorded = false;
                runtime.health.failure_total += 1;
                runtime.health.consecutive_failures += 1;
                let retry_in_seconds = next_backoff_seconds(runtime.current_backoff_seconds);
                runtime.current_backoff_seconds = retry_in_seconds;
                runtime.next_attempt_at = now + Duration::from_secs(retry_in_seconds);
                warn!(
                    "Heartbeat failure error={} retry_in_seconds={}",
                    message, retry_in_seconds
                );
            }
            Err(HeartbeatError::InvalidResponse(message)) => {
                runtime.health.claimed = false;
                runtime.health.token_status = "unconfirmed".into();
                runtime.health.telemetry_recorded = false;
                runtime.health.failure_total += 1;
                runtime.health.consecutive_failures += 1;
                let retry_in_seconds = next_backoff_seconds(runtime.current_backoff_seconds);
                runtime.current_backoff_seconds = retry_in_seconds;
                runtime.next_attempt_at = now + Duration::from_secs(retry_in_seconds);
                warn!(
                    "Heartbeat failure invalid_response={} retry_in_seconds={}",
                    message, retry_in_seconds
                );
            }
        }
        Ok(())
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn startup_jitter_seconds() -> u64 {
    let mut rng = rand::thread_rng();
    5 + (rng.next_u64() % 11)
}

fn jittered_interval_seconds(interval_seconds: u64) -> u64 {
    let min = (interval_seconds as f64 * 0.9).floor().max(1.0) as u64;
    let max = (interval_seconds as f64 * 1.1).ceil().max(1.0) as u64;
    if max <= min {
        return min;
    }
    let mut rng = rand::thread_rng();
    min + (rng.next_u64() % (max - min + 1))
}

fn next_backoff_seconds(current_backoff: u64) -> u64 {
    if current_backoff == 0 {
        10
    } else {
        (current_backoff * 2).min(300)
    }
}

fn token_warning_allowed(last_warning: Option<Instant>, now: Instant) -> bool {
    last_warning
        .map(|last| now.duration_since(last) >= Duration::from_secs(TOKEN_WARNING_COOLDOWN_SECONDS))
        .unwrap_or(true)
}

fn is_private_ipv4(ip: Ipv4Addr) -> bool {
    ip.is_private()
}

fn select_private_ip(addresses: &[Ipv4Addr]) -> String {
    if let Some(ip) = addresses.iter().copied().find(|ip| is_private_ipv4(*ip)) {
        return ip.to_string();
    }
    if let Some(ip) = addresses.iter().copied().find(|v| !v.is_loopback()) {
        return ip.to_string();
    }
    "0.0.0.0".to_string()
}

fn build_payload(
    device_id: String,
    private_ip: String,
    name: String,
    device_token: Option<String>,
) -> HeartbeatPayload {
    HeartbeatPayload {
        protocol_version: 2,
        identity_secret: String::new(),
        telemetry: None,
        device_id,
        private_ip,
        name,
        claimed: false,
        device_token: device_token.filter(|t| !t.trim().is_empty()),
    }
}

async fn resolve_private_ip(system_monitor: &SystemMonitorPlugin) -> String {
    match system_monitor.get_network_info().await {
        Ok(networks) => {
            let mut ipv4_addresses = vec![];
            for network in networks {
                gather_ipv4_addresses(&network, &mut ipv4_addresses);
            }
            select_private_ip(&ipv4_addresses)
        }
        Err(e) => {
            warn!("Failed to resolve network interfaces for heartbeat private_ip: {e}");
            "0.0.0.0".to_string()
        }
    }
}

fn gather_ipv4_addresses(network: &NetworkInfo, output: &mut Vec<Ipv4Addr>) {
    for ip in &network.ip_addresses {
        if let IpAddr::V4(ipv4) = ip.address {
            output.push(ipv4);
        }
    }
}

#[derive(Debug)]
enum HeartbeatError {
    Transport(String),
    HttpStatus { status: StatusCode },
    InvalidResponse(String),
}

async fn send_heartbeat(
    client: &Client,
    config: &HeartbeatConfig,
    payload: &HeartbeatPayload,
) -> Result<HeartbeatResponse, HeartbeatError> {
    let mut url = config.base_url.clone();
    url.set_path("/api/v1/devices/heartbeat/v2");
    let mut response = client
        .post(url)
        .timeout(Duration::from_secs(config.timeout_seconds))
        .json(payload)
        .send()
        .await
        .map_err(|e| HeartbeatError::Transport(e.to_string()))?;
    let status = response.status();
    if !status.is_success() {
        return Err(HeartbeatError::HttpStatus { status });
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| HeartbeatError::Transport(e.to_string()))?
    {
        if chunk.len() > MAX_RESPONSE_BYTES.saturating_sub(body.len()) {
            return Err(HeartbeatError::InvalidResponse(
                "response exceeds size limit".into(),
            ));
        }
        body.extend_from_slice(&chunk);
    }
    let parsed: HeartbeatResponse = serde_json::from_slice(&body)
        .map_err(|e| HeartbeatError::InvalidResponse(e.to_string()))?;
    if !parsed.ok || parsed.protocol_version != 2 {
        return Err(HeartbeatError::InvalidResponse(
            "response `ok` was false".to_string(),
        ));
    }
    Ok(parsed)
}

fn is_token_invalid_hint(status: Option<StatusCode>, body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    let body_hint = lower.contains("token")
        && (lower.contains("invalid") || lower.contains("revoked") || lower.contains("expired"));
    let status_hint = matches!(
        status,
        Some(StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
    );
    body_hint || status_hint
}

pub fn validate_heartbeat_key_value(key: &str, value: &str) -> Result<(), Error> {
    match key {
        "heartbeat.interval_seconds" => {
            let parsed = value.parse::<u64>().map_err(|_| {
                Error::new(
                    ErrorKind::InvalidInput,
                    "heartbeat.interval_seconds must be a positive integer",
                )
            })?;
            if !(MIN_INTERVAL_SECONDS..=MAX_INTERVAL_SECONDS).contains(&parsed) {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "heartbeat.interval_seconds must be between 60 and 600",
                ));
            }
        }
        "heartbeat.timeout_seconds" => {
            let parsed = value.parse::<u64>().map_err(|_| {
                Error::new(
                    ErrorKind::InvalidInput,
                    "heartbeat.timeout_seconds must be a positive integer",
                )
            })?;
            if !(MIN_TIMEOUT_SECONDS..=MAX_TIMEOUT_SECONDS).contains(&parsed) {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "heartbeat.timeout_seconds must be between 3 and 60",
                ));
            }
        }
        "heartbeat.base_url" => {
            let url = Url::parse(value).map_err(|_| {
                Error::new(
                    ErrorKind::InvalidInput,
                    "heartbeat.base_url must be a valid URL",
                )
            })?;
            if url.scheme() != "https"
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "heartbeat.base_url must be HTTPS without credentials, query, or fragment",
                ));
            }
        }
        "heartbeat.enabled" | "heartbeat.control_enabled" | "heartbeat.telemetry_enabled"
            if value != "true" && value != "false" =>
        {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "Heartbeat enable flags must be true or false",
            ));
        }
        "heartbeat.name" if value.trim().is_empty() || value.len() > 256 => {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "heartbeat.name must contain 1 to 256 bytes",
            ));
        }
        "heartbeat.api_key" | "heartbeat.device_token" if value.len() > 256 => {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "device key exceeds 256 bytes",
            ));
        }
        _ => {}
    }
    Ok(())
}

fn default_base_url() -> Result<Url, Error> {
    Url::parse(&format!("https://{DEFAULT_FULLNODE_WS_HOST}"))
        .map_err(|e| Error::other(format!("Invalid default heartbeat URL: {e}")))
}

pub(crate) async fn load_heartbeat_config(
    config: &RwLock<ConfigManager>,
) -> Result<HeartbeatConfig, Error> {
    let config = config.read().await;
    let enabled = config
        .get("heartbeat.enabled")
        .await
        .map(|v| v.value.eq_ignore_ascii_case("true"))
        .unwrap_or(true);
    let base_url = match config.get("heartbeat.base_url").await {
        Some(entry) if !entry.value.trim().is_empty() => {
            validate_heartbeat_key_value("heartbeat.base_url", entry.value.trim())?;
            Url::parse(entry.value.trim()).map_err(Error::other)?
        }
        _ => default_base_url()?,
    };
    let interval_seconds = config
        .get("heartbeat.interval_seconds")
        .await
        .and_then(|v| v.value.parse::<u64>().ok())
        .filter(|v| (MIN_INTERVAL_SECONDS..=MAX_INTERVAL_SECONDS).contains(v))
        .unwrap_or(DEFAULT_INTERVAL_SECONDS);
    let timeout_seconds = config
        .get("heartbeat.timeout_seconds")
        .await
        .and_then(|v| v.value.parse::<u64>().ok())
        .filter(|v| (MIN_TIMEOUT_SECONDS..=MAX_TIMEOUT_SECONDS).contains(v))
        .unwrap_or(DEFAULT_TIMEOUT_SECONDS);
    let device_token = match config.get("heartbeat.api_key").await {
        Some(v) if !v.value.trim().is_empty() => Some(v.value),
        _ => config.get("heartbeat.device_token").await.map(|v| v.value),
    }
    .map(|v| v.trim().to_string())
    .filter(|v| !v.is_empty());
    let name = config
        .get("heartbeat.name")
        .await
        .map(|v| v.value)
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(fallback_device_name);
    Ok(HeartbeatConfig {
        enabled,
        base_url,
        interval_seconds,
        timeout_seconds,
        device_token,
        name,
    })
}

fn fallback_device_name() -> String {
    sysinfo::System::host_name().unwrap_or_else(|| "Druid Garden Device".to_string())
}

async fn migrate_device_id(
    db: &SqlitePool,
    config: &RwLock<ConfigManager>,
    new_id: &str,
) -> Result<Option<String>, Error> {
    let mut config = config.write().await;
    let previous = config.get("heartbeat.device_id").await.map(|v| v.value);
    let old = previous.filter(|id| !id.starts_with("dg2_") && !id.is_empty());
    if let Some(old) = &old {
        let key = "heartbeat.legacy_device_id";
        config
            .set(
                key,
                AddConfigEntry {
                    key: key.into(),
                    value: old.clone(),
                    last_value: String::new(),
                    category: "system".into(),
                    system: 1,
                },
                Some(db),
            )
            .await?;
    }
    let key = "heartbeat.device_id";
    config
        .set(
            key,
            AddConfigEntry {
                key: key.into(),
                value: new_id.into(),
                last_value: String::new(),
                category: "system".into(),
                system: 1,
            },
            Some(db),
        )
        .await?;
    Ok(config
        .get("heartbeat.legacy_device_id")
        .await
        .map(|v| v.value))
}

#[derive(Clone, Serialize)]
pub struct DeviceTelemetry {
    schema_version: u32,
    firmware_version: String,
    update_channel: String,
    uptime_seconds: u64,
    cpu_percent: f32,
    memory_total_bytes: u64,
    memory_used_bytes: u64,
    disk_total_bytes: u64,
    disk_used_bytes: u64,
    farmer_running: Option<bool>,
}
async fn collect_telemetry(
    monitor: &SystemMonitorPlugin,
    farmer: &FarmerManager,
) -> Result<DeviceTelemetry, Error> {
    let cpu = monitor.get_cpu_info().await?;
    let memory = monitor.get_memory_info().await?;
    let disks = monitor.get_disk_info().await?;
    let total = disks
        .iter()
        .fold(0u64, |sum, d| sum.saturating_add(d.total));
    let used = disks
        .iter()
        .fold(0u64, |sum, d| sum.saturating_add(d.used))
        .min(total);
    let channel = dg_edge_updater::configured_channel().unwrap_or_else(|_| "unknown".into());
    let farmer_running = match farmer.farmer_status().await {
        FarmerStatus::Running => Some(true),
        FarmerStatus::Stopped | FarmerStatus::Exited(_) => Some(false),
        FarmerStatus::Unknown => None,
    };
    Ok(DeviceTelemetry {
        schema_version: 1,
        firmware_version: env!("CARGO_PKG_VERSION").into(),
        update_channel: if matches!(channel.as_str(), "stable" | "beta") {
            channel
        } else {
            "unknown".into()
        },
        uptime_seconds: sysinfo::System::uptime(),
        cpu_percent: if cpu.global_usage.is_finite() {
            cpu.global_usage.clamp(0.0, 100.0)
        } else {
            0.0
        },
        memory_total_bytes: memory.total,
        memory_used_bytes: memory.used.min(memory.total),
        disk_total_bytes: total,
        disk_used_bytes: used,
        farmer_running,
    })
}

#[interval(1000)]
pub async fn run_device_heartbeat(
    heartbeat: State<DeviceHeartbeatPlugin>,
    config: State<RwLock<ConfigManager>>,
    system_monitor: State<SystemMonitorPlugin>,
    farmer: State<FarmerManager>,
) -> Result<(), Error> {
    heartbeat
        .0
        .tick(
            config.0.as_ref(),
            system_monitor.0.as_ref(),
            farmer.0.as_ref(),
        )
        .await
}

#[get("/api/system/heartbeat/health", output = "json", eoutput = "bytes")]
pub async fn heartbeat_health(
    heartbeat: State<DeviceHeartbeatPlugin>,
) -> Result<HeartbeatHealth, Error> {
    Ok(heartbeat.0.current_health().await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn payload_includes_token_only_when_non_empty() {
        let with_token = build_payload(
            "device-id".to_string(),
            "192.168.0.10".to_string(),
            "name".to_string(),
            Some("dgt_abc.def".to_string()),
        );
        assert_eq!(with_token.device_token.as_deref(), Some("dgt_abc.def"));

        let empty_token = build_payload(
            "device-id".to_string(),
            "192.168.0.10".to_string(),
            "name".to_string(),
            Some("   ".to_string()),
        );
        assert!(empty_token.device_token.is_none());
    }

    #[test]
    fn backoff_progression_and_cap() {
        assert_eq!(next_backoff_seconds(0), 10);
        assert_eq!(next_backoff_seconds(10), 20);
        assert_eq!(next_backoff_seconds(20), 40);
        assert_eq!(next_backoff_seconds(40), 80);
        assert_eq!(next_backoff_seconds(160), 300);
        assert_eq!(next_backoff_seconds(300), 300);
    }

    #[test]
    fn private_ip_resolver_fallback_order() {
        let addresses = vec![
            Ipv4Addr::new(127, 0, 0, 1),
            Ipv4Addr::new(8, 8, 8, 8),
            Ipv4Addr::new(192, 168, 1, 11),
        ];
        assert_eq!(select_private_ip(&addresses), "192.168.1.11");

        let public_only = vec![Ipv4Addr::new(8, 8, 8, 8), Ipv4Addr::new(127, 0, 0, 1)];
        assert_eq!(select_private_ip(&public_only), "8.8.8.8");

        let loopback_only = vec![Ipv4Addr::new(127, 0, 0, 1)];
        assert_eq!(select_private_ip(&loopback_only), "0.0.0.0");
    }

    #[test]
    fn config_validation_rejects_invalid_values() {
        assert!(validate_heartbeat_key_value("heartbeat.interval_seconds", "59").is_err());
        assert!(validate_heartbeat_key_value("heartbeat.interval_seconds", "601").is_err());
        assert!(validate_heartbeat_key_value("heartbeat.timeout_seconds", "61").is_err());
        assert!(validate_heartbeat_key_value("heartbeat.base_url", "http://druid.garden").is_err());
        assert!(validate_heartbeat_key_value(
            "heartbeat.base_url",
            "https://user:secret@druid.garden"
        )
        .is_err());
        assert!(validate_heartbeat_key_value("heartbeat.timeout_seconds", "2").is_err());
        assert!(validate_heartbeat_key_value("heartbeat.base_url", "not_a_url").is_err());
        assert!(validate_heartbeat_key_value("heartbeat.interval_seconds", "300").is_ok());
        assert!(validate_heartbeat_key_value("heartbeat.timeout_seconds", "10").is_ok());
        assert!(validate_heartbeat_key_value("heartbeat.base_url", "https://druid.garden").is_ok());
    }

    #[tokio::test]
    #[ignore = "requires loopback socket permissions for mock HTTP server"]
    async fn startup_heartbeat_sends_expected_payload() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0u8; 4096];
            let size = stream.read(&mut request).await.unwrap();
            let request_text = String::from_utf8_lossy(&request[..size]);
            assert!(request_text.contains("POST /api/v1/devices/heartbeat/v2 HTTP/1.1"));
            assert!(request_text.contains("\"device_id\":\"dev-1\""));
            assert!(request_text.contains("\"private_ip\":\"192.168.1.2\""));
            let response = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 131\r\n\r\n{\"ok\":true,\"public_ip\":\"100.1.2.3\",\"protocol_version\":2,\"claimed\":false,\"token_status\":\"not_configured\",\"telemetry_recorded\":false}";
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        let client = Client::new();
        let config = HeartbeatConfig {
            enabled: true,
            base_url: Url::parse(&format!("http://{address}")).unwrap(),
            interval_seconds: 300,
            timeout_seconds: 10,
            device_token: None,
            name: "device-name".to_string(),
        };
        let payload = build_payload(
            "dev-1".to_string(),
            "192.168.1.2".to_string(),
            "device-name".to_string(),
            None,
        );
        let public_ip = send_heartbeat(&client, &config, &payload).await.unwrap();
        assert_eq!(public_ip.public_ip.as_deref(), Some("100.1.2.3"));
        server.await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires loopback socket permissions for mock HTTP server"]
    async fn retries_on_server_failure_then_resets_after_success() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for response in [
                "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 18\r\n\r\n{\"error\":\"failed\"}",
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 131\r\n\r\n{\"ok\":true,\"public_ip\":\"100.1.2.3\",\"protocol_version\":2,\"claimed\":false,\"token_status\":\"not_configured\",\"telemetry_recorded\":false}",
            ] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = vec![0u8; 4096];
                let _ = stream.read(&mut request).await.unwrap();
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let client = Client::new();
        let config = HeartbeatConfig {
            enabled: true,
            base_url: Url::parse(&format!("http://{address}")).unwrap(),
            interval_seconds: 300,
            timeout_seconds: 10,
            device_token: None,
            name: "device-name".to_string(),
        };
        let payload = build_payload(
            "dev-1".to_string(),
            "192.168.1.2".to_string(),
            "device-name".to_string(),
            None,
        );
        let first = send_heartbeat(&client, &config, &payload).await;
        assert!(matches!(first, Err(HeartbeatError::HttpStatus { .. })));
        let retry = next_backoff_seconds(0);
        assert_eq!(retry, 10);
        let second = send_heartbeat(&client, &config, &payload).await.unwrap();
        assert_eq!(second.public_ip.as_deref(), Some("100.1.2.3"));
        let reset_after_success = 0u64;
        assert_eq!(next_backoff_seconds(reset_after_success), 10);
        server.await.unwrap();
    }
}
