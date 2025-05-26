use portfu::prelude::serde_json;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

#[derive(Debug, Serialize, Deserialize)]
pub struct PreloadConfig {
    pub mnemonic: String,
    pub first_address: String,
    pub contract_address: String,
    pub payout_address: String,
    pub launcher_id: String,
    pub worker_name: String,
}
impl TryFrom<&Path> for PreloadConfig {
    type Error = std::io::Error;

    fn try_from(value: &Path) -> Result<Self, Self::Error> {
        serde_json::from_str::<PreloadConfig>(&fs::read_to_string(value)?)
            .map_err(|e| std::io::Error::new(ErrorKind::Other, format!("{:?}", e)))
    }
}
