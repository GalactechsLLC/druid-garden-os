mod config;
mod database;
mod first_run;
mod legacy;
mod models;
mod plugins;
mod service_groups;
mod utils;
mod web;

use crate::config::ConfigManager;
use crate::models::ServerSettings;
use crate::plugins::disk_management::DiskManagerPlugin;
use crate::plugins::farmer::{update_local_stats, FarmerManager};
use crate::plugins::file_manager::FileManagerPlugin;
use crate::plugins::system_monitor::{refresh_system_info, SystemMonitorPlugin};
use crate::plugins::PluginManager;
use crate::service_groups::{
    admin_group, editor_group, manager_group, none_group, super_group, user_groups, viewer_group,
};
use crate::utils::{
    connect_to_docker, create_argon, create_pool, find_index_service, perform_startup_checks,
    run_migrations,
};
use crate::web::auth::{BasicAuthHandle, PasswordUpdateWrapper};
use dg_logger::DruidGardenLogger;
use druid_garden_os::init_logger;
use log::info;
use portfu::prelude::http::HeaderName;
use portfu::prelude::*;
use portfu::wrappers::cors::Cors;
use portfu::wrappers::sessions::SessionWrapper;
use std::env::args;
use std::io::Error;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

pub const fn version() -> &'static str {
    druid_garden_os::version()
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .unwrap_or_default();
    let mut args = args();
    if args.len() == 2 && args.nth(1).unwrap_or_default() == "--version" {
        println!("{}", version());
        return Ok(());
    }
    let logger = init_logger()?;
    let settings = ServerSettings::from_env()?;
    let db = create_pool(&settings.database_path).await?;
    let argon = create_argon()?;
    run_migrations(&db).await?;
    perform_startup_checks(&db, &argon).await?;
    info!("Connecting to Docker");
    let docker = connect_to_docker()?;
    info!("Setting Up Auth");
    let basic_auth = BasicAuthHandle::new(db.clone(), argon.clone());
    info!("Setting Up Farmer Manager");
    let farmer_manager = Arc::new(FarmerManager::new(db.clone()).await?);
    info!("Setting Up Plugin Manager");
    let plugin_manager = PluginManager::new(&db, PathBuf::from(settings.plugin_path)).await;
    info!("Setting Up Config Manager");
    let config_manager = ConfigManager::new(&db).await?;
    info!("Setting Up System Monitor");
    let system_manager = SystemMonitorPlugin::new().await;
    info!("Loading Network Information");
    let network_info = system_manager.get_network_info().await?;
    let ip_list = network_info.into_iter().fold(vec![], |mut r, v| {
        for a in v.ip_addresses {
            let address = a.address.to_string();
            r.push(format!("http://{}:8080", address));
            r.push(format!("http://{}:8443", address));
            r.push(address);
        }
        r
    });
    info!("Setting Up File Manager");
    let file_manager = FileManagerPlugin::new();
    info!("Setting Up Disk Manager");
    let disk_manager = DiskManagerPlugin::new();
    info!("Setting Up Static HTML Files");
    let static_files: ServiceGroup = ServiceGroup::from(druid_garden_os::HtmlFiles {});
    let index_service = find_index_service(&static_files).expect("Failed to find index service");
    info!("Setting Server");
    let server = ServerBuilder::default()
        .host(settings.hostname)
        .port(settings.port)
        .shared_state(RwLock::new(plugin_manager))
        .shared_state::<DruidGardenLogger>(logger)
        .shared_state(argon)
        .shared_state(docker)
        .shared_state(db)
        .shared_state(system_manager)
        .shared_state::<FarmerManager>(farmer_manager.clone())
        .shared_state(file_manager)
        .shared_state(disk_manager)
        .shared_state(RwLock::new(config_manager))
        .default_service(index_service)
        .wrap(Arc::new(Cors::new(
            [
                "http://localhost",
                "http://localhost:8443",
                "http://localhost:8080",
                "http://127.0.0.1",
                "http://127.0.0.1:8443",
                "http://127.0.0.1:8080",
                "https://druid.garden",
                "https://dev.druid.garden",
            ]
            .into_iter()
            .map(String::from)
            .chain(ip_list)
            .collect(),
            ["GET", "POST", "HEAD"]
                .into_iter()
                .map(String::from)
                .collect(),
            vec![
                HeaderName::from_static("host"),
                HeaderName::from_static("accept-encoding"),
                HeaderName::from_static("referer"),
                HeaderName::from_static("content-type"),
            ],
            false,
        )))
        .register(static_files)
        .wrap(Arc::new(SessionWrapper::default()))
        .register(none_group(basic_auth))
        .register(user_groups())
        .wrap(Arc::new(PasswordUpdateWrapper {}))
        .register(viewer_group())
        .register(editor_group())
        .register(manager_group())
        .register(admin_group())
        .register(super_group())
        .task(update_local_stats)
        .task(refresh_system_info);
    info!("Starting Services");
    let res = server.build().run().await;
    info!("Shutting Down");
    farmer_manager.stop_farmer().await?;
    info!("Farmer Stopped");
    res
}
