use std::io::{Error, ErrorKind};

pub mod config;
pub mod plugins;
pub mod stats;
pub mod users;

pub fn map_sqlx_error(e: sqlx::Error) -> Error {
    println!("{e:?}");
    Error::new(ErrorKind::Other, e)
}
