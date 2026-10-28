use crate::config::ConfigManager;
use crate::models::config::AddConfigEntry;
use crate::plugins::system_monitor::SystemMonitorPlugin;
use dg_sysfs::classes::block::disk::FileSystem;
use log::{info, warn};
use portfu::prelude::State;
use portfu_core::Json;
use portfu_macros::{interval, post};
use serde::Deserialize;
use sqlx::SqlitePool;
use std::ffi::OsStr;
use std::io::{Error, ErrorKind};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::path::{Component, Path};
use tokio::process::Command;
use tokio::sync::RwLock;

#[derive(Debug, Default)]
pub struct DiskManagerPlugin {}
impl DiskManagerPlugin {
    pub fn new() -> DiskManagerPlugin {
        DiskManagerPlugin::default()
    }
    pub async fn unmount<M: AsRef<OsStr>>(&self, mount_point: M) -> Result<(), Error> {
        let target = trusted_mount_target(Path::new(mount_point.as_ref()), false)?;
        let pinned = format!("/proc/{}/fd/{}", std::process::id(), target.as_raw_fd());
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            Command::new("sudo")
                .kill_on_drop(true)
                .arg("-n")
                .arg("umount")
                .arg("--no-canonicalize")
                .arg("--")
                .arg(pinned)
                .output(),
        )
        .await
        .map_err(|_| Error::new(ErrorKind::TimedOut, "Disk operation timed out"))??;
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
        let device_path = std::fs::canonicalize(device_path.as_ref())?;
        if !device_path.starts_with("/dev")
            || !std::fs::metadata(&device_path)?
                .file_type()
                .is_block_device()
        {
            return Err(Error::new(
                ErrorKind::PermissionDenied,
                "Only local block devices may be mounted",
            ));
        }
        let target = trusted_mount_target(Path::new(mount_path.as_ref()), true)?;
        let pinned = format!("/proc/{}/fd/{}", std::process::id(), target.as_raw_fd());
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            Command::new("sudo")
                .kill_on_drop(true)
                .arg("-n")
                .arg("mount")
                .arg("--no-canonicalize")
                .args(["-o", "nodev,nosuid,noexec"])
                .arg("--")
                .arg(device_path)
                .arg(pinned)
                .output(),
        )
        .await
        .map_err(|_| Error::new(ErrorKind::TimedOut, "Disk operation timed out"))??;
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
                        disk_manager.0.mount(&partition.device, &mount_path).await?;
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
            let mut found = false;
            let path_buf = Path::new(&params.device_path);
            for disk in known_disks {
                if let Some(partition) = disk.partitions.into_iter().find(|p| p.device == path_buf)
                {
                    if partition.mount_path.is_some() {
                        return Err(Error::new(
                            ErrorKind::PermissionDenied,
                            "Partition is already mounted",
                        ));
                    }
                    found = true;
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
            if !found {
                return Err(Error::new(ErrorKind::PermissionDenied, "Unknown partition"));
            }
            validate_mount_path(Path::new(&params.mount_path))?;
            //Find the Disk we are Mounting.
            state
                .0
                .mount(&params.device_path, &params.mount_path)
                .await?;
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
    config: State<RwLock<ConfigManager>>,
    database: State<SqlitePool>,
    system_monitor: State<SystemMonitorPlugin>,
    params: Json<Option<UnMountParams>>,
) -> Result<(), Error> {
    match params.inner() {
        Some(params) => {
            // The automounter holds a config read guard while mounting. Keep
            // this write guard through unmount and policy removal so it cannot
            // immediately undo the user's explicit action.
            let mut config = config.write().await;
            state.0.unmount(&params.mount_path).await?;
            let entries = crate::database::config::get_config(&database.0).await?;
            for entry in entries {
                if entry.key.starts_with("auto-mount-")
                    && Path::new(&entry.value) == Path::new(&params.mount_path)
                {
                    config.delete(&entry.key, &database.0).await?;
                }
            }
            drop(config);
            system_monitor.0.reload_disks().await
        }
        None => Err(Error::new(
            ErrorKind::InvalidInput,
            "Invalid Unmount Params",
        )),
    }
}

fn validate_mount_path(path: &Path) -> Result<(), Error> {
    let relative = path.strip_prefix("/mnt").map_err(|_| {
        Error::new(
            ErrorKind::PermissionDenied,
            "Mount targets must be directly under /mnt",
        )
    })?;
    let components: Vec<_> = relative.components().collect();
    if components.len() != 1 || !matches!(components[0], Component::Normal(_)) {
        return Err(Error::new(
            ErrorKind::PermissionDenied,
            "Mount targets must be directly under /mnt",
        ));
    }
    Ok(())
}
fn trusted_mount_target(path: &Path, create: bool) -> Result<std::fs::File, Error> {
    validate_mount_path(path)?;
    let root = std::fs::symlink_metadata("/mnt")?;
    if !root.is_dir() || root.uid() != 0 || root.mode() & 0o022 != 0 {
        return Err(Error::new(
            ErrorKind::PermissionDenied,
            "Untrusted mount root",
        ));
    }
    if create {
        use std::os::unix::fs::DirBuilderExt;
        match std::fs::DirBuilder::new().mode(0o700).create(path) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
}
#[cfg(test)]
mod policy_tests {
    use super::*;
    #[test]
    fn mount_targets_cannot_escape_storage_root() {
        for path in [
            "/",
            "/etc",
            "/mnt",
            "/mnt/../etc",
            "/mnt/disk/child",
            "-a",
            "mnt/disk",
        ] {
            assert!(validate_mount_path(Path::new(path)).is_err(), "{path}");
        }
        assert!(validate_mount_path(Path::new("/mnt/plot-disk")).is_ok());
    }
}
