use dg_network_manager::all_devices;
use dg_network_manager::dbus_api::devices::Device;
use dg_sysfs::classes::block::disk::{DiskType, FileSystem, Partition};
use dg_sysfs::classes::block::BlockEnumerator;
use log::{debug, error, warn};
use nvml_wrapper::enum_wrappers::device::TemperatureSensor;
use nvml_wrapper::Nvml;
use portfu::prelude::serde_json::Value;
use portfu::prelude::{serde_json, State};
use portfu_macros::{get, interval};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Error;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use sysinfo::System;
use tokio::process::Command;
use tokio::sync::RwLock;

#[derive(Serialize)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub parent: Option<u32>,
    pub started: u64,
    pub cpu_usage: f32,
    pub memory_usage: u64,
}

#[derive(Serialize)]
pub struct SystemInfo {
    name: String,
    arch: String,
    kernel: String,
    os_version: String,
    hostname: String,
    uptime: u64,
    running_processes: Vec<ProcessInfo>,
}

#[derive(Serialize)]
pub struct MemoryInfo {
    pub free: u64,
    pub available: u64,
    pub total: u64,
    pub used: u64,
    pub free_swap: u64,
    pub total_swap: u64,
    pub used_swap: u64,
}

#[derive(Serialize)]
pub struct DiskInfo {
    pub dev_path: String,
    pub mount_path: Option<String>,
    pub file_system: Option<FileSystem>,
    pub name: String,
    pub total: u64,
    pub used: u64,
    pub usage: DiskUsage,
    pub partitions: Vec<Partition>,
    pub vendor: Option<String>,
    pub model: Option<String>,
    pub disk_type: DiskType,
}

#[derive(Serialize)]
pub struct DiskUsage {
    pub recently_read: u64,
    pub recently_writen: u64,
    pub total_read: u64,
    pub total_writen: u64,
}

#[derive(Serialize)]
pub struct IPAddressInfo {
    pub address: IpAddr,
    pub net_mask: u8,
    pub gateway: IpAddr,
}

#[derive(Serialize)]
pub struct NetworkInfo {
    pub name: String,
    pub ip_addresses: Vec<IPAddressInfo>,
    pub mac_address: String,
    pub data_downloaded: u64,
    pub data_uploaded: u64,
}

#[derive(Clone, Debug, Serialize)]
pub enum GpuType {
    Amd,
    Nvidia,
}

#[derive(Clone, Debug, Serialize)]
pub struct GpuInfo {
    pub index: u32,
    pub brand: GpuType,
    pub name: String,
    pub fan_speeds: Vec<u32>,
    pub gpu_usage: u32,
    pub memory_usage: u32,
    pub temperature: u32,
}

#[derive(Serialize)]
pub struct CpuUsage {
    pub name: String,
    pub brand: String,
    pub vendor: String,
    pub usage: f32,
    pub freq: u64,
}

#[derive(Serialize)]
pub struct CpuInfo {
    pub global_usage: f32,
    pub physical_count: usize,
    pub thread_count: usize,
    pub cpu_usage: Vec<CpuUsage>,
    pub load_averages: (f64, f64, f64),
}
#[derive(Deserialize)]
struct UnitValue {
    value: Value,
    // unit: String,
}
#[derive(Deserialize)]
struct AmdUsage {
    gfx_activity: UnitValue,
    // umc_activity: UnitValue,
    // mm_activity: UnitValue,
    // vcn_activity: Vec<UnitValue>,
    // jpeg_activity: Vec<UnitValue>,
}

#[derive(Deserialize)]
struct AmdTemperature {
    edge: Option<UnitValue>,
    hotspot: Option<UnitValue>,
    // mem: UnitValue,
}

#[derive(Deserialize)]
struct AmdFanUsage {
    // speed: u32,
    // max: u32,
    // rpm: u32,
    usage: Option<UnitValue>,
}

