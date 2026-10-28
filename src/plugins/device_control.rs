//! Opt-in, read-only replies to the claimed owner's monitoring requests.
use crate::config::ConfigManager;
use crate::plugins::device_heartbeat::{load_heartbeat_config, DeviceHeartbeatPlugin};
use crate::plugins::farmer::FarmerManager;
use crate::plugins::system_monitor::SystemMonitorPlugin;
use portfu::prelude::futures_util::{SinkExt, StreamExt};
use portfu::prelude::serde_json::{self, json, Value};
use portfu::prelude::tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{protocol::WebSocketConfig, Message},
};
use portfu::prelude::State;
use portfu_macros::{get, interval};
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::io::{Error, ErrorKind};
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

const MAX_MESSAGE: usize = 16 * 1024;
const MAX_RESULT: usize = 64 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Default, Clone, Serialize)]
pub struct ControlHealth {
    pub enabled: bool,
    pub connected: bool,
    pub reconnects: u64,
    pub completed_requests: u64,
    pub last_error: Option<String>,
}

#[derive(Default)]
pub struct DeviceControlPlugin {
    health: RwLock<ControlHealth>,
}

#[derive(Clone, PartialEq)]
struct ConnectionSettings {
    url: String,
    token: String,
    name: String,
}

async fn settings(config: &RwLock<ConfigManager>) -> Result<Option<ConnectionSettings>, Error> {
    let enabled = config
        .read()
        .await
        .get("heartbeat.control_enabled")
        .await
        .is_some_and(|entry| entry.value == "true");
    if !enabled {
        return Ok(None);
    }
    let heartbeat = load_heartbeat_config(config).await?;
    if !heartbeat.enabled {
        return Ok(None);
    }
    let Some(token) = heartbeat
        .device_token
        .filter(|token| !token.trim().is_empty())
    else {
        return Ok(None);
    };
    let mut url = heartbeat.base_url;
    if url.scheme() != "https" {
        return Err(Error::new(
            ErrorKind::PermissionDenied,
            "Monitoring requires HTTPS",
        ));
    }
    url.set_scheme("wss")
        .map_err(|_| Error::other("Invalid monitoring URL"))?;
    url.set_path("/events/device-control");
    Ok(Some(ConnectionSettings {
        url: url.to_string(),
        token,
        name: heartbeat.name,
    }))
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Inbound {
    Command {
        request_id: String,
        command: String,
        #[serde(default)]
        args: Value,
    },
    Error {
        #[serde(rename = "message")]
        _message: String,
    },
}

fn permitted(command: &str, args: &Value) -> bool {
    matches!(command, "status" | "stats" | "drives.list")
        && (args.is_null() || args.as_object().is_some_and(|map| map.is_empty()))
}

fn result_message(
    id: &str,
    device: &str,
    command: &str,
    output: Result<Value, &str>,
) -> Result<String, Error> {
    let value = match output {
        Ok(output) => {
            json!({"type":"device_command_result","request_id":id,"device_id":device,"command":command,"status":"ok","output":output,"error":null})
        }
        Err(error) => {
            json!({"type":"device_command_result","request_id":id,"device_id":device,"command":command,"status":"error","output":null,"error":error})
        }
    };
    let serialized = serde_json::to_string(&value).map_err(Error::other)?;
    if serialized.len() > MAX_RESULT {
        return Err(Error::other("Monitoring result exceeds limit"));
    }
    Ok(serialized)
}

async fn read_command(
    command: &str,
    heartbeat: &DeviceHeartbeatPlugin,
    farmer: &FarmerManager,
    monitor: &SystemMonitorPlugin,
) -> Result<Value, Error> {
    match command {
        "status" => Ok(json!({
            "firmware_version": env!("CARGO_PKG_VERSION"),
            "uptime_seconds": sysinfo::System::uptime(),
            "farmer": farmer.farmer_status().await,
            "heartbeat": heartbeat.current_health().await,
        })),
        "stats" => {
            let mut rows = farmer.recent_farmer_stats().await?;
            rows.truncate(32);
            serde_json::to_value(rows).map_err(Error::other)
        }
        "drives.list" => {
            let mut disks = monitor.get_disk_info().await?;
            disks.truncate(64);
            for disk in &mut disks {
                disk.partitions.truncate(128);
            }
            serde_json::to_value(disks).map_err(Error::other)
        }
        _ => Err(Error::new(
            ErrorKind::PermissionDenied,
            "Command is not allowed",
        )),
    }
}

async fn connection(
    settings_snapshot: &ConnectionSettings,
    plugin: &DeviceControlPlugin,
    config: &RwLock<ConfigManager>,
    heartbeat: &DeviceHeartbeatPlugin,
    farmer: &FarmerManager,
    monitor: &SystemMonitorPlugin,
) -> Result<(), Error> {
    let ws_config = WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE))
        .max_frame_size(Some(MAX_MESSAGE))
        .write_buffer_size(0)
        .max_write_buffer_size(MAX_RESULT + 1024);
    let (mut socket, _) = tokio::time::timeout(
        IO_TIMEOUT,
        connect_async_with_config(settings_snapshot.url.as_str(), Some(ws_config), false),
    )
    .await
    .map_err(|_| Error::other("Monitoring connection timed out"))?
    .map_err(|_| Error::other("Monitoring connection failed"))?;
    let device_id = heartbeat.current_health().await.device_id;
    let hello = json!({"type":"device_hello","device_token":settings_snapshot.token,"device_id":device_id,"name":settings_snapshot.name,"private_ip":null,"identity_secret":heartbeat.identity_proof()});
    tokio::time::timeout(
        IO_TIMEOUT,
        socket.send(Message::Text(hello.to_string().into())),
    )
    .await
    .map_err(|_| Error::other("Monitoring handshake timed out"))?
    .map_err(|_| Error::other("Monitoring handshake failed"))?;
    // The protocol has no explicit hello acknowledgement. Connected describes
    // transport state; server rejection immediately clears it below.
    {
        let mut health = plugin.health.write().await;
        health.connected = true;
        health.last_error = None;
    }
    let mut config_poll = tokio::time::interval(Duration::from_secs(5));
    config_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_received = Instant::now();
    let mut last_command: Option<Instant> = None;
    let mut recent = VecDeque::new();
    loop {
        tokio::select! {
            _ = config_poll.tick() => {
                if settings(config).await?.as_ref() != Some(settings_snapshot) { return Ok(()); }
                if last_received.elapsed() > Duration::from_secs(60) { return Err(Error::other("Monitoring connection idle timeout")); }
            }
            incoming = socket.next() => {
                let message = incoming.ok_or_else(|| Error::other("Monitoring connection closed"))?
                    .map_err(|_| Error::other("Invalid monitoring transport frame"))?;
                last_received = Instant::now();
                let response = match message {
                    Message::Ping(payload) => Some(Message::Pong(payload)),
                    Message::Pong(_) => None,
                    Message::Close(_) => return Ok(()),
                    Message::Text(text) => {
                        if text.len() > MAX_MESSAGE { return Err(Error::other("Monitoring message too large")); }
                        let incoming: Inbound = serde_json::from_str(&text).map_err(|_| Error::other("Invalid monitoring message"))?;
                        match incoming {
                            Inbound::Error { .. } => return Err(Error::other("Website rejected monitoring session")),
                            Inbound::Command { request_id, command, args } => {
                                if request_id.is_empty() || request_id.len() > 128 || command.len() > 64 {
                                    return Err(Error::other("Invalid monitoring request fields"));
                                }
                                if recent.contains(&request_id) { continue; }
                                if last_command.is_some_and(|last| last.elapsed() < Duration::from_secs(1)) {
                                    return Err(Error::other("Monitoring request rate exceeded"));
                                }
                                last_command = Some(Instant::now());
                                recent.push_back(request_id.clone());
                                if recent.len() > 128 { recent.pop_front(); }
                                let output = if !permitted(&command, &args) {
                                    Err("Only read-only status, stats, and drives.list with empty arguments are enabled")
                                } else {
                                    match tokio::time::timeout(Duration::from_secs(5), read_command(&command, heartbeat, farmer, monitor)).await {
                                        Ok(Ok(value)) => Ok(value),
                                        Ok(Err(_)) => Err("Monitoring data is currently unavailable"),
                                        Err(_) => Err("Monitoring request timed out"),
                                    }
                                };
                                // Consent and credentials may change while gathering statistics.
                                if settings(config).await?.as_ref() != Some(settings_snapshot) { return Ok(()); }
                                let response = result_message(&request_id, &device_id, &command, output)
                                    .or_else(|_| result_message(&request_id, &device_id, &command, Err("Monitoring result exceeds limit")))?;
                                plugin.health.write().await.completed_requests += 1;
                                Some(Message::Text(response.into()))
                            }
                        }
                    }
                    _ => return Err(Error::other("Unsupported monitoring frame")),
                };
                if let Some(response) = response {
                    tokio::time::timeout(IO_TIMEOUT, socket.send(response)).await
                        .map_err(|_| Error::other("Monitoring send timed out"))?
                        .map_err(|_| Error::other("Monitoring send failed"))?;
                }
            }
        }
    }
}

