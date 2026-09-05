//! Passive buzzer on GPIO21 driven by LEDC PWM. Tones are played on their own thread so the
//! main loop never blocks; a shared flag mutes everything when the user turns the buzzer off.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::Result;
use esp_idf_svc::hal::gpio::AnyOutputPin;
use esp_idf_svc::hal::ledc::{config::TimerConfig, LedcDriver, LedcTimerDriver, Resolution, CHANNEL0, TIMER0};
use esp_idf_svc::hal::units::Hertz;
use esp_idf_svc::sys;

#[derive(Debug, Clone, Copy)]
pub enum Tone {
    Boot,
    PortalOpen,
    PortalClose,
    Safeguard,
    FactoryReset,
    OtaDone,
    Error,
}

/// (frequency Hz, on ms, pause ms) steps.
fn sequence(t: Tone) -> &'static [(u32, u64, u64)] {
    match t {
        Tone::Boot => &[(1500, 60, 40), (2200, 80, 0)],
        Tone::PortalOpen => &[(1200, 80, 60), (1600, 80, 60), (2000, 120, 0)],
        Tone::PortalClose => &[(2000, 80, 60), (1600, 80, 60), (1200, 120, 0)],
        Tone::Safeguard => &[(2500, 120, 100), (2500, 120, 100), (2500, 120, 0)],
        Tone::FactoryReset => &[(900, 600, 0)],
        Tone::OtaDone => &[(1800, 80, 60), (1800, 80, 0)],
        Tone::Error => &[(600, 250, 0)],
    }
}

pub struct Buzzer {
    tx: Sender<Tone>,
    enabled: Arc<AtomicBool>,
}

impl Buzzer {
    pub fn spawn(timer: TIMER0<'static>, ledc_ch: CHANNEL0<'static>, pin: AnyOutputPin<'static>, enabled: bool) -> Result<Buzzer> {
        let (tx, rx) = channel::<Tone>();
        let flag = Arc::new(AtomicBool::new(enabled));
        let flag2 = flag.clone();
        thread::Builder::new().name("buzzer".into()).stack_size(3072).spawn(move || {
            let timer_drv = match LedcTimerDriver::new(timer, &TimerConfig::default().frequency(Hertz(2000)).resolution(Resolution::Bits10)) {
                Ok(t) => t,
                Err(e) => {
                    log::error!("buzzer timer: {e}");
                    return;
                }
            };
            let mut drv = match LedcDriver::new(ledc_ch, timer_drv, pin) {
                Ok(d) => d,
                Err(e) => {
                    log::error!("buzzer channel: {e}");
                    return;
                }
            };
            let _ = drv.set_duty(0);
            let half = drv.get_max_duty() / 2;
            for tone in rx {
                if !flag2.load(Ordering::Relaxed) {
                    continue;
                }
                for &(freq, on_ms, pause_ms) in sequence(tone) {
                    // Retune timer 0 directly; the driver owns the timer handle.
                    unsafe {
                        sys::ledc_set_freq(sys::ledc_mode_t_LEDC_LOW_SPEED_MODE, sys::ledc_timer_t_LEDC_TIMER_0, freq);
                    }
                    let _ = drv.set_duty(half);
                    thread::sleep(Duration::from_millis(on_ms));
                    let _ = drv.set_duty(0);
                    if pause_ms > 0 {
                        thread::sleep(Duration::from_millis(pause_ms));
                    }
                }
            }
        })?;
        Ok(Buzzer { tx, enabled: flag })
    }

    pub fn play(&self, t: Tone) {
        let _ = self.tx.send(t);
    }

    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
    }
}
