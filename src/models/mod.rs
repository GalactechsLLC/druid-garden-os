use std::env;
use std::io::Error;

pub mod config;
pub mod plugins;

pub struct ServerSettings {
    pub hostname: String,
    pub port: u16,
    pub database_path: String,
    pub plugin_path: String,
}

impl ServerSettings {
    pub fn from_env() -> Result<Self, Error> {
        let hostname = env::var("DG_HOSTNAME").unwrap_or("0.0.0.0".to_string());
        let port = env::var("DG_PORT")
            .map(|s| s.parse().unwrap())
            .unwrap_or(8080u16);
        let database_path =
            env::var("DATABASE_FILE").unwrap_or(String::from("druid_garden.sqlite"));
        let plugin_path = env::var("DG_BIN_PATH").unwrap_or(String::from("./plugins"));
        Ok(ServerSettings {
            hostname,
            port,
            database_path,
            plugin_path,
        })
    }
}
