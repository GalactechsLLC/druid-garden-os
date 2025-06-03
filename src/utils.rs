use crate::database::config::get_config_key;
use crate::first_run::{check_for_default_admin_account, validate_config_table};
use argon2::{Algorithm, Argon2, Params, Version};
use bollard::Docker;
use dg_network_manager::dbus_api::devices::Device;
use dg_network_manager::dbus_api::network_manager::NetworkManagerClient;
use dg_network_manager::{
    create_hotspot, scan_all_ssids, try_existing_connections, wireless_devices,
};
use log::{debug, error, info, warn};
use portfu::prelude::{Service, ServiceGroup};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::SqlitePool;
use std::io::Error;
use std::path::Path;
use std::time::Duration;
use tokio::fs;
use tokio::net::TcpStream;
use tokio::time::timeout;

pub async fn create_pool(database_path: &str) -> Result<SqlitePool, Error> {
    if let Some(parent) = Path::new(database_path).parent() {
        fs::create_dir_all(parent).await?;
    }
    let connect_opts = SqliteConnectOptions::new()
        .filename(database_path)
        .create_if_missing(true)
        // PERSIST is for less wear on SD card systems
        .journal_mode(SqliteJournalMode::Persist)
        .synchronous(SqliteSynchronous::Full);
    SqlitePoolOptions::new()
        .max_connections(50)
        .connect_with(connect_opts)
        .await
        .map_err(|e| Error::other(format!("Failed to connect to SQLite database: {}", e)))
}

pub fn create_argon() -> Result<Argon2<'static>, Error> {
    Ok(Argon2::new(
        Algorithm::Argon2id,
        Version::V0x13,
        //https://cheatsheetseries.owasp.org/cheatsheets/Password_Storage_Cheat_Sheet.html#argon2id
        Params::new(47104, 1, 1, Some(32))
            .map_err(|e| Error::other(format!("Invalid Argon params: {}", e)))?,
    ))
}

pub async fn run_migrations(pool: &SqlitePool) -> Result<(), Error> {
    sqlx::migrate!()
        .run(pool)
        .await
        .map_err(|e| Error::other(format!("Failed to Migrate Database: {e:?}")))
}

pub fn connect_to_docker() -> Result<Docker, Error> {
    Docker::connect_with_defaults()
        .map_err(|e| Error::other(format!("Failed to connect to docker: {}", e)))
}

pub fn find_index_service(static_files: &ServiceGroup) -> Option<Service> {
    static_files
        .services
        .iter()
        .filter_map(|s| {
            if s.path.matches("/") {
                Some(s.clone())
            } else {
                None
            }
        })
        .next()
}

pub async fn has_internet_connection() -> bool {
    let endpoints = [
        "8.8.8.8:53",        // Google's DNS
        "1.1.1.1:53",        // Cloudflare's DNS
        "208.67.222.222:53", // OpenDNS
    ];
    let mut connection_established = false;
    for &address in &endpoints {
        debug!("Attempting to connect to {}...", address);
        if let Ok(Ok(_stream)) = timeout(Duration::from_secs(5), TcpStream::connect(address)).await
        {
            debug!("Successfully connected via {}!", address);
            connection_established = true;
            break;
        } else {
            warn!("Connection to {} failed.", address);
        }
    }
    if !connection_established {
        error!("No internet connection detected on any endpoint.");
    }
    connection_established
}

pub async fn perform_startup_checks(
    pool: &SqlitePool,
    argon: &Argon2<'static>,
) -> Result<(), Error> {
    //Check Users table is Empty, and Create Default user account
    check_for_default_admin_account(pool, argon).await?;

    //Check for Default config Entries
    validate_config_table(pool).await?;

    //First Check if we are connected to internet
    if !has_internet_connection().await {
        let network_manager = NetworkManagerClient::new().await?;
        let mut connected = false;
        //If we have no internet check for existing connections to activate
        for device in network_manager.get_devices().await? {
            if let Ok(true) = try_existing_connections(device).await {
                tokio::time::sleep(Duration::from_secs(3)).await;
                connected = has_internet_connection().await;
                if !connected {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    connected = has_internet_connection().await;
                    if !connected {
                        continue;
                    }
                }
                break;
            }
        }
        if !connected {
            //No Internet or valid connections, start the hotspot
            let wireless_device_name = get_config_key(pool, "wifi_device")
                .await?
                .map(|c| c.value)
                .unwrap_or_default();
            let hotspot_ssid = get_config_key(pool, "hotspot_ssid")
                .await?
                .map(|c| c.value)
                .unwrap_or("DG_OS_SETUP".to_string());
            let hotspot_password = get_config_key(pool, "hotspot_password")
                .await?
                .map(|c| c.value)
                .unwrap_or("DG_xch1234!".to_string());
            let mut found_wireless_device: Option<Device> = None;
            for device in wireless_devices().await? {
                let wireless_device = match &device {
                    Device::Wireless(w) => w,
                    _ => unreachable!(),
                };
                match wireless_device.interface().await {
                    Ok(v) => {
                        if v == wireless_device_name {
                            found_wireless_device = Some(device);
                            break;
                        }
                    }
                    Err(e) => {
                        error!("Failure when Checking Device Interface Name: {e:?}");
                    }
                }
            }
            match found_wireless_device {
                Some(device) => {
                    //Pre Scan the List of Access Point around us
                    let _ = scan_all_ssids(device.clone()).await?;
                    if create_hotspot(
                        device.clone(),
                        None,
                        hotspot_ssid.clone(),
                        Some(hotspot_password.clone()),
                    )
                    .await
                    .is_ok()
                    {
                        info!("Started Hotspot on SSID: {}", hotspot_ssid);
                    }
                }
                None => {
                    let wireless_devices = wireless_devices().await?;
                    if wireless_devices.is_empty() {
                        error!("No Wireless devices to start hotspot and no internet connection. ")
                    } else {
                        for device in wireless_devices {
                            //Pre Scan the List of Access Point around us
                            let _ = scan_all_ssids(device.clone()).await?;
                            if create_hotspot(
                                device,
                                None,
                                hotspot_ssid.clone(),
                                Some(hotspot_password.clone()),
                            )
                            .await
                            .is_ok()
                            {
                                info!("Started Hotspot on SSID: {}", hotspot_ssid);
                                break;
                            }
                        }
                    }
                }
            }
        }
    } else {
        info!("Device has an Internet Connection");
    }
    Ok(())
}