#[derive(Deserialize)]
struct AmdMemoryUsage {
    total_vram: UnitValue,
    used_vram: UnitValue,
    // free_vram: UnitValue,
    // total_visible_vram: UnitValue,
    // used_visible_vram: UnitValue,
    // free_visible_vram: UnitValue,
    // total_gtt: UnitValue,
    // used_gtt: UnitValue,
    // free_gtt: UnitValue,
}

#[derive(Deserialize)]
struct AmdGPUAsic {
    market_name: Option<String>,
    // vendor_id: Option<String>,
    // vendor_name: Option<String>,
    // subvendor_id: Option<String>,
    // device_id: Option<String>,
    // subsystem_id: Option<String>,
    // rev_id: Option<String>,
    // asic_serial: Option<String>,
    // oam_id: Option<String>,
    // num_compute_units: Option<String>,
    // target_graphics_version: Option<String>
}

#[derive(Deserialize)]
struct AmdGPUInfo {
    gpu: Option<u32>,
    asic: Option<AmdGPUAsic>,
}

#[derive(Deserialize)]
struct AmdGPUUsage {
    gpu: Option<u32>,
    usage: Option<AmdUsage>,
    temperature: Option<AmdTemperature>,
    fan: Option<AmdFanUsage>,
    mem_usage: Option<AmdMemoryUsage>,
}

