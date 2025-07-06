use crate::config::ConfigManager;
use crate::gpio::{detect_gpio_chips, PinSet};
use crate::models::config::AddConfigEntry;
use gpiod::Chip;
use log::{error, info, warn};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::io::{Error, ErrorKind};
use std::mem::replace;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

#[derive(Default, Debug)]
pub struct LedState {
    pub brightness: u8,
    pub mode: LedColorMode,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
pub enum PinColor {
    Red,
    Green,
    Blue,
}

#[derive(Debug, Deserialize, Serialize)]
pub enum LedColorMode {
    Pulse(LedColor, u64),
    Solid(LedColor),
}
impl Default for LedColorMode {
    fn default() -> Self {
        LedColorMode::Solid(LedColor { r: 255, g: 0, b: 0 })
    }
}

pub const DEFAULT_PWM_PERIOD_US: u64 = 20000;

pub struct LedManager {
    state: LedState,
    chips: Vec<Chip>,
    pub red_pins: PinSet,
    pub green_pins: PinSet,
    pub blue_pins: PinSet,
    config_manager: Arc<RwLock<ConfigManager>>,
    db: SqlitePool,
}
impl LedManager {
    pub async fn init(
        config_manager: Arc<RwLock<ConfigManager>>,
        db: SqlitePool,
    ) -> Result<Self, Error> {
        let mut chips = detect_gpio_chips().await?;
        chips.sort_by(|my, other| my.name().cmp(other.name()));
        let red_pins = match config_manager.read().await.get("led-red-pins").await {
            Some(pins) => {
                let mut pin_set = PinSet::default();
                for pin in pins.value.split(",") {
                    if let Ok(pin) = u32::from_str(pin) {
                        pin_set.get_or_init(&chips, pin).await?;
                    }
                }
                pin_set
            }
            None => Default::default(),
        };
        let green_pins = match config_manager.read().await.get("led-green-pins").await {
            Some(pins) => {
                let mut pin_set = PinSet::default();
                for pin in pins.value.split(",") {
                    if let Ok(pin) = u32::from_str(pin) {
                        pin_set.get_or_init(&chips, pin).await?;
                    }
                }
                pin_set
            }
            None => Default::default(),
        };
        let blue_pins = match config_manager.read().await.get("led-blue-pins").await {
            Some(pins) => {
                let mut pin_set = PinSet::default();
                for pin in pins.value.split(",") {
                    if let Ok(pin) = u32::from_str(pin) {
                        pin_set.get_or_init(&chips, pin).await?;
                    }
                }
                pin_set
            }
            None => Default::default(),
        };
        let mut slf = Self {
            state: LedState {
                brightness: 255,
                mode: LedColorMode::Solid(LedColor::WHITE),
            },
            red_pins,
            green_pins,
            blue_pins,
            chips,
            config_manager,
            db,
        };
        slf.sync_state().await;
        Ok(slf)
    }
    pub async fn set_color_mode(&mut self, mode: LedColorMode) {
        self.state.mode = mode;
        self.sync_state().await;
    }
    pub async fn set_brightness(&mut self, brightness: u8) {
        self.state.brightness = brightness;
        self.sync_state().await;
    }
    pub fn get_brightness(&self) -> u8 {
        self.state.brightness
    }
    async fn sync_state(&mut self) {
        let (color, period_duration) = match &self.state.mode {
            LedColorMode::Pulse(color, period) => (color, Duration::from_micros(*period)),
            LedColorMode::Solid(color) => (color, Duration::from_micros(DEFAULT_PWM_PERIOD_US)),
        };
        let red_duty = Duration::from_micros(get_duty(color.r, self.state.brightness));
        let green_duty = Duration::from_micros(get_duty(color.g, self.state.brightness));
        let blue_duty = Duration::from_micros(get_duty(color.b, self.state.brightness));
        for (pin, signal_handle) in self.red_pins.pins().iter_mut() {
            if signal_handle.signal_thread.is_finished() {
                warn!("Signal Thread is Finished for Pin: {}", *pin);
                let dummy_thread = tokio::task::spawn_blocking(|| Ok(()));
                let old_value = replace(&mut signal_handle.signal_thread, dummy_thread);
                match old_value.await {
                    Ok(Ok(_)) => {}
                    Ok(Err(e)) => {
                        error!("Error in Signal Thread: {e:?}");
                    }
                    Err(e) => {
                        error!("Join Error for Signal Handle: {e:?}");
                    }
                }
            }
            signal_handle.set_pwm(period_duration, red_duty);
        }
        for (pin, signal_handle) in self.green_pins.pins().iter_mut() {
            if signal_handle.signal_thread.is_finished() {
                warn!("Signal Thread is Finished for Pin: {}", *pin);
                let dummy_thread = tokio::task::spawn_blocking(|| Ok(()));
                let old_value = replace(&mut signal_handle.signal_thread, dummy_thread);
                match old_value.await {
                    Ok(Ok(_)) => {}
                    Ok(Err(e)) => {
                        error!("Error in Signal Thread: {e:?}");
                    }
                    Err(e) => {
                        error!("Join Error for Signal Handle: {e:?}");
                    }
                }
            }
            signal_handle.set_pwm(period_duration, green_duty);
        }
        for (pin, signal_handle) in self.blue_pins.pins().iter_mut() {
            if signal_handle.signal_thread.is_finished() {
                warn!("Signal Thread is Finished for Pin: {}", *pin);
                let dummy_thread = tokio::task::spawn_blocking(|| Ok(()));
                let old_value = replace(&mut signal_handle.signal_thread, dummy_thread);
                match old_value.await {
                    Ok(Ok(_)) => {}
                    Ok(Err(e)) => {
                        error!("Error in Signal Thread: {e:?}");
                    }
                    Err(e) => {
                        error!("Join Error for Signal Handle: {e:?}");
                    }
                }
            }
            signal_handle.set_pwm(period_duration, blue_duty);
        }
    }
    pub async fn set_pin_mode(&mut self, pin: u32, mode: PinColor) {
        let existing_value = self
            .red_pins
            .take(pin)
            .map(|p| (p, PinColor::Red))
            .or_else(|| self.green_pins.take(pin).map(|p| (p, PinColor::Green)))
            .or_else(|| self.blue_pins.take(pin).map(|p| (p, PinColor::Blue)));
        let old_mode = existing_value.as_ref().map(|v| v.1);
        info!(
            "Adding pin {pin} mode: {mode:?}, existed: {}",
            existing_value.is_some()
        );
        match mode {
            PinColor::Red => match existing_value {
                Some(v) => {
                    if let Some(v) = self.red_pins.set_handler(pin, v.0) {
                        v.stop();
                    }
                }
                None => {
                    if let Err(e) = self.red_pins.get_or_init(&self.chips, pin).await {
                        error!("Failed to init pin {pin}: {e:?}");
                    }
                }
            },
            PinColor::Green => match existing_value {
                Some(v) => {
                    if let Some(v) = self.green_pins.set_handler(pin, v.0) {
                        v.stop();
                    }
                }
                None => {
                    if let Err(e) = self.green_pins.get_or_init(&self.chips, pin).await {
                        error!("Failed to init pin {pin}: {e:?}");
                    }
                }
            },
            PinColor::Blue => match existing_value {
                Some(v) => {
                    if let Some(v) = self.blue_pins.set_handler(pin, v.0) {
                        v.stop();
                    }
                }
                None => {
                    if let Err(e) = self.blue_pins.get_or_init(&self.chips, pin).await {
                        error!("Failed to init pin {pin}: {e:?}");
                    }
                }
            },
        }
        self.update_config(pin, mode, old_mode).await;
        self.sync_state().await;
    }
    async fn update_config(&mut self, pin: u32, color: PinColor, old_color: Option<PinColor>) {
        //Remove from old config
        if let Some(old_color) = old_color {
            let config_key = match old_color {
                PinColor::Red => "led-red-pins",
                PinColor::Green => "led-green-pins",
                PinColor::Blue => "led-blue-pins",
            };
            let mut config_manager = self.config_manager.write().await;
            if let Some(entry) = config_manager.get(config_key).await {
                let mut pin_set = vec![];
                for pin_str in entry.value.split(",") {
                    if let Ok(pin_number) = u32::from_str(pin_str) {
                        if pin_number != pin {
                            pin_set.push(pin_str);
                        }
                    }
                }
                let new_value = pin_set.join(",");
                if let Err(e) = config_manager
                    .set(
                        config_key,
                        AddConfigEntry {
                            key: entry.key,
                            value: new_value,
                            last_value: entry.value,
                            category: entry.category,
                            system: entry.system,
                        },
                        Some(&self.db),
                    )
                    .await
                {
                    error!("Failed to set pin {pin} in DB: {e:?}");
                }
            }
        }

        let config_key = match color {
            PinColor::Red => "led-red-pins",
            PinColor::Green => "led-green-pins",
            PinColor::Blue => "led-blue-pins",
        };
        let mut config_manager = self.config_manager.write().await;
        if let Some(entry) = config_manager.get(config_key).await {
            let mut pin_set = vec![];
            for pin_str in entry.value.split(",") {
                if let Ok(pin_number) = u32::from_str(pin_str) {
                    if pin_number != pin {
                        pin_set.push(pin_number);
                    }
                }
            }
            pin_set.push(pin);
            let new_value = pin_set
                .into_iter()
                .map(|v| v.to_string())
                .collect::<Vec<String>>()
                .join(",");
            if let Err(e) = config_manager
                .set(
                    config_key,
                    AddConfigEntry {
                        key: entry.key,
                        value: new_value,
                        last_value: entry.value,
                        category: entry.category,
                        system: entry.system,
                    },
                    Some(&self.db),
                )
                .await
            {
                error!("Failed to set pin {pin} in DB: {e:?}");
            }
        } else if let Err(e) = config_manager
            .set(
                config_key,
                AddConfigEntry {
                    key: config_key.to_string(),
                    value: format!("{pin}"),
                    last_value: String::new(),
                    category: "led-settings".to_string(),
                    system: 0,
                },
                Some(&self.db),
            )
            .await
        {
            error!("Failed to set pin {pin} in DB: {e:?}");
        }
    }
    pub async fn get_pin_value(&self, pin: u32) -> Result<u8, Error> {
        let existing_value = self
            .red_pins
            .get(pin)
            .or_else(|| self.green_pins.get(pin))
            .or_else(|| self.blue_pins.get(pin));
        match existing_value {
            Some(v) => v.pin.get().map(|v| v[0] as u8),
            None => Err(Error::new(
                ErrorKind::NotFound,
                format!("Pin {pin} not found"),
            )),
        }
    }
    pub async fn stop_all(&mut self) -> Result<(), Error> {
        for signal_handle in self.red_pins.pins().values_mut() {
            signal_handle.stop();
        }
        for signal_handle in self.green_pins.pins().values_mut() {
            signal_handle.stop();
        }
        for signal_handle in self.blue_pins.pins().values_mut() {
            signal_handle.stop();
        }
        Ok(())
    }
    pub async fn clear(&mut self) -> Result<(), Error> {
        self.stop_all().await?;
        self.red_pins.pins().clear();
        self.green_pins.pins().clear();
        self.blue_pins.pins().clear();
        self.config_manager
            .write()
            .await
            .delete("led-red-pins", &self.db)
            .await?;
        self.config_manager
            .write()
            .await
            .delete("led-green-pins", &self.db)
            .await?;
        self.config_manager
            .write()
            .await
            .delete("led-blue-pins", &self.db)
            .await?;
        Ok(())
    }
}
impl Drop for LedManager {
    fn drop(&mut self) {
        for signal_handle in self.red_pins.pins().values_mut() {
            signal_handle.stop();
        }
        for signal_handle in self.green_pins.pins().values_mut() {
            signal_handle.stop();
        }
        for signal_handle in self.blue_pins.pins().values_mut() {
            signal_handle.stop();
        }
    }
}

#[derive(Default, Debug, Deserialize, Serialize)]
pub struct LedColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}
impl LedColor {
    pub const OFF: Self = Self { r: 0, g: 0, b: 0 };
    pub const WHITE: Self = Self {
        r: 255,
        g: 255,
        b: 255,
    };
    pub const RED: Self = Self { r: 255, g: 0, b: 0 };
    pub const GREEN: Self = Self { r: 0, g: 255, b: 0 };
    pub const BLUE: Self = Self { r: 0, g: 0, b: 255 };
    pub const PURPLE: Self = Self {
        r: 128,
        g: 0,
        b: 128,
    };
    pub const YELLOW: Self = Self {
        r: 255,
        g: 180,
        b: 0,
    };
    pub const ORANGE: Self = Self {
        r: 255,
        g: 80,
        b: 0,
    };
}

pub fn get_duty(color_value: u8, intensity: u8) -> u64 {
    ((color_value as u64 * DEFAULT_PWM_PERIOD_US) as f32 / 255f32 * (intensity as f32 / 255f32))
        as u64
}
