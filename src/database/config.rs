use crate::database::map_sqlx_error;
use crate::models::config::{AddConfigEntry, ConfigEntry};
use sqlx::SqlitePool;
use std::io::Error;
use time::OffsetDateTime;

pub async fn get_config(pool: &SqlitePool) -> Result<Vec<ConfigEntry>, Error> {
    sqlx::query_as!(
        ConfigEntry,
        r#"
        SELECT key, value, last_value, category, system, created, modified
        FROM config
        "#
    )
    .fetch_all(pool)
    .await
    .map_err(map_sqlx_error)
}
pub async fn get_config_key(pool: &SqlitePool, key: &str) -> Result<Option<ConfigEntry>, Error> {
    let results = sqlx::query_as!(
        ConfigEntry,
        r#"
        SELECT key, value, last_value, category, system, created, modified
        FROM config
        WHERE key = $1
        "#,
        key
    )
    .fetch_one(pool)
    .await
    .map(Some);
    match results {
        Ok(result) => Ok(result),
        Err(sqlx::Error::RowNotFound) => Ok(None),
        Err(e) => Err(map_sqlx_error(e)),
    }
}
pub async fn create_config_entry(
    pool: &SqlitePool,
    entry: &AddConfigEntry,
) -> Result<Option<ConfigEntry>, Error> {
    let now = OffsetDateTime::now_utc();
    let key = sqlx::query!(
        r#"
        INSERT INTO config (key, value, last_value, category, system, created, modified)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        ON CONFLICT (key)
        DO UPDATE SET
            last_value = value,
            value = EXCLUDED.value,
            category = EXCLUDED.category,
            modified = EXCLUDED.modified
        RETURNING key
        "#,
        entry.key,
        entry.value,
        entry.last_value,
        entry.category,
        entry.system,
        now,
        now,
    )
    .fetch_one(pool)
    .await
    .map_err(map_sqlx_error)?
    .key;
    get_config_key(pool, &key).await
}
pub async fn delete_config_entry(pool: &SqlitePool, key: &str) -> Result<u64, Error> {
    sqlx::query!(
        r#"
        DELETE FROM config
        WHERE key = $1
        "#,
        key
    )
    .execute(pool)
    .await
    .map(|r| r.rows_affected())
    .map_err(map_sqlx_error)
}
