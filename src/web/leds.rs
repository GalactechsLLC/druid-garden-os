use crate::plugins::led_manager::{LedColorMode, LedManager, PinColor};
use portfu::prelude::State;
use portfu_core::Json;
use portfu_macros::post;
use serde::{Deserialize, Serialize};
use std::io::{Error, ErrorKind};
use tokio::sync::RwLock;

#[derive(Debug, Deserialize, Serialize)]
pub struct PinUpdatePayload {
    pub pin: u8,
    pub color: PinColor,
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
