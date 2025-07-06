use crate::plugins::led_manager::{LedColorMode, LedManager, PinColor};
use portfu::prelude::{Path, State};
use portfu_core::Json;
use portfu_macros::{delete, get, post};
use serde::{Deserialize, Serialize};
use std::io::{Error, ErrorKind};
use std::str::FromStr;
use tokio::sync::RwLock;

#[derive(Debug, Deserialize, Serialize)]
pub struct PinUpdatePayload {
    pub pin: u32,
    pub color: PinColor,
}
#[get("/led/brightness", output = "json", eoutput = "bytes")]
pub async fn get_brightness(led_manager: State<RwLock<LedManager>>) -> Result<u8, Error> {
    Ok(led_manager.0.read().await.get_brightness())
}

#[post("/led/brightness/{brightness}", output = "json", eoutput = "bytes")]
pub async fn set_brightness(
    led_manager: State<RwLock<LedManager>>,
    brightness: Path,
) -> Result<(), Error> {
    match u8::from_str(&brightness.inner()) {
        Ok(payload) => {
            led_manager.0.write().await.set_brightness(payload).await;
            Ok(())
        }
        Err(e) => Err(Error::new(
            ErrorKind::InvalidInput,
            format!("Invalid Pin Number: {e:?}"),
        )),
    }
}

#[get("/led/pin/{pin}", output = "json", eoutput = "bytes")]
pub async fn get_pin_value(led_manager: State<RwLock<LedManager>>, pin: Path) -> Result<u8, Error> {
    match u32::from_str(&pin.inner()) {
        Ok(payload) => led_manager.0.read().await.get_pin_value(payload).await,
        Err(e) => Err(Error::new(
            ErrorKind::InvalidInput,
            format!("Invalid Pin Number: {e:?}"),
        )),
    }
}

#[post("/led/pin", output = "json", eoutput = "bytes")]
pub async fn set_pin_mode(
    led_manager: State<RwLock<LedManager>>,
    payload: Json<Option<PinUpdatePayload>>,
) -> Result<(), Error> {
    match payload.inner() {
        Some(payload) => {
            led_manager
                .0
                .write()
                .await
                .set_pin_mode(payload.pin, payload.color)
                .await;
            Ok(())
        }
        None => Err(Error::new(
            ErrorKind::InvalidInput,
            "Invalid Led Color Mode",
        )),
    }
}

#[delete("/led/pin", output = "json", eoutput = "bytes")]
pub async fn clear_pin_modes(led_manager: State<RwLock<LedManager>>) -> Result<(), Error> {
    led_manager.0.write().await.clear().await
}

#[post("/led/color", output = "json", eoutput = "bytes")]
pub async fn set_color_mode(
    led_manager: State<RwLock<LedManager>>,
    payload: Json<Option<LedColorMode>>,
) -> Result<(), Error> {
    match payload.inner() {
        Some(payload) => {
            led_manager.0.write().await.set_color_mode(payload).await;
            Ok(())
        }
        None => Err(Error::new(
            ErrorKind::InvalidInput,
            "Invalid Led Color Mode",
        )),
    }
}
