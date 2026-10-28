use crate::database::config::{create_config_entry, get_config_key};
use crate::database::users::{has_no_users, register, UserWithInfoWithPassword};
use crate::models::config::AddConfigEntry;
use argon2::Argon2;
use dg_sysfs::classes::net::{NetDevice, NetEnumerator};
use portfu_admin::users::UserRole;
use rand::RngCore;
use sqlx::SqlitePool;
use std::io::{Error, ErrorKind, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::MetadataExt;
use sysinfo::System;

/// The bootstrap credential is never printed or returned through the API.
/// Open every directory relative to a held descriptor, rejecting symlinks.
pub fn bootstrap_password(create: bool) -> Result<Option<Vec<u8>>, Error> {
    let root = std::fs::File::open("/")?;
    let mut directory: OwnedFd = root.into();
    for (component, final_directory) in [("var", false), ("lib", false), ("druid-garden", true)] {
        let name = std::ffi::CString::new(component).map_err(Error::other)?;
        let mut fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 && Error::last_os_error().kind() == ErrorKind::NotFound {
            if !create {
                return Ok(None);
            }
            if final_directory {
                let result = unsafe { libc::mkdirat(directory.as_raw_fd(), name.as_ptr(), 0o700) };
                if result < 0 && Error::last_os_error().kind() != ErrorKind::AlreadyExists {
                    return Err(Error::last_os_error());
                }
                fd = unsafe {
                    libc::openat(
                        directory.as_raw_fd(),
                        name.as_ptr(),
                        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                    )
                };
            }
        }
        if fd < 0 {
            return Err(Error::last_os_error());
        }
        let file = unsafe { std::fs::File::from_raw_fd(fd) };
        let meta = file.metadata()?;
        let current_uid = unsafe { libc::geteuid() };
        let trusted_owner = meta.uid() == current_uid || (!final_directory && meta.uid() == 0);
        if !trusted_owner || meta.mode() & 0o022 != 0 {
            return Err(Error::new(
                ErrorKind::PermissionDenied,
                "Bootstrap directory must be owned by the service user and not writable by others",
            ));
        }
        directory = file.into();
    }
    let name = c"bootstrap-password";
    let read_flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
    let mut fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), read_flags) };
    if fd < 0 && Error::last_os_error().kind() == ErrorKind::NotFound {
        if !create {
            return Ok(None);
        }
        fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(Error::last_os_error());
        }
        let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
        let mut bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        let password: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        file.write_all(password.as_bytes())?;
        file.sync_all()?;
        let directory_file = std::fs::File::from(directory);
        directory_file.sync_all()?;
        return Ok(Some(password.into_bytes()));
    }
    if fd < 0 {
        return Err(Error::last_os_error());
    }
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    let meta = file.metadata()?;
    if !meta.is_file()
        || meta.nlink() != 1
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.mode() & 0o077 != 0
        || meta.len() != 64
    {
        return Err(Error::new(
            ErrorKind::PermissionDenied,
            "Invalid bootstrap credential file ownership, permissions, or contents",
        ));
    }
    let mut password = Vec::new();
    (&mut file).take(65).read_to_end(&mut password)?;
    if password.len() != 64 || !password.iter().all(u8::is_ascii_hexdigit) {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "Invalid bootstrap credential",
        ));
    }
    Ok(Some(password))
}

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
                password: bootstrap_password(true)?
                    .ok_or_else(|| Error::other("Missing bootstrap password"))?,
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
    if get_config_key(pool, "heartbeat.control_enabled")
        .await?
        .is_none()
    {
        create_config_entry(
            pool,
            &AddConfigEntry {
                key: "heartbeat.control_enabled".to_string(),
                value: "false".to_string(),
                last_value: "".to_string(),
                category: "system".to_string(),
                system: 1,
            },
        )
        .await?;
    }
    if get_config_key(pool, "heartbeat.enabled").await?.is_none() {
        create_config_entry(
            pool,
            &AddConfigEntry {
                key: "heartbeat.enabled".to_string(),
                value: "true".to_string(),
                last_value: "".to_string(),
                category: "system".to_string(),
                system: 1,
            },
        )
        .await?;
    }
    if get_config_key(pool, "heartbeat.base_url").await?.is_none() {
        create_config_entry(
            pool,
            &AddConfigEntry {
                key: "heartbeat.base_url".to_string(),
                value: "https://druid.garden".to_string(),
                last_value: "".to_string(),
                category: "system".to_string(),
                system: 1,
            },
        )
        .await?;
    }
    if get_config_key(pool, "heartbeat.interval_seconds")
        .await?
        .is_none()
    {
        create_config_entry(
            pool,
            &AddConfigEntry {
                key: "heartbeat.interval_seconds".to_string(),
                value: "300".to_string(),
                last_value: "".to_string(),
                category: "system".to_string(),
                system: 1,
            },
        )
        .await?;
    }
    if get_config_key(pool, "heartbeat.timeout_seconds")
        .await?
        .is_none()
    {
        create_config_entry(
            pool,
            &AddConfigEntry {
                key: "heartbeat.timeout_seconds".to_string(),
                value: "10".to_string(),
                last_value: "".to_string(),
                category: "system".to_string(),
                system: 1,
            },
        )
        .await?;
    }
    if get_config_key(pool, "heartbeat.api_key").await?.is_none() {
        let existing_token = get_config_key(pool, "heartbeat.device_token")
            .await?
            .map(|v| v.value)
            .unwrap_or_default();
        create_config_entry(
            pool,
            &AddConfigEntry {
                key: "heartbeat.api_key".to_string(),
                value: existing_token,
                last_value: "".to_string(),
                category: "system".to_string(),
                system: 1,
            },
        )
        .await?;
    }
    if get_config_key(pool, "heartbeat.name").await?.is_none() {
        create_config_entry(
            pool,
            &AddConfigEntry {
                key: "heartbeat.name".to_string(),
                value: System::host_name().unwrap_or_else(|| "Druid Garden Device".to_string()),
                last_value: "".to_string(),
                category: "system".to_string(),
                system: 1,
            },
        )
        .await?;
    }
    Ok(())
}
