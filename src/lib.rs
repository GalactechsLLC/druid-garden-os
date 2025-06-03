use dg_logger::{DruidGardenLogger, TimestampFormat};
use log::Level;
use portfu_macros::static_files;
use std::env;
use std::io::Error;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;

pub mod config;
pub mod database;
pub mod first_run;
pub mod legacy;
pub mod models;
pub mod plugins;
pub mod service_groups;
pub mod utils;
pub mod web;

pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
pub const fn pkg_name() -> &'static str {
    env!("CARGO_PKG_NAME")
}
pub fn named_version() -> String {
    format!("{}: {}", pkg_name(), version())
}

#[static_files("druid-garden-os-ui/public")]
pub struct HtmlFiles;

pub type FarmerThread = RwLock<Option<JoinHandle<Result<(), Error>>>>;

pub fn init_logger() -> Result<Arc<DruidGardenLogger>, Error> {
    unsafe {
        env::set_var("ZBUS_TRACE", "0");
    }
    DruidGardenLogger::build()
        .use_colors(true)
        .current_level(Level::Info)
        .timestamp_format(TimestampFormat::Local)
        .with_target_level("zbus", Level::Warn)
        .with_target_level("tracing", Level::Warn)
        .init()
        .map_err(Error::other)
}
