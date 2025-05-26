-- Add up migration script here
CREATE TABLE IF NOT EXISTS config (
    key TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL,
    last_value TEXT NOT NULL,
    category TEXT NOT NULL,
    system INTEGER NOT NULL,
    created DATETIME NOT NULL,
    modified DATETIME NOT NULL
);
CREATE TABLE IF NOT EXISTS plugins (
    id INTEGER PRIMARY KEY NOT NULL,
    label TEXT NOT NULL,
    name TEXT UNIQUE NOT NULL,
    enabled INTEGER NOT NULL,
    plugin_type TEXT NOT NULL,
    source TEXT NOT NULL,
    run_command TEXT NOT NULL,
    repo TEXT NOT NULL,
    tag TEXT NOT NULL,
    version TEXT NOT NULL,
    added DATETIME NOT NULL,
    updated DATETIME NOT NULL
);
CREATE TABLE IF NOT EXISTS plugin_environment (
    plugin_id INTEGER NOT NULL,
    key TEXT NOT NULL,
    value TEXT NOT NULL,
    added DATETIME NOT NULL,
    updated DATETIME NOT NULL,
    FOREIGN KEY(plugin_id) REFERENCES plugins(id),
    UNIQUE (plugin_id, key)
);
CREATE TABLE IF NOT EXISTS "users" (
    "id" INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
    "username" TEXT NOT NULL,
    "password" BLOB NOT NULL,
    "role" TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS "linked_oAuth" (
    "user" BIGINT NOT NULL,
    "provider" TEXT NOT NULL,
    "id" TEXT NOT NULL,
    FOREIGN KEY(user) REFERENCES users(id)
);
CREATE TABLE IF NOT EXISTS farmer_stats (
    challenge_hash BLOB NOT NULL,
    sp_hash BLOB NOT NULL,
    running INTEGER NOT NULL,
    og_plot_count INTEGER NOT NULL,
    nft_plot_count INTEGER NOT NULL,
    compresses_plot_count INTEGER NOT NULL,
    invalid_plot_count INTEGER NOT NULL,
    total_plot_space INTEGER NOT NULL,
    full_node_height INTEGER NOT NULL,
    full_node_difficulty INTEGER NOT NULL,
    full_node_synced INTEGER NOT NULL,
    gathered DATETIME NOT NULL,
    UNIQUE (challenge_hash, sp_hash)
);
