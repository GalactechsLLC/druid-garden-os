use crate::config::ConfigManager;
use crate::models::config::AddConfigEntry;
use crate::plugins::system_monitor::{DiskInfo, SystemMonitorPlugin};
use dg_sysfs::classes::block::disk::FileSystem;
use log::{info, warn};
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
        create_dir_all(mount_point).await?;
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
    system_manager.0.reload_disks().await?;
    //Load All Known Disks
    let known_disks = system_manager.0.get_disk_info().await?;
    //Check if Disk has partition to auto mount
    for disk in known_disks {
        for partition in disk.partitions {
            let uuid = match partition.file_system {
                None => partition.uuid,
                Some(file_system) => match file_system {
                    FileSystem::Btrfs(uuid)
                    | FileSystem::ExFAT(uuid)
                    | FileSystem::Ext2(uuid)
                    | FileSystem::Ext3(uuid)
                    | FileSystem::Ext4(uuid)
                    | FileSystem::F2FS(uuid)
                    | FileSystem::FAT12(uuid)
                    | FileSystem::FAT16(uuid)
                    | FileSystem::FAT32(uuid)
                    | FileSystem::JFS(uuid)
                    | FileSystem::NTFS(uuid)
                    | FileSystem::ReiserFS(uuid)
                    | FileSystem::XFS(uuid) => Some(uuid),
                    FileSystem::ISO9660 | FileSystem::Unknown => partition.uuid,
                },
            };
            if let Some(uuid) = uuid {
                let key = format!("auto-mount-{}", uuid);
                info!("Looking Automount Entry - {key}");
                if let Some(entry) = config.read().await.get(&key).await {
                    info!("Found Automount Entry");
                    if partition.mount_path.is_none() {
                        info!("Found Unmounted Disk");
                        let mount_path = entry.value;
                        disk_manager
                            .0
                            .mount(&partition.device, &mount_path)
                            .await?;
                    }
                }
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
            let mut uuid = None;
            let path_buf = Path::new(&params.device_path);
            for disk in known_disks {
                if let Some(partition) = disk.partitions.into_iter().find(|p| p.device == path_buf)
                {
                    uuid = match partition.file_system {
                        None => partition.uuid,
                        Some(file_system) => match file_system {
                            FileSystem::Btrfs(uuid)
                            | FileSystem::ExFAT(uuid)
                            | FileSystem::Ext2(uuid)
                            | FileSystem::Ext3(uuid)
                            | FileSystem::Ext4(uuid)
                            | FileSystem::F2FS(uuid)
                            | FileSystem::FAT12(uuid)
                            | FileSystem::FAT16(uuid)
                            | FileSystem::FAT32(uuid)
                            | FileSystem::JFS(uuid)
                            | FileSystem::NTFS(uuid)
                            | FileSystem::ReiserFS(uuid)
                            | FileSystem::XFS(uuid) => Some(uuid),
                            FileSystem::ISO9660 | FileSystem::Unknown => partition.uuid,
                        },
                    };
                    break;
                }
            }
            if params.auto_mount.unwrap_or(false) {
                match uuid {
                    Some(uuid) => {
                        //Drive is set to auto mount
                        let key = format!("auto-mount-{uuid}");
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
                    None => {
                        warn!("Unable to automount without a device UUID");
                    }
                }
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
