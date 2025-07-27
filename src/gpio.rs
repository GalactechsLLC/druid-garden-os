use gpiod::{Active, Bias, Chip, Input, Lines, Options, Output};
use libc::{sched_param, timespec, CLOCK_MONOTONIC, PR_SET_TIMERSLACK, SCHED_RR};
use log::{error, info};
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::io::{Error, ErrorKind};
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::sleep;
use std::time::Duration;
use tokio::task::JoinHandle;

pub async fn detect_gpio_chips() -> Result<Vec<Chip>, Error> {
    info!("Detecting Chips");
    let all_gpio_devices = Chip::list_devices()?;
    info!("Found {} Potential Chips", all_gpio_devices.len());
    let mut chips = vec![];
    for device in all_gpio_devices {
        let chip = Chip::new(device)?;
        chips.push(chip);
    }
    Ok(chips)
}

#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub enum PinMode {
    Output,
    Input,
}

pub enum Pin {
    Output(Lines<Output>),
    Input(Lines<Input>),
}

impl Pin {
    pub fn new(chips: &[Chip], offset: u32, pin_mode: PinMode) -> Result<Self, Error> {
        let mut cur_offset = offset;
        info!("Searching {} Total Chips. ", chips.len());
        for (index, chip) in chips.iter().enumerate() {
            let num_lines = chip.num_lines();
            info!("Chip {index} Has {num_lines} Lines ");
            if num_lines <= cur_offset {
                cur_offset -= num_lines;
                continue;
            }
            match pin_mode {
                PinMode::Output => {
                    let options = Options::output([cur_offset])
                        .values([false])
                        .active(Active::High)
                        .bias(Bias::Disable)
                        .consumer("pi-led");
                    let lines = chip.request_lines(options)?;
                    return Ok(Self::Output(lines));
                }
                PinMode::Input => {
                    let options = Options::input([cur_offset]);
                    let lines = chip.request_lines(options)?;
                    return Ok(Self::Input(lines));
                }
            }
        }
        Err(Error::other(format!("No pin at Offset {offset}!")))
    }

    pub fn get(&self) -> Result<Vec<bool>, Error> {
        match self {
            Pin::Input(input) => input.get_values(vec![false; input.lines().len()]),
            Pin::Output(output) => output.get_values(vec![false; output.lines().len()]),
        }
    }

    pub fn set(&self, value: bool) -> Result<(), Error> {
        match self {
            Pin::Output(output) => {
                let new_vals = vec![value; output.lines().len()];
                output.set_values(&new_vals)?;
                assert_eq!(self.get()?, new_vals);
                Ok(())
            }
            Pin::Input(_) => Err(Error::new(
                ErrorKind::InvalidInput,
                "Cannot set value on input pin",
            )),
        }
    }
}

#[derive(Default)]
pub struct PinSet {
    pins: HashMap<u32, PwmSignalHandler>,
}

impl PinSet {
    pub fn new() -> Self {
        Self {
            pins: HashMap::new(),
        }
    }

    pub async fn get_or_init(
        &mut self,
        chips: &[Chip],
        offset: u32,
    ) -> Result<&mut PwmSignalHandler, Error> {
        if let Entry::Vacant(e) = self.pins.entry(offset) {
            let pin = Pin::new(chips, offset, PinMode::Output)?;
            e.insert(PwmSignalHandler::new(
                pin,
                Duration::from_nanos(0),
                Duration::from_nanos(0),
            ));
        }
        Ok(self
            .pins
            .get_mut(&offset)
            .expect("Occupied Entry was None or failed to insert, Should not happen"))
    }

    pub fn get(&self, offset: u32) -> Option<&PwmSignalHandler> {
        self.pins.get(&offset)
    }

    pub async fn stop_all(self) -> Result<(), Error> {
        for pin in self.pins.into_values() {
            pin.stop();
            match pin.signal_thread.await {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => error!("Error stopping pin thread: {e:?}"),
                Err(e) => error!("Error joining pin thread: {e:?}"),
            }
        }
        Ok(())
    }

    pub fn take(&mut self, offset: u32) -> Option<PwmSignalHandler> {
        self.pins.remove(&offset)
    }

    pub fn set_handler(
        &mut self,
        offset: u32,
        handler: PwmSignalHandler,
    ) -> Option<PwmSignalHandler> {
        self.pins.insert(offset, handler)
    }

    pub fn pins(&mut self) -> &mut HashMap<u32, PwmSignalHandler> {
        &mut self.pins
    }
}

pub const DEFAULT_PWN_PERIOD_US: u64 = 20000;

pub struct PwmSignalSettings {
    period: Duration,
    pulse_width: Duration,
}

pub enum PwmSignal {
    Stop,
    Update(PwmSignalSettings),
}

pub struct PwmSignalHandler {
    pub signal_thread: JoinHandle<Result<(), Error>>,
    control_channel: Sender<PwmSignal>,
    pub pin: Arc<Pin>,
}

const SLEEP_THRESHOLD: i64 = 250_000;
const BUSYWAIT_MAX: i64 = 200_000;
const BUSYWAIT_REMAINDER: i64 = 100;

static THREAD_SYNC: AtomicU64 = AtomicU64::new(0);
static THREAD_WAITING: AtomicU64 = AtomicU64::new(0);