#[interval(1000)]
pub async fn run_device_control(
    plugin: State<DeviceControlPlugin>,
    config: State<RwLock<ConfigManager>>,
    heartbeat: State<DeviceHeartbeatPlugin>,
    farmer: State<FarmerManager>,
    monitor: State<SystemMonitorPlugin>,
) -> Result<(), Error> {
    tokio::select! {
        result = control_loop(plugin, config, heartbeat, farmer, monitor) => result,
        _ = portfu::pfcore::signal::await_termination() => Ok(()),
    }
}

async fn control_loop(
    plugin: State<DeviceControlPlugin>,
    config: State<RwLock<ConfigManager>>,
    heartbeat: State<DeviceHeartbeatPlugin>,
    farmer: State<FarmerManager>,
    monitor: State<SystemMonitorPlugin>,
) -> Result<(), Error> {
    let mut failures = 0u32;
    loop {
        let snapshot = match settings(&config.0).await {
            Ok(Some(settings)) => settings,
            Ok(None) => {
                let mut health = plugin.health.write().await;
                health.enabled = false;
                health.connected = false;
                health.last_error = None;
                drop(health);
                failures = 0;
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
            Err(_) => {
                plugin.health.write().await.last_error =
                    Some("Invalid monitoring configuration".into());
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };
        {
            let mut health = plugin.health.write().await;
            health.enabled = true;
            health.reconnects += 1;
        }
        let started = Instant::now();
        let outcome = connection(
            &snapshot,
            &plugin.0,
            &config.0,
            &heartbeat.0,
            &farmer.0,
            &monitor.0,
        )
        .await;
        {
            let mut health = plugin.health.write().await;
            health.connected = false;
            health.last_error = outcome.err().map(|error| error.to_string());
        }
        failures = if started.elapsed() > Duration::from_secs(60) {
            0
        } else {
            failures.saturating_add(1).min(6)
        };
        let delay = (1u64 << failures).min(60) + rand::thread_rng().gen_range(0..5);
        for _ in 0..delay {
            tokio::time::sleep(Duration::from_secs(1)).await;
            if settings(&config.0).await.ok().flatten().as_ref() != Some(&snapshot) {
                failures = 0;
                break;
            }
        }
    }
}

#[get("/api/system/control/health", output = "json", eoutput = "bytes")]
pub async fn control_health(plugin: State<DeviceControlPlugin>) -> Result<ControlHealth, Error> {
    Ok(plugin.health.read().await.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn remote_actions_are_read_only_and_argument_free() {
        for name in ["status", "stats", "drives.list"] {
            assert!(permitted(name, &json!({})));
        }
        for name in [
            "reboot",
            "logs",
            "config.get",
            "config.set",
            "drives.manage",
            "update.apply",
            "sh",
            "STATUS",
        ] {
            assert!(!permitted(name, &Value::Null));
        }
        assert!(!permitted("status", &json!({"command":"reboot"})));
        assert!(!permitted("stats", &json!([])));
    }
    #[test]
    fn results_match_website_envelope_and_limit_size() {
        let result: Value = serde_json::from_str(
            &result_message(
                "cmd_1",
                "device_1",
                "status",
                Ok(json!({"uptime_seconds":4})),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(result["type"], "device_command_result");
        assert_eq!(result["request_id"], "cmd_1");
        assert_eq!(result["device_id"], "device_1");
        assert_eq!(result["status"], "ok");
        assert!(result["error"].is_null());
        assert!(
            result_message("id", "device", "stats", Ok(json!("x".repeat(MAX_RESULT)))).is_err()
        );
    }
}
