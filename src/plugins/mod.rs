pub mod confined_files;
pub mod device_control;
pub mod device_heartbeat;
pub(crate) mod device_identity;
pub mod disk_management;
pub mod farmer;
pub mod file_manager;
pub mod led_manager;
pub mod system_monitor;
use crate::database::plugins::{create_plugin, delete_plugin, get_all_plugins};
use crate::models::plugins::{AddPlugin, Plugin, PluginType};
use crate::version;
use bollard::container::{Config, CreateContainerOptions};
use bollard::image::CreateImageOptions;
use bollard::service::{HostConfig, PortBinding};
use bollard::Docker;
use portfu::prelude::futures_util::StreamExt;
use semver::Version;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::collections::hash_map::VacantEntry;
use std::collections::HashMap;
use std::fs::Permissions;
use std::io::{Error, ErrorKind};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use time::OffsetDateTime;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PluginUpdates {
    pub name: String,
    pub current_version: String,
    pub new_version: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PluginStore {
    pub plugins: Vec<StorePlugin>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PastStorePlugin {
    pub p_type: String,
    pub repo: String,
    pub tag: String,
    pub source: String,
    pub version: String,
    pub added: String,
    pub replaced: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct StorePlugin {
    pub name: String,
    pub p_type: String,
    pub repo: String,
    pub tag: String,
    pub source: String,
    pub added: String,
    pub version: String,
    pub updated: String,
    pub past_versions: Vec<PastStorePlugin>,
}

pub struct RuntimeMetadata {
    pub started: Arc<OffsetDateTime>,
}

pub enum PluginRuntime {
    BuiltIn,
    Docker(RuntimeMetadata),
    File(RuntimeMetadata),
}

pub struct PluginManager {
    bin_folder: PathBuf,
    plugins: HashMap<String, Plugin>,
    plugin_runtimes: HashMap<String, PluginRuntime>,
    available_plugins: HashMap<String, StorePlugin>,
    start_time: Arc<OffsetDateTime>,
}
impl PluginManager {
    pub async fn new(db: &SqlitePool, bin_folder: PathBuf) -> Self {
        let plugins = get_all_plugins(db).await.unwrap_or_default();
        let mut manager = Self {
            bin_folder,
            plugins: HashMap::from_iter(plugins.into_iter().map(|v| (v.name.clone(), v))),
            plugin_runtimes: Default::default(),
            available_plugins: Default::default(),
            start_time: Arc::new(OffsetDateTime::now_utc()),
        };
        manager.update_plugin_store().await.ok().unwrap_or_default();
        //Install the builtin Plugins
        manager.plugins.insert(
            "file_manager".to_string(),
            Plugin {
                id: None,
                label: "File Manager".to_string(),
                name: "file_manager".to_string(),
                enabled: 1,
                plugin_type: PluginType::BuiltIn,
                repo: "https://github.com/GalactechsLLC/dg_xch_os".to_string(),
                tag: "".to_string(),
                source: "".to_string(),
                run_command: None,
                version: version().to_string(),
                added: OffsetDateTime::now_utc(),
                updated: OffsetDateTime::now_utc(),
            },
        );
        manager
            .plugin_runtimes
            .insert("file_manager".to_string(), PluginRuntime::BuiltIn);
        manager.plugins.insert(
            "disk_manager".to_string(),
            Plugin {
                id: None,
                label: "Disk Manager".to_string(),
                name: "disk_manager".to_string(),
                enabled: 1,
                plugin_type: PluginType::BuiltIn,
                repo: "https://github.com/GalactechsLLC/dg_xch_os".to_string(),
                tag: "".to_string(),
                source: "".to_string(),
                run_command: None,
                version: version().to_string(),
                added: OffsetDateTime::now_utc(),
                updated: OffsetDateTime::now_utc(),
            },
        );
        manager
            .plugin_runtimes
            .insert("disk_manager".to_string(), PluginRuntime::BuiltIn);

        manager.plugins.insert(
            "system_monitor".to_string(),
            Plugin {
                id: None,
                label: "System Monitor".to_string(),
                name: "system_monitor".to_string(),
                enabled: 1,
                plugin_type: PluginType::BuiltIn,
                repo: "https://github.com/GalactechsLLC/dg_xch_os".to_string(),
                tag: "".to_string(),
                source: "".to_string(),
                run_command: None,
                version: version().to_string(),
                added: OffsetDateTime::now_utc(),
                updated: OffsetDateTime::now_utc(),
            },
        );
        manager
            .plugin_runtimes
            .insert("system_monitor".to_string(), PluginRuntime::BuiltIn);

        manager.plugins.insert(
            "farmer_manager".to_string(),
            Plugin {
                id: None,
                label: "Fast Farmer".to_string(),
                name: "farmer_manager".to_string(),
                enabled: 1,
                plugin_type: PluginType::BuiltIn,
                repo: "https://builds.druid.garden/".to_string(),
                tag: "fast_farmer_gh".to_string(),
                source: "".to_string(),
                run_command: None,
                version: version().to_string(),
                added: OffsetDateTime::now_utc(),
                updated: OffsetDateTime::now_utc(),
            },
        );
        manager
            .plugin_runtimes
            .insert("farmer_manager".to_string(), PluginRuntime::BuiltIn);
        manager
    }
    pub async fn available_plugins(&self) -> Vec<StorePlugin> {
        self.available_plugins.values().cloned().collect()
    }
    pub async fn add(&mut self, plugin: AddPlugin, db: &SqlitePool) -> Result<Plugin, Error> {
        let plugin: Plugin = plugin.into();
        plugin.validate_external()?;
        if self.plugins.contains_key(&plugin.name) {
            return Err(Error::new(
                ErrorKind::AlreadyExists,
                "Plugin already exists",
            ));
        }
        let plugin = create_plugin(db, &plugin)
            .await?
            .ok_or_else(|| Error::other("Plugin was not saved"))?;
        self.plugins.insert(plugin.name.clone(), plugin.clone());
        Ok(plugin)
    }
    pub async fn update_plugin(
        &mut self,
        plugin: AddPlugin,
        db: &SqlitePool,
    ) -> Result<Plugin, Error> {
        let plugin: Plugin = plugin.into();
        plugin.validate_external()?;
        let current = self
            .plugins
            .get(&plugin.name)
            .cloned()
            .ok_or_else(|| Error::new(ErrorKind::NotFound, "Plugin not found"))?;
        current.validate_external()?;
        if self.status(current).await?.running {
            return Err(Error::new(
                ErrorKind::WouldBlock,
                "Stop the plugin before updating it",
            ));
        }
        let plugin = create_plugin(db, &plugin)
            .await?
            .ok_or_else(|| Error::other("Plugin was not saved"))?;
        self.plugins.insert(plugin.name.clone(), plugin.clone());
        self.plugin_runtimes.remove(&plugin.name);
        Ok(plugin)
    }
    pub async fn start(&mut self, plugin: Plugin) -> Result<bool, Error> {
        plugin.validate_external()?;
        if self.status(plugin.clone()).await?.running {
            return Err(Error::new(
                ErrorKind::AlreadyExists,
                "Plugin already running",
            ));
        }
        self.plugin_runtimes.remove(&plugin.name);
        let std::collections::hash_map::Entry::Vacant(entry) =
            self.plugin_runtimes.entry(plugin.name.clone())
        else {
            return Err(Error::other("Plugin runtime conflict"));
        };
        match plugin.plugin_type {
            PluginType::Docker => start_docker_plugin(entry, plugin).await?,
            PluginType::File => start_file_plugin(self.bin_folder.clone(), entry, plugin).await?,
            _ => {
                return Err(Error::new(
                    ErrorKind::Unsupported,
                    "Unsupported plugin type",
                ))
            }
        }
        Ok(true)
    }
    pub async fn stop(&mut self, plugin: Plugin) -> Result<bool, Error> {
        plugin.validate_external()?;
        match plugin.plugin_type {
            PluginType::Docker => {
                stop_owned_container(&docker_client()?, &plugin.name).await?;
            }
            PluginType::File => {
                stop_file_unit(&plugin.name).await?;
            }
            _ => {
                return Err(Error::new(
                    ErrorKind::Unsupported,
                    "Unsupported plugin type",
                ))
            }
        }
        self.plugin_runtimes.remove(&plugin.name);
        Ok(true)
    }
    pub async fn status(&self, plugin: Plugin) -> Result<PluginStatus, Error> {
        if matches!(
            self.plugin_runtimes.get(&plugin.name),
            Some(PluginRuntime::BuiltIn)
        ) {
            return Ok(PluginStatus {
                running: true,
                should_be_running: true,
                started: Some(*self.start_time),
            });
        }
        plugin.validate_external()?;
        let running = match plugin.plugin_type {
            PluginType::Docker => owned_container(&docker_client()?, &plugin.name)
                .await?
                .is_some_and(|(_, running)| running),
            PluginType::File => file_unit_running(&plugin.name).await?,
            _ => false,
        };
        let started = self
            .plugin_runtimes
            .get(&plugin.name)
            .and_then(|runtime| match runtime {
                PluginRuntime::Docker(metadata) | PluginRuntime::File(metadata) => {
                    Some(*metadata.started)
                }
                PluginRuntime::BuiltIn => None,
            });
        Ok(PluginStatus {
            running,
            should_be_running: self.plugin_runtimes.contains_key(&plugin.name),
            started,
        })
    }
    pub async fn update_plugin_store(&mut self) -> Result<bool, Error> {
        let plugin_yaml = String::from_utf8(
            fetch_bytes("https://plugins.druid.garden/plugins.yaml", 1024 * 1024).await?,
        )
        .map_err(Error::other)?;
        let plugin_store: PluginStore = serde_yaml::from_str(&plugin_yaml)
            .map_err(|e| Error::other(format!("Failed parsing yaml: {e}")))?;
        self.available_plugins = HashMap::from_iter(
            plugin_store
                .plugins
                .into_iter()
                .map(|v| (v.name.clone(), v)),
        );
        Ok(true)
    }
    pub async fn plugin_updates(&self) -> Result<Vec<PluginUpdates>, Error> {
        let mut updates = vec![];
        for (k, v) in self.plugins.iter() {
            if let Some(available) = self.available_plugins.get(k) {
                if let (Ok(current_version), Ok(available_version)) = (
                    Version::parse(&v.version),
                    Version::parse(&available.version),
                ) {
                    if available_version > current_version {
                        updates.push(PluginUpdates {
                            name: k.clone(),
                            current_version: current_version.to_string(),
                            new_version: available_version.to_string(),
                        });
                    }
                }
            }
        }
        Ok(updates)
    }
    pub async fn uninstall(&mut self, plugin: Plugin, db: &SqlitePool) -> Result<bool, Error> {
        plugin.validate_external()?;
        self.stop(plugin.clone()).await?;
        let removed = delete_plugin(db, &plugin.name).await? > 0;
        if removed {
            self.plugins.remove(&plugin.name);
        }
        Ok(removed)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginStatus {
    pub running: bool,
    pub should_be_running: bool,
    pub started: Option<OffsetDateTime>,
}

const OWNER_LABEL: &str = "io.druid-garden.managed";
const PLUGIN_LABEL: &str = "io.druid-garden.plugin";
const MAX_PLUGIN_BYTES: usize = 64 * 1024 * 1024;

fn container_name(name: &str) -> String {
    format!("dgos-plugin-{name}")
}
fn unit_name(name: &str) -> String {
    format!("dgos-plugin-{name}.service")
}
fn unit_description(name: &str) -> String {
    format!("Druid Garden isolated plugin: {name}")
}
fn docker_client() -> Result<Docker, Error> {
    Docker::connect_with_defaults().map_err(Error::other)
}

fn owned_labels(labels: Option<&HashMap<String, String>>, name: &str) -> bool {
    labels.is_some_and(|labels| {
        labels.get(OWNER_LABEL).map(String::as_str) == Some("true")
            && labels.get(PLUGIN_LABEL).map(String::as_str) == Some(name)
    })
}
async fn owned_container(docker: &Docker, name: &str) -> Result<Option<(String, bool)>, Error> {
    crate::models::plugins::validate_plugin_name(name)?;
    let inspected = tokio::time::timeout(
        Duration::from_secs(30),
        docker.inspect_container(&container_name(name), None),
    )
    .await
    .map_err(|_| Error::other("Container inspection timed out"))?;
    match inspected {
        Ok(container) => {
            if !owned_labels(
                container
                    .config
                    .as_ref()
                    .and_then(|config| config.labels.as_ref()),
                name,
            ) {
                return Err(Error::new(
                    ErrorKind::PermissionDenied,
                    "Refusing to touch a container not owned by this plugin manager",
                ));
            }
            Ok(Some((
                container
                    .id
                    .ok_or_else(|| Error::other("Container ID missing"))?,
                container
                    .state
                    .and_then(|state| state.running)
                    .unwrap_or(false),
            )))
        }
        Err(bollard::errors::Error::DockerResponseServerError {
            status_code: 404, ..
        }) => Ok(None),
        Err(error) => Err(Error::other(error)),
    }
}
async fn stop_owned_container(docker: &Docker, name: &str) -> Result<(), Error> {
    if let Some((id, running)) = owned_container(docker, name).await? {
        if running {
            tokio::time::timeout(Duration::from_secs(30), docker.stop_container(&id, None))
                .await
                .map_err(|_| Error::other("Container stop timed out"))?
                .map_err(Error::other)?;
        }
        tokio::time::timeout(Duration::from_secs(30), docker.remove_container(&id, None))
            .await
            .map_err(|_| Error::other("Container removal timed out"))?
            .map_err(Error::other)?;
    }
    Ok(())
}
fn docker_image(plugin: &Plugin) -> Result<String, Error> {
    let source = &plugin.source;
    if source.is_empty()
        || source.starts_with('-')
        || !source
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:/@-".contains(&byte))
    {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "Invalid Docker image reference",
        ));
    }
    if source.contains('@')
        || plugin.tag.is_empty()
        || source
            .rsplit('/')
            .next()
            .is_some_and(|part| part.contains(':'))
    {
        return Ok(source.clone());
    }
    if !plugin
        .tag
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "Invalid Docker image tag",
        ));
    }
    Ok(format!("{source}:{}", plugin.tag))
}
fn isolated_host_config() -> HostConfig {
    HostConfig {
        port_bindings: Some(HashMap::from([(
            "80/tcp".into(),
            Some(vec![PortBinding {
                host_ip: Some("127.0.0.1".into()),
                host_port: Some("8081".into()),
            }]),
        )])),
        cap_drop: Some(vec!["ALL".into()]),
        security_opt: Some(vec!["no-new-privileges:true".into()]),
        readonly_rootfs: Some(true),
        memory: Some(512 * 1024 * 1024),
        memory_swap: Some(512 * 1024 * 1024),
        pids_limit: Some(128),
        nano_cpus: Some(1_000_000_000),
        tmpfs: Some(HashMap::from([(
            "/tmp".into(),
            "rw,noexec,nosuid,nodev,size=64m".into(),
        )])),
        ..Default::default()
    }
}
pub async fn start_docker_plugin(
    entry: VacantEntry<'_, String, PluginRuntime>,
    plugin: Plugin,
) -> Result<(), Error> {
    plugin.validate_external()?;
    let docker = docker_client()?;
    let image = docker_image(&plugin)?;
    // Resolve ownership before any destructive operation. Names alone confer no authority.
    stop_owned_container(&docker, &plugin.name).await?;
    tokio::time::timeout(Duration::from_secs(120), async {
        let mut progress = docker.create_image(
            Some(CreateImageOptions {
                from_image: image.clone(),
                ..Default::default()
            }),
            None,
            None,
        );
        while let Some(message) = progress.next().await {
            message.map_err(Error::other)?;
        }
        Ok::<(), Error>(())
    })
    .await
    .map_err(|_| Error::other("Plugin image pull timed out"))??;
    // Pin this launch to the pulled image ID, not a mutable tag.
    let image_id = docker
        .inspect_image(&image)
        .await
        .map_err(Error::other)?
        .id
        .ok_or_else(|| Error::other("Image ID missing"))?;
    let container = docker
        .create_container(
            Some(CreateContainerOptions {
                name: container_name(&plugin.name),
                platform: None,
            }),
            Config {
                image: Some(image_id),
                user: Some("65532:65532".to_string()),
                labels: Some(HashMap::from([
                    (OWNER_LABEL.to_string(), "true".to_string()),
                    (PLUGIN_LABEL.to_string(), plugin.name.clone()),
                ])),
                exposed_ports: Some(HashMap::from([("80/tcp".to_string(), HashMap::new())])),
                host_config: Some(isolated_host_config()),
                ..Default::default()
            },
        )
        .await
        .map_err(Error::other)?;
    docker
        .start_container::<String>(&container.id, None)
        .await
        .map_err(Error::other)?;
    entry.insert(PluginRuntime::Docker(RuntimeMetadata {
        started: Arc::new(OffsetDateTime::now_utc()),
    }));
    Ok(())
}