impl PwmSignalHandler {
    fn new(pin: Pin, period: Duration, pulse_width: Duration) -> PwmSignalHandler {
        let (sender, receiver) = std::sync::mpsc::channel();
        let inner_pin = Arc::new(pin);
        let thread_pin = inner_pin.clone();
        PwmSignalHandler {
            pin: inner_pin,
            signal_thread: tokio::task::spawn_blocking(move || {
                // Set the scheduling policy to real-time round robin at the highest priority. This
                // will silently fail if we're not running as root.
                unsafe {
                    let mut params = MaybeUninit::<sched_param>::zeroed().assume_init();
                    params.sched_priority = libc::sched_get_priority_max(SCHED_RR);
                    libc::sched_setscheduler(0, SCHED_RR, &params);
                    // Set timer slack to 1 ns (default = 50 µs). This is only relevant if we're unable
                    // to set a real-time scheduling policy.
                    libc::prctl(PR_SET_TIMERSLACK, 1);
                }

                let mut period_ns = period.as_nanos() as i64;
                let mut pulse_width_ns = pulse_width.as_nanos() as i64;
                let mut start_ns = get_time_ns();

                loop {
                    while THREAD_WAITING.load(Ordering::SeqCst) != 0 {
                        std::hint::spin_loop();
                    }
                    // Receive Updates
                    while let Ok(msg) = receiver.try_recv() {
                        match msg {
                            PwmSignal::Update(settings) => {
                                info!("Got Update signal");
                                // Reconfigure period and pulse width
                                pulse_width_ns = settings.pulse_width.as_nanos() as i64;
                                period_ns = settings.period.as_nanos() as i64;
                                if pulse_width_ns > period_ns {
                                    info!("Adjusting Pulse Width from {pulse_width_ns} to {period_ns}");
                                    pulse_width_ns = period_ns;
                                }
                                info!("Updating Settings to Period {period_ns}, Pulse Width {pulse_width_ns}");
                            }
                            PwmSignal::Stop => {
                                info!("Got Stop signal");
                                return Ok(());
                            }
                        }
                    }

                    if pulse_width_ns > 0 {
                        thread_pin.set(true)?;
                        if pulse_width_ns == period_ns {
                            sleep(Duration::from_millis(10));
                            continue;
                        }
                        THREAD_SYNC.fetch_add(1, Ordering::SeqCst);
                    } else {
                        thread_pin.set(false)?;
                        sleep(Duration::from_millis(10));
                        continue;
                    }

                    // Sleep if we have enough time remaining, while reserving some time
                    // for busy waiting to compensate for sleep taking longer than needed.
                    if pulse_width_ns >= SLEEP_THRESHOLD {
                        sleep(Duration::from_nanos(
                            pulse_width_ns.saturating_sub(BUSYWAIT_MAX) as u64,
                        ));
                    }

                    // Busy-wait for the remaining active time, minus BUSYWAIT_REMAINDER
                    // to account for get_time_ns() overhead
                    loop {
                        if pulse_width_ns.saturating_sub(get_time_ns().saturating_sub(start_ns))
                            <= BUSYWAIT_REMAINDER
                        {
                            break;
                        }
                        std::hint::spin_loop();
                    }

                    thread_pin.set(false)?;

                    let remaining_ns =
                        period_ns.saturating_sub(get_time_ns().saturating_sub(start_ns));

                    // Sleep if we have enough time remaining, while reserving some time
                    // for busy waiting to compensate for sleep taking longer than needed.
                    if remaining_ns >= SLEEP_THRESHOLD {
                        sleep(Duration::from_nanos(
                            remaining_ns.saturating_sub(BUSYWAIT_MAX) as u64,
                        ));
                    }

                    // Busy-wait for the remaining inactive time, minus BUSYWAIT_REMAINDER
                    // to account for get_time_ns() overhead
                    loop {
                        let current_ns = get_time_ns();
                        if period_ns.saturating_sub(current_ns.saturating_sub(start_ns))
                            <= BUSYWAIT_REMAINDER
                        {
                            start_ns = current_ns;
                            break;
                        }
                        std::hint::spin_loop();
                    }
                    THREAD_SYNC.fetch_sub(1, Ordering::SeqCst);
                    THREAD_WAITING.fetch_add(1, Ordering::SeqCst);
                    while THREAD_SYNC.load(Ordering::SeqCst) != 0 {
                        std::hint::spin_loop();
                    }
                    THREAD_WAITING.fetch_sub(1, Ordering::SeqCst);
                }
            }),
            control_channel: sender,
        }
    }

    pub fn set_pwm(&self, period: Duration, pulse_width: Duration) {
        if let Err(e) = self
            .control_channel
            .send(PwmSignal::Update(PwmSignalSettings {
                period,
                pulse_width,
            }))
        {
            error!("Error Setting Pin PWM: {e:?}");
        }
    }

    pub fn set_high(&self) {
        if let Err(e) = self
            .control_channel
            .send(PwmSignal::Update(PwmSignalSettings {
                period: Duration::from_micros(DEFAULT_PWN_PERIOD_US),
                pulse_width: Duration::from_micros(DEFAULT_PWN_PERIOD_US),
            }))
        {
            error!("Error Setting Pin High: {e:?}");
        }
    }

    pub fn set_low(&self) {
        if let Err(e) = self
            .control_channel
            .send(PwmSignal::Update(PwmSignalSettings {
                period: Duration::from_micros(DEFAULT_PWN_PERIOD_US),
                pulse_width: Duration::from_micros(0),
            }))
        {
            error!("Error Setting Pin Low: {e:?}");
        }
    }

    pub fn stop(&self) {
        if let Err(e) = self.control_channel.send(PwmSignal::Stop) {
            error!("Error Stopping Pin: {e:?}");
        }
    }
}

const NANOS_PER_SEC: i64 = 1_000_000_000;

// Standard Fast Clock for NS
#[inline(always)]
fn get_time_ns() -> i64 {
    let mut ts = timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe {
        libc::clock_gettime(CLOCK_MONOTONIC, &mut ts);
    }
    (ts.tv_sec * NANOS_PER_SEC) + ts.tv_nsec
}
