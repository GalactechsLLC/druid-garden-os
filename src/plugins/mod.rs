pub mod disk_management;
pub mod farmer;
pub mod file_manager;
pub mod system_monitor;

use crate::database::plugins::{create_plugin, delete_plugin, get_all_plugins};
use crate::models::plugins::{AddPlugin, Plugin, PluginType};
use crate::version;
use bollard::container::{Config, CreateContainerOptions, ListContainersOptions};
use bollard::image::CreateImageOptions;
use bollard::service::{HostConfig, PortBinding};
use bollard::Docker;
use log::{error, info, warn};
use portfu::prelude::futures_util::StreamExt;
use semver::Version;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::collections::hash_map::{Entry, VacantEntry};
use std::collections::HashMap;
use std::fs::Permissions;
use std::io::{Error, ErrorKind};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use time::OffsetDateTime;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::select;
use tokio::task::JoinHandle;

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
    pub run: Option<Arc<AtomicBool>>,
    pub join_handle: Option<JoinHandle<Result<(), Error>>>,
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
            .insert("Disk Manager".to_string(), PluginRuntime::BuiltIn);

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
                name: "Fast Farmer".to_string(),
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
        //Check Loaded Plugins for Plugin with same name
        if self.plugins.contains_key(&plugin.name) {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!("Plugin {} already exists", plugin.name),
            ));
        }
        let plugin = plugin.into();
        create_plugin(db, &plugin).await?;
        self.plugins.insert(plugin.name.clone(), plugin.clone());
        Ok(plugin)
    }
    pub async fn update_plugin(
        &mut self,
        plugin: AddPlugin,
        db: &SqlitePool,
    ) -> Result<Plugin, Error> {
        //Check Loaded Plugins for Plugin with same name
        if !self.plugins.contains_key(&plugin.name) {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!("Plugin {} does not exists", plugin.name),
            ));
        }
        let plugin = plugin.into();
        create_plugin(db, &plugin).await?;
        Ok(plugin)
    }
    pub async fn start(&mut self, plugin: Plugin) -> Result<bool, Error> {
        match self.plugin_runtimes.entry(plugin.name.clone()) {
            Entry::Occupied(_) => Err(Error::new(
                ErrorKind::AlreadyExists,
                "Plugin Already Running",
            )),
            Entry::Vacant(entry) => {
                match plugin.plugin_type {
                    PluginType::BuiltIn => {}
                    PluginType::Docker => {
                        start_docker_plugin(entry, plugin).await?;
                    }
                    PluginType::RustProject => {
                        start_rust_plugin(entry, plugin).await?;
                    }
                    PluginType::File => {
                        start_file_plugin(self.bin_folder.clone(), entry, plugin).await?;
                    }
                    PluginType::Invalid => {
                        warn!("Tried to Start Invalid Plugin: {}", plugin.name);
                    }
                }
                Ok(true)
            }
        }
    }
    pub async fn stop(&mut self, plugin: Plugin) -> Result<bool, Error> {
        match self.plugin_runtimes.entry(plugin.name.clone()) {
            Entry::Occupied(runtime) => match runtime.remove() {
                PluginRuntime::Docker(mut metadata) => {
                    if let Some(run) = metadata.run.take() {
                        run.store(false, Ordering::Relaxed);
                    }
                    if let Some(handle) = metadata.join_handle.take() {
                        handle.abort();
                        match handle.await {
                            Ok(output) => {
                                if let Err(e) = output {
                                    return Err(Error::new(
                                        ErrorKind::Other,
                                        format!("Failed in plugin: {}", e),
                                    ));
                                }
                            }
                            Err(e) => {
                                return Err(Error::new(
                                    ErrorKind::Other,
                                    format!("Failed to join plugin thread: {}", e),
                                ));
                            }
                        }
                    }
                    info!("Connecting to Docker");
                    let docker = Docker::connect_with_defaults().map_err(|e| {
                        Error::new(
                            ErrorKind::Other,
                            format!("Failed to connect to docker: {}", e),
                        )
                    })?;
                    info!("Stopping Container");
                    docker
                        .stop_container(&plugin.name, None)
                        .await
                        .map_err(|e| {
                            Error::new(
                                ErrorKind::Other,
                                format!("Failed to stop docker container: {}", e),
                            )
                        })?;
                    info!("Removing Container");
                    docker
                        .remove_container(&plugin.name, None)
                        .await
                        .map_err(|e| {
                            Error::new(
                                ErrorKind::Other,
                                format!("Failed to remove docker container: {}", e),
                            )
                        })?;
                    Ok(true)
                }
                PluginRuntime::File(runtime) => {
                    if let Some(run) = runtime.run.as_ref() {
                        run.store(false, Ordering::Relaxed);
                    }
                    if let Some(handle) = runtime.join_handle {
                        handle.abort();
                        match handle.await {
                            Ok(Ok(_)) => {}
                            Ok(Err(e)) => {
                                error!("Error in Plugin: {}", e);
                            }
                            Err(e) => {
                                error!("Error Joining Plugin Thread: {}", e);
                            }
                        }
                    }
                    Ok(true)
                }
                PluginRuntime::BuiltIn => Err(Error::new(
                    ErrorKind::AlreadyExists,
                    "Built In Plugins Cant Be Stopped",
                )),
            },
            Entry::Vacant(_) => Ok(false),
        }
    }
    pub async fn status(&self, plugin: Plugin) -> Result<PluginStatus, Error> {
        match self.plugin_runtimes.get(&plugin.name) {
            Some(runtime) => match runtime {
                PluginRuntime::Docker(metadata) => {
                    info!("Connecting to Docker");
                    let docker = Docker::connect_with_defaults().map_err(|e| {
                        Error::new(
                            ErrorKind::Other,
                            format!("Failed to connect to docker: {}", e),
                        )
                    })?;
                    info!("Fetching Status");
                    docker
                        .inspect_container(&plugin.name, None)
                        .await
                        .map_err(|e| {
                            Error::new(
                                ErrorKind::Other,
                                format!("Failed to remove docker container: {}", e),
                            )
                        })?;
                    match docker
                        .list_containers(Some(ListContainersOptions {
                            all: true,
                            filters: HashMap::from([(
                                "name".to_string(),
                                vec![plugin.name.clone()],
                            )]),
                            ..Default::default()
                        }))
                        .await
                    {
                        Ok(containers) => match containers.first() {
                            None => Ok(PluginStatus {
                                running: false,
                                should_be_running: false,
                                started: None,
                            }),
                            Some(container) => Ok(PluginStatus {
                                running: container.state.as_ref().map(|v| v.to_ascii_lowercase())
                                    != Some("exited".to_string()),
                                should_be_running: if let Some(v) = &metadata.run {
                                    v.load(Ordering::Relaxed)
                                } else {
                                    plugin.enabled > 0
                                },
                                started: Some(*metadata.started),
                            }),
                        },
                        Err(e) => Err(Error::new(
                            ErrorKind::Other,
                            format!("Failed to Fetch Plugin Status: {e:?}"),
                        )),
                    }
                }
                PluginRuntime::File(runtime) => Ok(PluginStatus {
                    running: runtime
                        .join_handle
                        .as_ref()
                        .map(|h| !h.is_finished())
                        .unwrap_or(false),
                    should_be_running: if let Some(v) = &runtime.run {
                        v.load(Ordering::Relaxed)
                    } else {
                        plugin.enabled > 0
                    },
                    started: Some(*runtime.started),
                }),
                PluginRuntime::BuiltIn => Ok(PluginStatus {
                    running: true,
                    should_be_running: true,
                    started: Some(*self.start_time),
                }),
            },
            None => {
                if self.plugins.contains_key(&plugin.name) {
                    Ok(PluginStatus {
                        running: false,
                        should_be_running: false,
                        started: None,
                    })
                } else {
                    Err(Error::new(ErrorKind::NotFound, "Plugin Does Not Exist"))
                }
            }
        }
    }
    pub async fn update_plugin_store(&mut self) -> Result<bool, Error> {
        let plugin_url = "https://plugins.druid.garden/plugins.yaml";
        let plugin_yaml = if plugin_url.starts_with("http") {
            reqwest::get(plugin_url)
                .await
                .map_err(|e| {
                    Error::new(
                        ErrorKind::Other,
                        format!("Failed fetching plugin store: {}", e),
                    )
                })?
                .text()
                .await
                .map_err(|e| Error::new(ErrorKind::Other, format!("Failed reading body: {}", e)))?
        } else {
            let mut file = tokio::fs::File::open(&plugin_url).await?;
            let mut buf = String::new();
            file.read_to_string(&mut buf).await?;
            buf.trim().to_string()
        };
        let plugin_store: PluginStore = serde_yaml::from_str(&plugin_yaml)
            .map_err(|e| Error::new(ErrorKind::Other, format!("Failed parsing yaml: {}", e)))?;
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
        if let Some(PluginRuntime::BuiltIn) = self.plugin_runtimes.get(&plugin.name) {
            Err(Error::new(
                ErrorKind::PermissionDenied,
                "Unable to Install Builtin Plugins",
            ))
        } else {
            let _ = self.plugins.remove(&plugin.name);
            let _ = self.plugin_runtimes.get(&plugin.name);
            delete_plugin(db, &plugin.name).await.map(|v| v > 0)
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginStatus {
    pub running: bool,
    pub should_be_running: bool,
    pub started: Option<OffsetDateTime>,
}

pub async fn start_rust_plugin(
    _entry: VacantEntry<'_, String, PluginRuntime>,
    _plugin: Plugin,
) -> Result<(), Error> {
    Err(Error::new(
        ErrorKind::Unsupported,
        "Rust Projects Not Supported Yet",
    ))
}

pub async fn start_file_plugin(
    bin_folder: PathBuf,
    entry: VacantEntry<'_, String, PluginRuntime>,
    plugin: Plugin,
) -> Result<(), Error> {
    info!("Starting Plugin: {}", plugin.name);
    let working_directory = bin_folder.join(&plugin.name).canonicalize()?;
    let file_path = working_directory.join(&plugin.name);
    if !file_path.exists() {
        tokio::fs::create_dir_all(&working_directory).await?;
        let url = format!("{}/{}/{}", &plugin.repo, &plugin.tag, &plugin.source)
            .replace("//", "/")
            .replace("//", "/");
        info!("Fetching Plugin From: {}", url);
        let response = reqwest::get(url)
            .await
            .map_err(|e| Error::new(ErrorKind::Other, format!("Failed to fetch file: {}", e)))?;
        let mut file = tokio::fs::File::create(&file_path).await?;
        file.write_all(
            response
                .bytes()
                .await
                .map_err(|e| {
                    Error::new(
                        ErrorKind::Other,
                        format!("Failed to read file from response: {}", e),
                    )
                })?
                .as_ref(),
        )
        .await?;
        file.set_permissions(Permissions::from_mode(0o755)).await?;
        info!("Created File at: {file_path:?}");
    }
    info!("Setting Working Directory for Plugin to: {working_directory:?}");
    let mut command = if let Some(run_cmd) = &plugin.run_command {
        Command::new(run_cmd)
    } else {
        Command::new(&plugin.name)
    };
    command.current_dir(working_directory);
    command.kill_on_drop(true);
    let run = Arc::new(AtomicBool::new(true));
    let handle_run = run.clone();
    let plugin_name = plugin.name.clone();
    entry.insert(PluginRuntime::File(RuntimeMetadata {
        run: Some(run),
        join_handle: Some(tokio::spawn(async move {
            info!("Calling Command: {command:?}");
            select! {
                output = command.output() => {
                    if let Err(e) = output {
                        error!("Plugin {plugin_name} Exited: {}", e);
                    }
                },
                () = async move {
                    while handle_run.load(Ordering::Relaxed) {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                } => (),
            }
            Ok(())
        })),
        started: Arc::new(OffsetDateTime::now_utc()),
    }));
    Ok(())
}

pub async fn start_docker_plugin(
    entry: VacantEntry<'_, String, PluginRuntime>,
    plugin: Plugin,
) -> Result<(), Error> {
    info!("Connecting to Docker");
    let docker = Docker::connect_with_defaults().map_err(|e| {
        Error::new(
            ErrorKind::Other,
            format!("Failed to connect to docker: {}", e),
        )
    })?;
    let mut image_progress = docker.create_image(
        Some(CreateImageOptions {
            from_image: plugin.source.clone(),
            repo: plugin.repo.clone(),
            tag: plugin.tag.clone(),
            ..Default::default()
        }),
        None,
        None,
    );
    while let Some(message) = image_progress.next().await {
        match message {
            Ok(progress) => {
                // You can inspect the progress message here.
                println!("{:?}", progress);
            }
            Err(e) => {
                // Handle any errors encountered during image creation.
                eprintln!("Error creating image: {}", e);
            }
        }
    }
    info!("Checking if Container exists");
    match docker
        .list_containers(Some(ListContainersOptions {
            all: true,
            filters: HashMap::from([("name".to_string(), vec![plugin.name.clone()])]),
            ..Default::default()
        }))
        .await
    {
        Ok(list) => {
            match list.first() {
                None => {
                    info!("No Existing Container found");
                }
                Some(_) => {
                    info!("Found Existing Container, Shutting Down");
                    if let Err(e) = docker.stop_container(&plugin.name, None).await {
                        eprintln!("Error stopping container: {}", e);
                        return Err(Error::new(
                            ErrorKind::Other,
                            format!("Failed to stop container: {}", e),
                        ));
                    }
                    // Remove the container
                    if let Err(e) = docker.remove_container(&plugin.name, None).await {
                        eprintln!("Error removing container: {}", e);
                        return Err(Error::new(
                            ErrorKind::Other,
                            format!("Failed to remove container: {}", e),
                        ));
                    }
                }
            }
        }
        Err(e) => {
            eprintln!("Error listing containers: {}", e);
            return Err(Error::new(
                ErrorKind::Other,
                format!("Failed to list containers: {}", e),
            ));
        }
    }
    let mut exposed_ports = HashMap::new();
    exposed_ports.insert("80/tcp".to_string(), HashMap::new());
    let mut port_bindings = HashMap::new();
    port_bindings.insert(
        "80/tcp".to_string(),
        Some(vec![PortBinding {
            host_ip: Some("0.0.0.0".to_string()),
            host_port: Some("8081".to_string()),
        }]),
    );
    let host_config = HostConfig {
        port_bindings: Some(port_bindings),
        ..Default::default()
    };
    info!("Creating Docker Runtime");
    let _ = docker
        .create_container(
            Some(CreateContainerOptions {
                name: plugin.name.clone(),
                platform: None,
            }),
            Config {
                image: Some(plugin.source.clone()),
                exposed_ports: Some(exposed_ports),
                host_config: Some(host_config),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| {
            error!("Failed to create Docker Container: {}", e);
            Error::new(
                ErrorKind::Other,
                format!("Failed to create docker container: {}", e),
            )
        })?;
    //Start the Plugin
    info!("Starting Docker Runtime");
    docker
        .start_container::<String>(&plugin.name, None)
        .await
        .map_err(|e| {
            Error::new(
                ErrorKind::Other,
                format!("Failed to start docker container: {}", e),
            )
        })?;
    entry.insert(PluginRuntime::Docker(RuntimeMetadata {
        run: Some(Arc::new(AtomicBool::new(true))),
        join_handle: None,
        started: Arc::new(OffsetDateTime::now_utc()),
    }));
    Ok(())
}