#[derive(Debug)]
pub struct SystemMonitorPlugin {
    system: RwLock<System>,
    disks: RwLock<BlockEnumerator>,
    networks: RwLock<HashMap<String, Device>>,
    gpus: RwLock<Vec<GpuInfo>>,
    nvml: RwLock<Option<Nvml>>,
    cpu_count: usize,
    last_disk_update: AtomicU64,
    last_net_update: AtomicU64,
    detected_amd_gpu: AtomicBool,
}
impl SystemMonitorPlugin {
    pub async fn new() -> SystemMonitorPlugin {
        let mut system = System::new_all();
        //We Need to refresh it after a short time to ensure that the data can be calculated
        system.refresh_cpu_usage();
        let cpu_count = System::physical_core_count().unwrap_or_default();
        //Try TO Initialize Nvidia Interface
        let mut gpus = vec![];
        let nvml = match Nvml::init() {
            Ok(nvml) => {
                gpus.extend(Self::get_nvidia_gpu_info(&nvml));
                Some(nvml)
            }
            Err(e) => {
                warn!("Failed to initialize NVML interface, Not detecting Nvidia GPU Info. {e:?}");
                None
            }
        };
        //Detect AMD Devices
        let detected_amd_gpu = if SystemMonitorPlugin::has_amd_gpu_detection().await {
            gpus.extend(SystemMonitorPlugin::get_amd_gpu_info().await);
            AtomicBool::new(true)
        } else {
            AtomicBool::new(false)
        };
        SystemMonitorPlugin {
            system: RwLock::new(system),
            disks: RwLock::new(BlockEnumerator::new()),
            networks: RwLock::new(Default::default()),
            nvml: RwLock::new(nvml),
            gpus: RwLock::new(gpus),
            cpu_count,
            last_disk_update: AtomicU64::new(0),
            last_net_update: AtomicU64::new(0),
            detected_amd_gpu,
        }
    }
    fn get_nvidia_gpu_info(nvml: &Nvml) -> Vec<GpuInfo> {
        let count = nvml.device_count().unwrap_or_default();
        debug!("Detected {count} NVIDIA GPU(s)");
        let mut gpus = vec![];
        for device_id in 0..count {
            match nvml.device_by_index(device_id) {
                Ok(device) => {
                    let name = device.name().unwrap_or("Unknown".to_string());
                    debug!("Loading GPU {device_id}: {name}");
                    match device.utilization_rates() {
                        Ok(utilization) => {
                            let mut fan_speeds = vec![];
                            for fan_id in 0..device.num_fans().unwrap_or_default() {
                                fan_speeds.push(device.fan_speed(fan_id).unwrap_or_default());
                            }
                            gpus.push(GpuInfo {
                                index: device_id,
                                brand: GpuType::Nvidia,
                                name,
                                fan_speeds,
                                gpu_usage: utilization.gpu,
                                memory_usage: utilization.memory,
                                temperature: device
                                    .temperature(TemperatureSensor::Gpu)
                                    .unwrap_or_default(),
                            });
                        }
                        Err(e) => error!("Error Loading GPU at Index {device_id}. {e:?}"),
                    }
                }
                Err(e) => error!("Error Loading GPU at Index {device_id}. {e:?}"),
            }
        }
        gpus
    }
    async fn has_amd_gpu_detection() -> bool {
        match Command::new("amd-smi").args(["--help"]).output().await {
            Ok(output) => output.status.success(),
            Err(_) => false,
        }
    }
    async fn get_amd_gpu_info() -> Vec<GpuInfo> {
        match Command::new("amd-smi")
            .args([
                "metric",
                "--fan",
                "--usage",
                "--mem-usage",
                "--temperature",
                "--json",
            ])
            .output()
            .await
        {
            Ok(output) => {
                if !output.status.success() {
                    error!("amd-smi command failed. Is amd-smi installed and in your PATH?");
                    return vec![];
                }
                let gpu_info = match Command::new("amd-smi")
                    .args(["static", "--asic", "--json"])
                    .output()
                    .await
                {
                    Ok(output) => {
                        let json_str =
                            String::from_utf8_lossy(&output.stdout).replace("\"N/A\"", "null");
                        let lines = json_str.lines();
                        let mut clean_string = String::new();
                        let mut found_json_start = false;
                        for line in lines {
                            found_json_start = found_json_start || line.trim().starts_with('[');
                            if found_json_start {
                                clean_string.push_str(line);
                            }
                        }
                        match serde_json::from_str::<Vec<AmdGPUInfo>>(&clean_string) {
                            Ok(data) => data,
                            Err(e) => {
                                error!("Failed to parse amd-smi static output: {e}");
                                return vec![];
                            }
                        }
                    }
                    Err(e) => {
                        error!("amd-smi command failed. {e:?}");
                        vec![]
                    }
                };
                debug!("Detected {} AMD GPU(s)", gpu_info.len());
                let json_str = String::from_utf8_lossy(&output.stdout).replace("\"N/A\"", "null");
                let lines = json_str.lines();
                let mut clean_string = String::new();
                let mut found_json_start = false;
                for line in lines {
                    found_json_start = found_json_start || line.trim().starts_with('[');
                    if found_json_start {
                        clean_string.push_str(line);
                    }
                }
                match serde_json::from_str::<Vec<AmdGPUUsage>>(&clean_string) {
                    Ok(data) => data
                        .into_iter()
                        .map(|x| GpuInfo {
                            index: x.gpu.unwrap_or_default(),
                            name: gpu_info
                                .iter()
                                .find(|v| v.gpu == x.gpu && x.gpu.is_some())
                                .map(|v| {
                                    v.asic
                                        .as_ref()
                                        .map(|a| {
                                            a.market_name.clone().unwrap_or("Unknown".to_string())
                                        })
                                        .unwrap_or("Unknown".to_string())
                                })
                                .unwrap_or("Unknown".to_string()),
                            brand: GpuType::Amd,
                            fan_speeds: vec![x
                                .fan
                                .as_ref()
                                .map(|v| {
                                    if let Some(v) = &v.usage {
                                        value_to_u32(&v.value)
                                    } else {
                                        0
                                    }
                                })
                                .unwrap_or_default()],
                            gpu_usage: x
                                .usage
                                .as_ref()
                                .map(|v| value_to_u32(&v.gfx_activity.value))
                                .unwrap_or_default(),
                            memory_usage: x
                                .mem_usage
                                .as_ref()
                                .map(|v| {
                                    value_to_u32(&v.used_vram.value)
                                        / value_to_u32(&v.total_vram.value)
                                })
                                .unwrap_or_default(),
                            temperature: x
                                .temperature
                                .as_ref()
                                .map(|v| {
                                    if let Some(c) = &v.hotspot {
                                        value_to_u32(&c.value)
                                    } else if let Some(c) = &v.edge {
                                        value_to_u32(&c.value)
                                    } else {
                                        0
                                    }
                                })
                                .unwrap_or_default(),
                        })
                        .collect(),
                    Err(e) => {
                        error!("Failed to parse amd-smi metric output: {e}");
                        vec![]
                    }
                }
            }
            Err(e) => {
                error!("amd-smi command failed. {e:?}");
                vec![]
            }
        }
    }
    pub async fn get_system_info(&self) -> Result<SystemInfo, Error> {
        Ok(SystemInfo {
            name: System::name().unwrap_or("Unknown".to_string()),
            arch: System::cpu_arch(),
            kernel: System::kernel_version().unwrap_or("Unknown".to_string()),
            os_version: System::long_os_version().unwrap_or("Unknown".to_string()),
            hostname: System::host_name().unwrap_or("Unknown".to_string()),
            uptime: System::uptime(),
            running_processes: self
                .system
                .read()
                .await
                .processes()
                .iter()
                .map(|(pid, p)| ProcessInfo {
                    pid: pid.as_u32(),
                    name: p.name().to_string_lossy().to_string(),
                    parent: p.parent().map(|p| p.as_u32()),
                    started: p.start_time(),
                    cpu_usage: p.cpu_usage(),
                    memory_usage: p.memory(),
                })
                .collect(),
        })
    }
    pub async fn get_gpu_info(&self) -> Result<Vec<GpuInfo>, Error> {
        Ok(self.gpus.read().await.clone())
    }
    pub async fn get_cpu_info(&self) -> Result<CpuInfo, Error> {
        let cpu_usage = self
            .system
            .read()
            .await
            .cpus()
            .iter()
            .map(|c| CpuUsage {
                name: c.name().to_string(),
                brand: c.brand().to_string(),
                vendor: c.vendor_id().to_string(),
                usage: c.cpu_usage(),
                freq: c.frequency(),
            })
            .collect::<Vec<CpuUsage>>();
        let avg = System::load_average();
        Ok(CpuInfo {
            global_usage: self.system.read().await.global_cpu_usage(),
            physical_count: self.cpu_count,
            thread_count: cpu_usage.len(),
            load_averages: (avg.one, avg.five, avg.fifteen),
            cpu_usage,
        })
    }
    pub async fn get_memory_info(&self) -> Result<MemoryInfo, Error> {
        let (free, available, total, used, free_swap, total_swap, used_swap) = {
            let system = self.system.read().await;
            (
                system.free_memory(),
                system.available_memory(),
                system.total_memory(),
                system.used_memory(),
                system.free_swap(),
                system.total_swap(),
                system.used_swap(),
            )
        };
        Ok(MemoryInfo {
            free,
            available,
            total,
            used,
            free_swap,
            total_swap,
            used_swap,
        })
    }
    pub async fn get_disk_info(&self) -> Result<Vec<DiskInfo>, Error> {
        let disks = self.disks.read().await;
        let mut disk_info = vec![];
        for disk in disks.get_all_disks() {
            let usage = disks.get_disk_usage(&disk.name);
            if matches!(disk.disk_type, DiskType::Unknown) {
                continue;
            }
            disk_info.push(DiskInfo {
                dev_path: disk.device.display().to_string(),
                mount_path: disk.mount_path.as_ref().map(|p| p.display().to_string()),
                file_system: disk.file_system,
                partitions: disk.partitions.clone(),
                name: disk.name.clone(),
                vendor: disk.vendor.clone(),
                model: disk.model.clone(),
                disk_type: disk.disk_type,
                total: disk.space_info.map(|v| v.total_space).unwrap_or(0),
                used: disk.space_info.map(|v| v.used_space).unwrap_or(0),
                usage: DiskUsage {
                    recently_read: usage.as_ref().map(|v| v.recently_read).unwrap_or_default(),
                    recently_writen: usage
                        .as_ref()
                        .map(|v| v.recently_writen)
                        .unwrap_or_default(),
                    total_read: usage.as_ref().map(|v| v.total_read).unwrap_or_default(),
                    total_writen: usage.as_ref().map(|v| v.total_writen).unwrap_or_default(),
                },
            })
        }
        Ok(disk_info)
    }