fn download_client() -> Result<reqwest::Client, Error> {
    reqwest::Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(Error::other)
}
async fn fetch_bytes(url: &str, limit: usize) -> Result<Vec<u8>, Error> {
    let mut response = download_client()?
        .get(url)
        .send()
        .await
        .map_err(Error::other)?
        .error_for_status()
        .map_err(Error::other)?;
    if response.status().is_redirection()
        || response
            .content_length()
            .is_some_and(|length| length > limit as u64)
    {
        return Err(Error::other("Plugin response exceeds limits or redirects"));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(Error::other)? {
        if bytes.len().saturating_add(chunk.len()) > limit {
            return Err(Error::other("Plugin response exceeds limit"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
fn plugin_url(plugin: &Plugin) -> Result<reqwest::Url, Error> {
    let mut url = reqwest::Url::parse(&plugin.repo).map_err(Error::other)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "Plugins require an HTTPS repository without URL credentials",
        ));
    }
    if plugin.source.is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "Plugin source is required",
        ));
    }
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| Error::other("Invalid repository URL"))?;
        segments.pop_if_empty();
        for part in [&plugin.tag, &plugin.source]
            .into_iter()
            .flat_map(|value| value.split('/'))
            .filter(|part| !part.is_empty())
        {
            if part == "." || part == ".." {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "Plugin source cannot traverse directories",
                ));
            }
            segments.push(part);
        }
    }
    Ok(url)
}
fn trusted_plugin_directory(root: &std::path::Path, name: &str) -> Result<PathBuf, Error> {
    // The root is provisioned by the OS, never created through the web API.
    if !root.is_absolute() || root.canonicalize()? != root {
        return Err(Error::new(
            ErrorKind::PermissionDenied,
            "Plugin root must be an absolute path without symlinks",
        ));
    }
    for ancestor in root.ancestors() {
        let metadata = std::fs::symlink_metadata(ancestor)?;
        if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
            return Err(Error::new(
                ErrorKind::PermissionDenied,
                "Plugin root must be root-owned and not writable by other users",
            ));
        }
    }
    let directory = root.join(name);
    match std::fs::create_dir(&directory) {
        Ok(()) => std::fs::set_permissions(&directory, Permissions::from_mode(0o755))?,
        Err(error) if error.kind() == ErrorKind::AlreadyExists => (),
        Err(error) => return Err(error),
    }
    let metadata = std::fs::symlink_metadata(&directory)?;
    if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
        return Err(Error::new(
            ErrorKind::PermissionDenied,
            "Unsafe plugin directory",
        ));
    }
    Ok(directory)
}
async fn system_command(program: &str, args: &[String]) -> Result<std::process::Output, Error> {
    tokio::time::timeout(
        Duration::from_secs(30),
        Command::new(program)
            .args(args)
            .env_clear()
            .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| Error::other("Plugin service operation timed out"))?
}
async fn file_unit_running(name: &str) -> Result<bool, Error> {
    crate::models::plugins::validate_plugin_name(name)?;
    let output = system_command(
        "/usr/bin/systemctl",
        &[
            "show".into(),
            "--no-pager".into(),
            "--property=LoadState,Description,ActiveState".into(),
            unit_name(name),
        ],
    )
    .await?;
    let text = String::from_utf8_lossy(&output.stdout);
    if text.lines().any(|line| line == "LoadState=not-found") {
        return Ok(false);
    }
    if !output.status.success() {
        return Err(Error::other("Cannot inspect plugin unit"));
    }
    let expected = format!("Description={}", unit_description(name));
    if !text.lines().any(|line| line == expected) {
        return Err(Error::new(
            ErrorKind::PermissionDenied,
            "Refusing to touch a service not owned by this plugin manager",
        ));
    }
    Ok(text
        .lines()
        .any(|line| matches!(line, "ActiveState=active" | "ActiveState=activating")))
}
async fn stop_file_unit(name: &str) -> Result<(), Error> {
    // Inspect ownership even for inactive services; never issue a stop for an
    // unrelated unit. Collected/nonexistent units are already stopped.
    if file_unit_running(name).await? {
        let output =
            system_command("/usr/bin/systemctl", &["stop".into(), unit_name(name)]).await?;
        if !output.status.success() {
            return Err(Error::other("Failed to stop isolated plugin service"));
        }
    }
    Ok(())
}
fn isolated_unit_args(name: &str, executable: &std::path::Path) -> Vec<String> {
    let mut args = vec![
        "--quiet".into(),
        "--collect".into(),
        "--service-type=exec".into(),
        format!("--unit={}", unit_name(name)),
        format!("--description={}", unit_description(name)),
    ];
    for property in ["DynamicUser=yes", "ProtectSystem=strict", "ProtectHome=yes", "PrivateTmp=yes", "PrivateDevices=yes", "NoNewPrivileges=yes", "RestrictSUIDSGID=yes", "ProtectKernelTunables=yes", "ProtectKernelModules=yes", "ProtectControlGroups=yes", "RestrictNamespaces=yes", "LockPersonality=yes", "CapabilityBoundingSet=", "AmbientCapabilities=", "MemoryMax=512M", "TasksMax=128", "CPUQuota=100%", "LimitCORE=0", "LimitNOFILE=1024", "KillMode=control-group", "TimeoutStopSec=15s", "InaccessiblePaths=-/var/lib/druid-garden -/etc/druid-garden -/run/docker.sock -/var/run/docker.sock"] {
        args.push(format!("--property={property}"));
    }
    args.push("--".into());
    args.push(executable.to_string_lossy().into_owned());
    args
}
pub async fn start_file_plugin(
    bin_folder: PathBuf,
    entry: VacantEntry<'_, String, PluginRuntime>,
    plugin: Plugin,
) -> Result<(), Error> {
    plugin.validate_external()?;
    let directory = trusted_plugin_directory(&bin_folder, &plugin.name)?;
    let url = plugin_url(&plugin)?;
    let bytes = fetch_bytes(url.as_str(), MAX_PLUGIN_BYTES).await?;
    if bytes.is_empty() {
        return Err(Error::other("Downloaded plugin is empty"));
    }
    let staged = tempfile::NamedTempFile::new_in(&directory)?;
    let mut file = tokio::fs::File::from_std(staged.as_file().try_clone()?);
    file.write_all(&bytes).await?;
    file.sync_all().await?;
    drop(file);
    staged
        .as_file()
        .set_permissions(Permissions::from_mode(0o755))?;
    let executable = directory.join("plugin");
    staged.persist(&executable).map_err(Error::other)?;
    std::fs::File::open(&directory)?.sync_all()?;
    let output = system_command(
        "/usr/bin/systemd-run",
        &isolated_unit_args(&plugin.name, &executable),
    )
    .await?;
    if !output.status.success() {
        return Err(Error::other(
            "Failed to start isolated plugin service; verify systemd sandbox support",
        ));
    }
    entry.insert(PluginRuntime::File(RuntimeMetadata {
        started: Arc::new(OffsetDateTime::now_utc()),
    }));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn container_ownership_and_isolation_are_explicit() {
        assert!(!owned_labels(None, "test"));
        let mut labels = HashMap::from([
            (OWNER_LABEL.to_string(), "true".to_string()),
            (PLUGIN_LABEL.to_string(), "other".to_string()),
        ]);
        assert!(!owned_labels(Some(&labels), "test"));
        labels.insert(PLUGIN_LABEL.into(), "test".into());
        assert!(owned_labels(Some(&labels), "test"));
        let policy = isolated_host_config();
        assert_eq!(policy.readonly_rootfs, Some(true));
        assert_eq!(policy.cap_drop, Some(vec!["ALL".into()]));
        assert_eq!(policy.pids_limit, Some(128));
        assert_eq!(
            policy.port_bindings.unwrap()["80/tcp"].as_ref().unwrap()[0]
                .host_ip
                .as_deref(),
            Some("127.0.0.1")
        );
    }
    #[test]
    fn file_units_require_privilege_and_resource_isolation() {
        let args = isolated_unit_args(
            "safe",
            std::path::Path::new("/opt/druid-garden/plugins/safe/plugin"),
        );
        for setting in [
            "DynamicUser=yes",
            "ProtectSystem=strict",
            "ProtectHome=yes",
            "PrivateDevices=yes",
            "NoNewPrivileges=yes",
            "CapabilityBoundingSet=",
            "TasksMax=128",
        ] {
            assert!(args.contains(&format!("--property={setting}")));
        }
        assert_eq!(args[args.len() - 2], "--");
    }
}
