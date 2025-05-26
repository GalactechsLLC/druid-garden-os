use crate::database::map_sqlx_error;
use crate::models::plugins::{Plugin, PluginEnvironmentEntry};
use sqlx::SqlitePool;
use std::io::Error;

pub async fn get_all_plugins(pool: &SqlitePool) -> Result<Vec<Plugin>, Error> {
    sqlx::query_as!(
        Plugin,
        r#"
        SELECT id, label, name, enabled, plugin_type, source, run_command, repo, tag, version, added, updated
        FROM plugins
        "#
    )
    .fetch_all(pool)
    .await
    .map_err(map_sqlx_error)
}
pub async fn get_plugin(pool: &SqlitePool, name: &str) -> Result<Option<Plugin>, Error> {
    let results = sqlx::query_as!(
        Plugin,
        r#"
        SELECT id, label, name, enabled, plugin_type, source, run_command, repo, tag, version, added, updated
        FROM plugins
        WHERE name = $1
        "#,
        name
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
pub async fn create_plugin(pool: &SqlitePool, entry: &Plugin) -> Result<Option<Plugin>, Error> {
    let name = sqlx::query!(
        r#"
        INSERT INTO plugins (id, label, name, enabled, plugin_type, source, run_command, repo, tag, version, added, updated)
        VALUES (NULL, $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
        ON CONFLICT (name)
        DO UPDATE SET
            label = EXCLUDED.label,
            name = EXCLUDED.name,
            enabled = EXCLUDED.enabled,
            plugin_type = EXCLUDED.plugin_type,
            source = EXCLUDED.source,
            run_command = EXCLUDED.run_command,
            repo = EXCLUDED.repo,
            tag = EXCLUDED.tag,
            version = EXCLUDED.version
        RETURNING name
        "#,
        entry.label,
        entry.name,
        entry.enabled,
        entry.plugin_type,
        entry.source,
        entry.run_command,
        entry.repo,
        entry.tag,
        entry.version,
        entry.added,
        entry.updated
    )
    .fetch_one(pool)
    .await
    .map_err(map_sqlx_error)?
    .name;
    get_plugin(pool, &name).await
}
pub async fn delete_plugin(pool: &SqlitePool, name: &str) -> Result<u64, Error> {
    sqlx::query!(
        r#"
        DELETE FROM plugins
        WHERE name = $1
        "#,
        name
    )
    .execute(pool)
    .await
    .map(|r| r.rows_affected())
    .map_err(map_sqlx_error)
}

pub async fn get_plugin_environment_entries(
    pool: &SqlitePool,
    name: &str,
) -> Result<Vec<PluginEnvironmentEntry>, Error> {
    sqlx::query_as!(
        PluginEnvironmentEntry,
        r#"
        SELECT PE.plugin_id, PE.key, PE.value, PE.added, PE.updated
        FROM plugin_environment as PE
        LEFT JOIN plugins as P ON PE.plugin_id = P.id
        WHERE P.name = $1
        "#,
        name
    )
    .fetch_all(pool)
    .await
    .map_err(map_sqlx_error)
}
pub async fn get_plugin_environment_entry(
    pool: &SqlitePool,
    name: &str,
    key: &str,
) -> Result<Option<PluginEnvironmentEntry>, Error> {
    let results = sqlx::query_as!(
        PluginEnvironmentEntry,
        r#"
        SELECT PE.plugin_id, PE.key, PE.value, PE.added, PE.updated
        FROM plugin_environment as PE
        LEFT JOIN plugins as P ON PE.plugin_id = P.id
        WHERE P.name = $1 AND PE.key = $2
        "#,
        name,
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
pub async fn get_plugin_environment_entry_by_id(
    pool: &SqlitePool,
    plugin_id: i64,
    key: &str,
) -> Result<Option<PluginEnvironmentEntry>, Error> {
    let results = sqlx::query_as!(
        PluginEnvironmentEntry,
        r#"
        SELECT plugin_id, key, value, added, updated
        FROM plugin_environment
        WHERE plugin_id = $1 AND key = $2
        "#,
        plugin_id,
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
pub async fn create_plugin_environment_entry(
    pool: &SqlitePool,
    entry: &PluginEnvironmentEntry,
) -> Result<Option<PluginEnvironmentEntry>, Error> {
    let _ = sqlx::query!(
        r#"
        INSERT INTO plugin_environment (plugin_id, key, value, added, updated)
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT (plugin_id, key)
        DO UPDATE SET
            value = EXCLUDED.value,
            updated = EXCLUDED.updated
        "#,
        entry.plugin_id,
        entry.key,
        entry.value,
        entry.added,
        entry.updated
    )
    .execute(pool)
    .await
    .map_err(map_sqlx_error)?;
    get_plugin_environment_entry_by_id(pool, entry.plugin_id, &entry.key).await
}
pub async fn delete_plugin_environment_entry(
    pool: &SqlitePool,
    name: &str,
    key: &str,
) -> Result<u64, Error> {
    match get_plugin(pool, name).await? {
        Some(plugin) => sqlx::query!(
            r#"
                DELETE FROM plugin_environment
                WHERE plugin_id = $1 AND key = $2
                "#,
            plugin.id,
            key
        )
        .execute(pool)
        .await
        .map(|q| q.rows_affected())
        .map_err(map_sqlx_error),
        None => Ok(0),
    }
}
