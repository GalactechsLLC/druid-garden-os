use std::io::Error;

pub mod config;
pub mod plugins;
pub mod stats;
pub mod users;

pub fn map_sqlx_error(e: sqlx::Error) -> Error {
    println!("{e:?}");
    Error::other(e)
}