    pub async fn reload_disks(&self) -> Result<(), Error> {
        if let Err(e) = self.disks.write().await.reload_disks().await {
            error!("Failed to Update Disk Usage: {e:?}");
        } else {
            let now_seconds = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("Expected System Time to be After EPOCH")
                .as_secs();
            self.last_disk_update.store(now_seconds, Ordering::Relaxed);
        }
        Ok(())
    }
    pub async fn get_network_info(&self) -> Result<Vec<NetworkInfo>, Error> {
        let networks = self.networks.read().await.clone();
        let mut net_info = vec![];
        for (_, n) in networks {
            match n {
                Device::Ethernet(dev) => {
                    debug!("Loading Wired Connection Info");
                    let active_connection = dev.active_connection().await.map_err(Error::other)?;
                    debug!("Loading Wired IpAddress Info");
                    let ip_addresses = match active_connection {
                        Some(active_connection) => {
                            let ip_config =
                                active_connection.ip4_config().await.map_err(Error::other)?;
                            ip_config.addresses().await.map_err(Error::other)?
                        }
                        None => Vec::with_capacity(0),
                    };
                    debug!("Loading Wired Mac Address Info");
                    let mac_address = dev.hw_address().await.map_err(Error::other)?;
                    debug!("Loading Wired Usage Stats");
                    let statistics = dev.get_statistics().await.map_err(Error::other)?;
                    debug!("Loading Wired Interface Name");
                    let interface_name = dev.interface().await?;
                    debug!("Found Interface Name: {interface_name}");
                    net_info.push(NetworkInfo {
                        name: interface_name,
                        ip_addresses: ip_addresses
                            .iter()
                            .filter_map(|v| {
                                if v.len() == 3 {
                                    Some(IPAddressInfo {
                                        address: IpAddr::V4(Ipv4Addr::from(v[0].to_be())),
                                        net_mask: v[1] as u8,
                                        gateway: IpAddr::V4(Ipv4Addr::from(v[2].to_be())),
                                    })
                                } else {
                                    warn!("Invalid Address in Network Manager Ipv4: {v:?}");
                                    None
                                }
                            })
                            .collect(),
                        mac_address,
                        data_downloaded: statistics.rx_bytes().await.unwrap_or_default(),
                        data_uploaded: statistics.tx_bytes().await.unwrap_or_default(),
                    });
                }
                Device::Wireless(dev) => {
                    debug!("Loading Wireless Connection Info");
                    let active_connection = dev.active_connection().await.map_err(Error::other)?;
                    debug!("Loading Wireless IpAddress Info");
                    let ip_addresses = match active_connection {
                        Some(active_connection) => {
                            let ip_config =
                                active_connection.ip4_config().await.map_err(Error::other)?;
                            ip_config.addresses().await.map_err(Error::other)?
                        }
                        None => Vec::with_capacity(0),
                    };
                    debug!("Loading Wireless Mac Address Info");
                    let mac_address = dev.hw_address().await.map_err(Error::other)?;
                    debug!("Loading Wireless Usage Stats");
                    let statistics = dev.get_statistics().await.map_err(Error::other)?;
                    debug!("Loading Wireless Interface Name");
                    let interface_name = dev.interface().await?;
                    debug!("Found Interface Name: {interface_name}");
                    net_info.push(NetworkInfo {
                        name: interface_name,
                        ip_addresses: ip_addresses
                            .iter()
                            .filter_map(|v| {
                                if v.len() == 3 {
                                    Some(IPAddressInfo {
                                        address: IpAddr::V4(Ipv4Addr::from(v[0].to_be())),
                                        net_mask: v[1] as u8,
                                        gateway: IpAddr::V4(Ipv4Addr::from(v[2].to_be())),
                                    })
                                } else {
                                    warn!("Invalid Address in Network Manager Ipv4: {v:?}");
                                    None
                                }
                            })
                            .collect(),
                        mac_address,
                        data_downloaded: statistics.rx_bytes().await.unwrap_or_default(),
                        data_uploaded: statistics.tx_bytes().await.unwrap_or_default(),
                    });
                }
                _ => {}
            }
        }
        Ok(net_info)
    }
}

