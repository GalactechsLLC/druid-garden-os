use crate::database::config::{
    create_config_entry, delete_config_entry, get_config, get_config_key,
};
use crate::models::config::{AddConfigEntry, ConfigEntry};
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::io::{Error, ErrorKind};
use time::OffsetDateTime;

pub static DEFAULT_FULLNODE_WS_HOST: &str = "druid.garden";
pub static DEFAULT_FULLNODE_WS_PORT: u16 = 443;
pub static DEFAULT_FULLNODE_RPC_HOST: &str = "druid.garden";
pub static DEFAULT_FULLNODE_RPC_PORT: u16 = 443;

pub struct ConfigManager {
    entries: HashMap<String, ConfigEntry>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mismatched_keys_cannot_change_cached_identity() {
        let mut config = ConfigManager {
            entries: HashMap::new(),
        };
        let result = config
            .set(
                "theme",
                AddConfigEntry {
                    key: "heartbeat.device_id".into(),
                    value: "attacker".into(),
                    last_value: String::new(),
                    category: "system".into(),
                    system: 0,
                },
                None,
            )
            .await;
        assert_eq!(result.unwrap_err().kind(), ErrorKind::InvalidInput);
        assert!(config.get("theme").await.is_none());
        assert!(config.get("heartbeat.device_id").await.is_none());
    }

    #[tokio::test]
    async fn deletion_updates_database_and_live_cache() {
        let db = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query("CREATE TABLE config (key TEXT PRIMARY KEY, value TEXT, last_value TEXT, category TEXT, system INTEGER, created TEXT, modified TEXT)")
            .execute(&db).await.unwrap();
        let mut config = ConfigManager::new(&db).await.unwrap();
        config
            .set(
                "heartbeat.api_key",
                AddConfigEntry {
                    key: "heartbeat.api_key".into(),
                    value: "dummy-token".into(),
                    last_value: String::new(),
                    category: "system".into(),
                    system: 0,
                },
                Some(&db),
            )
            .await
            .unwrap();
        config.delete("heartbeat.api_key", &db).await.unwrap();
        assert!(config.get("heartbeat.api_key").await.is_none());
        assert!(get_config_key(&db, "heartbeat.api_key")
            .await
            .unwrap()
            .is_none());
    }
}
impl ConfigManager {
    pub async fn new(db: &SqlitePool) -> Result<ConfigManager, Error> {
        let entries = get_config(db).await?;
        Ok(Self {
            entries: entries.into_iter().map(|e| (e.key.clone(), e)).collect(),
        })
    }
    pub async fn get(&self, key: &str) -> Option<ConfigEntry> {
        self.entries.get(key).cloned()
    }
    pub async fn set(
        &mut self,
        key: &str,
        entry: AddConfigEntry,
        db: Option<&SqlitePool>,
    ) -> Result<Option<ConfigEntry>, Error> {
        if entry.key != key {
            return Err(Error::new(ErrorKind::InvalidInput, "Config key mismatch"));
        }
        if let Some(db) = db {
            create_config_entry(db, &entry).await?;
        }
        Ok(self.entries.insert(
            key.to_string(),
            ConfigEntry {
                key: entry.key.clone(),
                value: entry.value.clone(),
                last_value: entry.last_value.clone(),
                category: entry.category.clone(),
                system: entry.system,
                created: OffsetDateTime::now_utc(),
                modified: OffsetDateTime::now_utc(),
            },
        ))
    }
    pub async fn reload_key(&mut self, key: &str, db: &SqlitePool) -> Result<(), Error> {
        match get_config_key(db, key).await? {
            Some(entry) => {
                self.set(
                    key,
                    AddConfigEntry {
                        key: entry.key.clone(),
                        value: entry.value.clone(),
                        last_value: entry.last_value.clone(),
                        category: entry.category.clone(),
                        system: entry.system,
                    },
                    None,
                )
                .await?;
                Ok(())
            }
            None => Err(Error::new(
                ErrorKind::NotFound,
                format!("Failed to find entry with key: {key}"),
            )),
        }
    }
    pub async fn reload(&mut self, db: &SqlitePool) -> Result<(), Error> {
        self.entries = get_config(db)
            .await?
            .into_iter()
            .map(|e| (e.key.clone(), e))
            .collect();
        Ok(())
    }
    pub async fn save(&mut self, db: &SqlitePool) -> Result<(), Error> {
        for entry in self.entries.values() {
            create_config_entry(
                db,
                &AddConfigEntry {
                    key: entry.key.clone(),
                    value: entry.value.clone(),
                    last_value: entry.last_value.clone(),
                    category: entry.category.clone(),
                    system: entry.system,
                },
            )
            .await?;
        }
        Ok(())
    }
    pub async fn delete(&mut self, key: &str, db: &SqlitePool) -> Result<(), Error> {
        delete_config_entry(db, key).await?;
        self.entries.remove(key);
        Ok(())
    }
}
