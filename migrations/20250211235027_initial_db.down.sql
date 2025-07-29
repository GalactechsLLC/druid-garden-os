-- Add down migration script here
DROP TABLE IF EXISTS config;
DROP TABLE IF EXISTS plugin_environment;
DROP TABLE IF EXISTS plugins;
DROP TABLE IF EXISTS linked_oAuth;
DROP TABLE IF EXISTS users;
DROP TABLE IF EXISTS farmer_stats;
DROP TABLE IF EXISTS _sqlx_migrations;