pub fn value_to_u32(value: &Value) -> u32 {
    if value.is_number() {
        if let Some(v) = value.as_u64() {
            v as u32
        } else if let Some(v) = value.as_i64() {
            v as u32
        } else if let Some(v) = value.as_f64() {
            v as u32
        } else {
            0
        }
    } else {
        0
    }
}

#[get("/api/system/info", output = "json", eoutput = "bytes")]
pub async fn get_info(state: State<SystemMonitorPlugin>) -> Result<SystemInfo, Error> {
    state.0.get_system_info().await
}

#[get("/api/system/cpu", output = "json", eoutput = "bytes")]
pub async fn get_cpu(state: State<SystemMonitorPlugin>) -> Result<CpuInfo, Error> {
    state.0.get_cpu_info().await
}

#[get("/api/system/gpus", output = "json", eoutput = "bytes")]
pub async fn get_gpus(state: State<SystemMonitorPlugin>) -> Result<Vec<GpuInfo>, Error> {
    state.0.get_gpu_info().await
}

#[get("/api/system/memory", output = "json", eoutput = "bytes")]
pub async fn get_memory(state: State<SystemMonitorPlugin>) -> Result<MemoryInfo, Error> {
    state.0.get_memory_info().await
}

#[get("/api/system/disks", output = "json", eoutput = "bytes")]
pub async fn get_disks(state: State<SystemMonitorPlugin>) -> Result<Vec<DiskInfo>, Error> {
    state.0.get_disk_info().await
}

