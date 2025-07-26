use crate::config::ConfigManager;
use crate::models::config::AddConfigEntry;
use crate::plugins::system_monitor::{DiskInfo, SystemMonitorPlugin};
use log::error;
use portfu::prelude::State;
use portfu_core::Json;
use portfu_macros::{interval, post};
use serde::Deserialize;
use sqlx::SqlitePool;
use std::ffi::OsStr;
use std::io::{Error, ErrorKind};
use std::path::Path;
use tokio::fs::create_dir_all;
use tokio::process::Command;
use tokio::sync::RwLock;

#[derive(Debug, Default)]
pub struct DiskManagerPlugin {}
impl DiskManagerPlugin {
    pub fn new() -> DiskManagerPlugin {
        DiskManagerPlugin::default()
    }
    pub async fn unmount<M: AsRef<OsStr>>(&self, mount_point: M) -> Result<(), Error> {
        let mount_point = mount_point.as_ref();
        let output = Command::new("sudo")
            .arg("umount")
            .arg(mount_point)
            .output()
            .await?;
        if !output.status.success() {
            return Err(Error::other(
                String::from_utf8_lossy(&output.stderr).to_string(),
            ));
        }
        Ok(())
    }
    pub async fn mount<D: AsRef<OsStr>, M: AsRef<OsStr>>(
        &self,
        device_path: D,
        mount_path: M,
    ) -> Result<(), Error> {
        let device_path = device_path.as_ref();
        let mount_point = mount_path.as_ref();
        let output = Command::new("sudo")
            .arg("mount")
            .arg(device_path)
            .arg(mount_point)
            .output()
            .await?;
        if !output.status.success() {
            return Err(Error::other(
                String::from_utf8_lossy(&output.stderr).to_string(),
            ));
        }
        Ok(())
    }
}

#[interval(10_000)]
pub async fn disk_auto_mounting(
    disk_manager: State<DiskManagerPlugin>,
    config: State<RwLock<ConfigManager>>,
    system_manager: State<SystemMonitorPlugin>,
) -> Result<(), Error> {
    //Load All Known Disks
    let known_disks = system_manager.0.get_disk_info().await?;
    //Find all unmounted Disks
    let unmounted: Vec<DiskInfo> = known_disks
        .into_iter()
        .filter(|f| f.mount_path.is_none())
        .collect();
    //Check if Disk is set to auto mount
    for disk in unmounted {
        let key = format!("auto-mount-{}", disk.name);
        if let Some(entry) = config.read().await.get(&key).await {
            let mount_path = entry.value;
            //Mount the disk and continue
            if let Err(e) = disk_manager.mount(&disk.dev_path, &mount_path).await {
                error!("Failure when Mounting Disk {}: {e:?}", disk.dev_path);
            }
        }
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct MountParams {
    device_path: String,
    mount_path: String,
    auto_mount: Option<bool>,
}

#[post("/api/disks/mount", output = "json", eoutput = "bytes")]
pub async fn mount(
    database: State<SqlitePool>,
    state: State<DiskManagerPlugin>,
    config: State<RwLock<ConfigManager>>,
    system_monitor: State<SystemMonitorPlugin>,
    params: Json<Option<MountParams>>,
) -> Result<(), Error> {
    match params.inner() {
        Some(params) => {
            //Confirm we know about the disk they want to mount
            let known_disks = system_monitor.0.get_disk_info().await?;
            //First Check Disks, then Check Partitions
            let mount_name = match known_disks
                .iter()
                .find(|d| d.dev_path == params.device_path)
            {
                Some(disk) => Some(disk.name.clone()),
                None => {
                    let mut value = None;
                    for disk in known_disks {
                        let path_buf = Path::new(&params.device_path);
                        value = disk
                            .partitions
                            .into_iter()
                            .find(|p| p.device == path_buf)
                            .map(|p| p.name.clone());
                        if value.is_some() {
                            break;
                        }
                    }
                    value
                }
            }
            .ok_or(Error::new(
                ErrorKind::NotFound,
                format!(
                    "Failed to find disk with Device Path: {}",
                    params.device_path
                ),
            ))?;
            if params.auto_mount.unwrap_or(false) {
                //Drive is set to auto mount
                let key = format!("auto-mount-{mount_name}");
                config
                    .write()
                    .await
                    .set(
                        &key,
                        AddConfigEntry {
                            key: key.clone(),
                            value: params.mount_path.clone(),
                            last_value: "".to_string(),
                            category: "preferences".to_string(),
                            system: 0,
                        },
                        Some(&database),
                    )
                    .await?;
            }
            //Find the Disk we are Mounting.
            create_dir_all(&params.mount_path).await?;
            state
                .0
                .mount(&params.device_path, &params.mount_path)
                .await?;
            system_monitor.0.reload_disks().await
        }
        None => Err(Error::new(ErrorKind::InvalidInput, "Invalid Mount Params")),
    }
}

#[derive(Deserialize)]
pub struct UnMountParams {
    mount_path: String,
}

#[post("/api/disks/unmount", output = "json", eoutput = "bytes")]
pub async fn unmount(
    state: State<DiskManagerPlugin>,
    params: Json<Option<UnMountParams>>,
) -> Result<(), Error> {
    match params.inner() {
        Some(params) => state.0.unmount(&params.mount_path).await,
        None => Err(Error::new(
            ErrorKind::InvalidInput,
            "Invalid Unmount Params",
        )),
    }
}
