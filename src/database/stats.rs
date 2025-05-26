use crate::database::map_sqlx_error;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::protocols::farmer::FarmerStats;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::io::Error;
use time::OffsetDateTime;

pub async fn get_farmer_stats_range(
    pool: &SqlitePool,
    start: OffsetDateTime,
    end: OffsetDateTime,
) -> Result<HashMap<(Bytes32, Bytes32), FarmerStats>, Error> {
    let mut results = HashMap::<(Bytes32, Bytes32), FarmerStats>::new();
    let rows = sqlx::query_as!(
        FarmerStats,
        r#"
        SELECT challenge_hash, sp_hash, running, og_plot_count, nft_plot_count, compresses_plot_count,
               invalid_plot_count, total_plot_space, full_node_height, full_node_difficulty,
               full_node_synced, gathered
        FROM farmer_stats
        WHERE gathered >= $1
        AND gathered <= $2
        "#,
        start,
        end
    )
        .fetch_all(pool)
        .await;
    match rows {
        Ok(rows) => {
            for row in rows {
                results.insert((row.sp_hash, row.challenge_hash), row);
            }
            Ok(results)
        }
        Err(sqlx::Error::RowNotFound) => Ok(results),
        Err(e) => Err(map_sqlx_error(e)),
    }
}

pub async fn has_farmer_stats(
    pool: &SqlitePool,
    challenge_hash: Bytes32,
    sp_hash: Bytes32,
) -> Result<bool, Error> {
    let challenge_hash: &[u8] = challenge_hash.as_ref();
    let sp_hash: &[u8] = sp_hash.as_ref();
    let row = sqlx::query!(
        r#"
        SELECT gathered
        FROM farmer_stats
        WHERE challenge_hash = $1
          AND sp_hash        = $2
        "#,
        challenge_hash,
        sp_hash
    )
    .fetch_one(pool)
    .await;
    match row {
        Ok(_) => Ok(true),
        Err(sqlx::Error::RowNotFound) => Ok(false),
        Err(e) => Err(map_sqlx_error(e)),
    }
}

pub async fn save_farmer_stats(
    pool: &SqlitePool,
    farmer_stats: FarmerStats,
) -> Result<bool, Error> {
    let challenge_hash: &[u8] = farmer_stats.challenge_hash.as_ref();
    let sp_hash: &[u8] = farmer_stats.sp_hash.as_ref();
    let q = sqlx::query!(
        r#"
        INSERT INTO farmer_stats (
            challenge_hash, sp_hash, running, og_plot_count,
            nft_plot_count, compresses_plot_count, invalid_plot_count,
            total_plot_space, full_node_height, full_node_difficulty,
            full_node_synced, gathered
        )
        VALUES ( $1, $2, $3,  $4,
                 $5, $6, $7,
                 $8, $9, $10,
                 $11, $12
        )
        ON CONFLICT(challenge_hash, sp_hash) DO UPDATE SET
            running               = excluded.running,
            og_plot_count         = excluded.og_plot_count,
            nft_plot_count        = excluded.nft_plot_count,
            compresses_plot_count = excluded.compresses_plot_count,
            invalid_plot_count    = excluded.invalid_plot_count,
            total_plot_space      = excluded.total_plot_space,
            full_node_height      = excluded.full_node_height,
            full_node_difficulty  = excluded.full_node_difficulty,
            full_node_synced      = excluded.full_node_synced,
            gathered              = excluded.gathered
        "#,
        challenge_hash,
        sp_hash,
        farmer_stats.running,
        farmer_stats.og_plot_count,
        farmer_stats.nft_plot_count,
        farmer_stats.compresses_plot_count,
        farmer_stats.invalid_plot_count,
        farmer_stats.total_plot_space,
        farmer_stats.full_node_height,
        farmer_stats.full_node_difficulty,
        farmer_stats.full_node_synced,
        farmer_stats.gathered
    );
    match q.execute(pool).await {
        Ok(rows) => Ok(rows.rows_affected() != 0),
        Err(e) => Err(map_sqlx_error(e)),
    }
}