#[get("/api/system/networks", output = "json", eoutput = "bytes")]
pub async fn get_networks(state: State<SystemMonitorPlugin>) -> Result<Vec<NetworkInfo>, Error> {
    state.0.get_network_info().await
}

#[interval(1000)]
pub async fn refresh_system_info(state: State<SystemMonitorPlugin>) -> Result<(), Error> {
    debug!("Refreshing CPU usage");
    state.0.system.write().await.refresh_cpu_all();
    debug!("Refreshing Memory usage");
    state.0.system.write().await.refresh_memory();
    let now_seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Expected System Time to be After EPOCH")
        .as_secs();
    if now_seconds - state.0.last_disk_update.load(Ordering::Relaxed) >= 30 {
        debug!("Refreshing Disk usage");
        if let Err(e) = state.0.disks.write().await.reload_disks().await {
            error!("Failed to Update Disk Usage: {e:?}");
        } else {
            state
                .0
                .last_disk_update
                .store(now_seconds, Ordering::Relaxed);
        }
    }
    if now_seconds - state.0.last_net_update.load(Ordering::Relaxed) >= 5 {
        debug!("Refreshing Network usage");
        for device in all_devices().await? {
            match &device {
                Device::Ethernet(dev) => {
                    state
                        .0
                        .networks
                        .write()
                        .await
                        .insert(dev.service_path().to_string(), device);
                }
                Device::Wireless(dev) => {
                    state
                        .0
                        .networks
                        .write()
                        .await
                        .insert(dev.service_path().to_string(), device);
                }
                _ => {}
            }
        }
        state
            .0
            .last_net_update
            .store(now_seconds, Ordering::Relaxed);
    }
    debug!("Refreshing GPU usage");
    let mut gpus = vec![];
    if let Some(nvml) = state.0.nvml.read().await.as_ref() {
        gpus.extend(SystemMonitorPlugin::get_nvidia_gpu_info(nvml));
        debug!("Finished Nvidia GPU refresh");
    }
    if state.0.detected_amd_gpu.load(Ordering::Relaxed) {
        gpus.extend(SystemMonitorPlugin::get_amd_gpu_info().await);
        debug!("Finished AMD GPU refresh");
    }
    *state.0.gpus.write().await = gpus;
    debug!("Refreshed System Values");
    Ok(())
}
