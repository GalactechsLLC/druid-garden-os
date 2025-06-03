-- Add up migration script here
DROP TABLE IF EXISTS farmer_stats;
CREATE TABLE farmer_stats (
    challenge_hash BLOB NOT NULL,
    sp_hash BLOB NOT NULL,
    running BOOLEAN NOT NULL,
    og_passed_filter INTEGER NOT NULL,
    og_plot_count INTEGER NOT NULL,
    nft_passed_filter INTEGER NOT NULL,
    nft_plot_count INTEGER NOT NULL,
    compressed_passed_filter INTEGER NOT NULL,
    compressed_plot_count INTEGER NOT NULL,
    invalid_plot_count INTEGER NOT NULL,
    proofs_found INTEGER NOT NULL,
    total_plot_space INTEGER NOT NULL,
    full_node_height INTEGER NOT NULL,
    full_node_difficulty INTEGER NOT NULL,
    full_node_synced BOOLEAN NOT NULL,
    gathered DATETIME NOT NULL,
    UNIQUE (challenge_hash, sp_hash)
);