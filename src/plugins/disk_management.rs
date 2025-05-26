use dg_sysfs::classes::block::disk::Disk;
use dg_sysfs::classes::block::{BlockDevice, BlockEnumerator};
use portfu::prelude::State;
use portfu_core::Json;
use portfu_macros::{get, post};
use serde::Deserialize;
use std::ffi::OsStr;
use std::io::{Error, ErrorKind};
use tokio::fs::create_dir_all;
use tokio::process::Command;

#[derive(Debug, Default)]
pub struct DiskManagerPlugin {
    enumerator: BlockEnumerator,
}
impl DiskManagerPlugin {
    pub fn new() -> DiskManagerPlugin {
        DiskManagerPlugin::default()
    }
    pub async fn list(&self) -> Result<Vec<Disk>, Error> {
        self.enumerator.get_devices().await.map(|v| {
            v.into_iter()
                .filter_map(|v| match v {
                    BlockDevice::Disk(disk) => Some(disk),
                    _ => None,
                })
                .collect()
        })
    }
    pub async fn list_mounted(&self) -> Result<Vec<BlockDevice>, Error> {
        self.enumerator.get_devices().await.map(|v| {
            v.into_iter()
                .filter(|v| match v {
                    BlockDevice::Disk(v) => v.mount_path.is_some(),
                    BlockDevice::Partition(v) => v.mount_path.is_some(),
                    _ => false,
                })
                .collect()
        })
    }
    pub async fn unmount<M: AsRef<OsStr>>(&self, mount_point: M) -> Result<bool, Error> {
        let mount_point = mount_point.as_ref();
        let output = Command::new("sudo")
            .arg("umount")
            .arg(mount_point)
            .output()
            .await?;
        if !output.status.success() {
            return Err(Error::new(
                ErrorKind::Other,
                String::from_utf8_lossy(&output.stderr).to_string(),
            ));
        }
        Ok(true)
    }
    pub async fn mount<D: AsRef<OsStr>, M: AsRef<OsStr>>(
        &self,
        device_path: D,
        mount_path: M,
    ) -> Result<bool, Error> {
        let device_path = device_path.as_ref();
        let mount_point = mount_path.as_ref();
        let output = Command::new("sudo")
            .arg("mount")
            .arg(device_path)
            .arg(mount_point)
            .output()
            .await?;
        if !output.status.success() {
            return Err(Error::new(
                ErrorKind::Other,
                String::from_utf8_lossy(&output.stderr).to_string(),
            ));
        }
        Ok(true)
    }
}

#[get("/api/disks/list", output = "json", eoutput = "bytes")]
pub async fn list_disks(state: State<DiskManagerPlugin>) -> Result<Vec<Disk>, Error> {
    state.0.list().await
}

#[derive(Deserialize)]
pub struct MountParams {
    device_path: String,
    mount_path: String,
}

#[post("/api/disks/mount", output = "json", eoutput = "bytes")]
pub async fn mount(
    state: State<DiskManagerPlugin>,
    params: Json<Option<MountParams>>,
) -> Result<bool, Error> {
    match params.inner() {
        Some(params) => {
            create_dir_all(&params.mount_path).await?;
            state.0.mount(&params.device_path, &params.mount_path).await
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
) -> Result<bool, Error> {
    match params.inner() {
        Some(params) => state.0.unmount(&params.mount_path).await,
        None => Err(Error::new(
            ErrorKind::InvalidInput,
            "Invalid Unmount Params",
        )),
    }
}
