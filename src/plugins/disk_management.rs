use crate::plugins::system_monitor::SystemMonitorPlugin;
use portfu::prelude::State;
use portfu_core::Json;
use portfu_macros::post;
use serde::Deserialize;
use std::ffi::OsStr;
use std::io::{Error, ErrorKind};
use tokio::fs::create_dir_all;
use tokio::process::Command;

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
            return Err(Error::new(
                ErrorKind::Other,
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
            return Err(Error::new(
                ErrorKind::Other,
                String::from_utf8_lossy(&output.stderr).to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Deserialize)]
pub struct MountParams {
    device_path: String,
    mount_path: String,
}

#[post("/api/disks/mount", output = "json", eoutput = "bytes")]
pub async fn mount(
    state: State<DiskManagerPlugin>,
    system_monitor: State<SystemMonitorPlugin>,
    params: Json<Option<MountParams>>,
) -> Result<(), Error> {
    match params.inner() {
        Some(params) => {
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
