use dg_sysfs::classes::gpio::{PinSet, DEFAULT_PWN_PERIOD_US};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

#[derive(Default, Debug)]
pub struct LedState {
    pub brightness: AtomicU8,
    pub mode: Arc<RwLock<LedColorMode>>,
}

#[derive(Debug, Deserialize, Serialize)]
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

pub struct LedManager {
    pub state: LedState,
    pub red_pin_set: PinSet,
    pub green_pin_set: PinSet,
    pub blue_pin_set: PinSet,
}
impl LedManager {
    pub async fn set_color_mode(&mut self, mode: LedColorMode) {
        let (color, period_duration) = match mode {
            LedColorMode::Pulse(color, period) => (color, Duration::from_micros(period)),
            LedColorMode::Solid(color) => (color, Duration::from_micros(DEFAULT_PWN_PERIOD_US)),
        };
        let red_duty = Duration::from_micros(get_duty(
            color.r,
            self.state.brightness.load(Ordering::Relaxed),
        ));
        let green_duty = Duration::from_micros(get_duty(
            color.g,
            self.state.brightness.load(Ordering::Relaxed),
        ));
        let blue_duty = Duration::from_micros(get_duty(
            color.b,
            self.state.brightness.load(Ordering::Relaxed),
        ));
        for single_handle in self.red_pin_set.pins().values_mut() {
            single_handle.set_pwm(period_duration, red_duty);
        }
        for single_handle in self.green_pin_set.pins().values_mut() {
            single_handle.set_pwm(period_duration, green_duty);
        }
        for single_handle in self.blue_pin_set.pins().values_mut() {
            single_handle.set_pwm(period_duration, blue_duty);
        }
    }
    pub async fn set_pin_mode(&mut self, pin: u8, mode: PinColor) {
        let existing_value = self
            .red_pin_set
            .take(pin)
            .or_else(|| self.green_pin_set.take(pin))
            .or_else(|| self.blue_pin_set.take(pin));
        match mode {
            PinColor::Red => match existing_value {
                Some(v) => {
                    if let Some(mut v) = self.red_pin_set.set_handler(pin, v) {
                        v.stop();
                    }
                }
                None => {
                    let _ = self.red_pin_set.get_or_init(pin).await;
                }
            },
            PinColor::Green => match existing_value {
                Some(v) => {
                    if let Some(mut v) = self.green_pin_set.set_handler(pin, v) {
                        v.stop();
                    }
                }
                None => {
                    let _ = self.green_pin_set.get_or_init(pin).await;
                }
            },
            PinColor::Blue => match existing_value {
                Some(v) => {
                    if let Some(mut v) = self.blue_pin_set.set_handler(pin, v) {
                        v.stop();
                    }
                }
                None => {
                    let _ = self.blue_pin_set.get_or_init(pin).await;
                }
            },
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
    ((color_value as u64 * DEFAULT_PWN_PERIOD_US) as f32 / 255f32 * (intensity as f32 / 255f32))
        as u64
}
