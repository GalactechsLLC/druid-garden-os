use crate::database::config::{create_config_entry, get_config_key};
use crate::database::users::{has_no_users, register, UserWithInfoWithPassword};
use crate::models::config::AddConfigEntry;
use argon2::Argon2;
use dg_sysfs::classes::net::{NetDevice, NetEnumerator};
use portfu_admin::users::UserRole;
use sqlx::SqlitePool;
use std::io::Error;

pub async fn check_for_default_admin_account(
    pool: &SqlitePool,
    argon: &Argon2<'static>,
) -> Result<(), Error> {
    if has_no_users(pool).await? {
        let _ = register(
            pool,
            argon,
            UserWithInfoWithPassword {
                id: -1,
                username: "Admin".to_string(),
                password: b"Admin".to_vec(),
                role: UserRole::SuperAdmin,
            },
        )
        .await?;
    }
    Ok(())
}

pub async fn validate_config_table(pool: &SqlitePool) -> Result<(), Error> {
    if get_config_key(pool, "bookmarks").await?.is_none() {
        create_config_entry(
            pool,
            &AddConfigEntry {
                key: "bookmarks".to_string(),
                value: r#"{
                "Root": "/",
                "Home": "~",
            }"#
                .to_string(),
                last_value: "{}".to_string(),
                category: "preferences".to_string(),
                system: 1,
            },
        )
        .await?;
    }
    if get_config_key(pool, "theme").await?.is_none() {
        create_config_entry(
            pool,
            &AddConfigEntry {
                key: "theme".to_string(),
                value: "default".to_string(),
                last_value: "default".to_string(),
                category: "preferences".to_string(),
                system: 1,
            },
        )
        .await?;
    }
    if get_config_key(pool, "wifi_device").await?.is_none() {
        let device_name = NetEnumerator::new()
            .get_devices()
            .await?
            .into_iter()
            .filter_map(|d| match d {
                NetDevice::Physical(p) => {
                    if p.wireless {
                        Some(p)
                    } else {
                        None
                    }
                }
                NetDevice::Loopback => None,
                NetDevice::Invalid(_) => None,
            })
            .map(|w| w.name)
            .next()
            .unwrap_or_default();
        create_config_entry(
            pool,
            &AddConfigEntry {
                key: "wifi_device".to_string(),
                value: device_name,
                last_value: "".to_string(),
                category: "system".to_string(),
                system: 1,
            },
        )
        .await?;
    }
    if get_config_key(pool, "hotspot_ssid").await?.is_none() {
        create_config_entry(
            pool,
            &AddConfigEntry {
                key: "hotspot_ssid".to_string(),
                value: "DG_OS_SETUP".to_string(),
                last_value: "".to_string(),
                category: "system".to_string(),
                system: 1,
            },
        )
        .await?;
    }
    if get_config_key(pool, "hotspot_password").await?.is_none() {
        create_config_entry(
            pool,
            &AddConfigEntry {
                key: "hotspot_password".to_string(),
                value: "DG_xch1234!".to_string(),
                last_value: "".to_string(),
                category: "system".to_string(),
                system: 1,
            },
        )
        .await?;
    }
    if get_config_key(pool, "farmer_name").await?.is_none() {
        create_config_entry(
            pool,
            &AddConfigEntry {
                key: "farmer_name".to_string(),
                value: "Some Random Name".to_string(),
                last_value: "".to_string(),
                category: "farmer".to_string(),
                system: 1,
            },
        )
        .await?;
    }
    Ok(())
}
