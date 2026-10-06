//! Optional DS3231 RTC on the Pico header (I2C0, SDA GPIO4, SCL GPIO5), e.g. Waveshare
//! Pico-RTC-DS3231. Read at boot when nothing else knows the time; written whenever Home
//! Assistant or SNTP provides it. Absent hardware is detected once and then ignored.

use anyhow::{anyhow, Result};
use esp_idf_svc::hal::delay::TickType;
use esp_idf_svc::hal::gpio::{InputPin, OutputPin};
use esp_idf_svc::hal::i2c::{config::Config, I2c, I2cDriver};
use esp_idf_svc::hal::units::Hertz;
use relay_core::rtc::{self as regs, DS3231_ADDR, REG_STATUS, REG_TIME, STATUS_OSF};

pub struct Rtc {
    i2c: I2cDriver<'static>,
}

fn timeout() -> u32 {
    TickType::new_millis(50).ticks()
}

impl Rtc {
    /// Returns `None` (and logs once) when no DS3231 answers on the bus.
    pub fn probe(
        i2c: impl I2c + 'static,
        sda: impl InputPin + OutputPin + 'static,
        scl: impl InputPin + OutputPin + 'static,
    ) -> Option<Rtc> {
        let cfg = Config::new().baudrate(Hertz(100_000)).sda_enable_pullup(true).scl_enable_pullup(true);
        let drv = match I2cDriver::new(i2c, sda, scl, &cfg) {
            Ok(d) => d,
            Err(e) => {
                log::warn!("rtc: i2c init failed: {e}");
                return None;
            }
        };
        let mut rtc = Rtc { i2c: drv };
        let mut st = [0u8; 1];
        match rtc.i2c.write_read(DS3231_ADDR, &[REG_STATUS], &mut st, timeout()) {
            Ok(()) => {
                log::info!("rtc: DS3231 detected (status 0x{:02x})", st[0]);
                Some(rtc)
            }
            Err(_) => {
                log::info!("rtc: none detected on I2C GPIO4/GPIO5");
                None
            }
        }
    }

    /// Current time (Unix seconds, UTC), or `None` if the chip lost power and has not been set.
    pub fn read(&mut self) -> Result<Option<u64>> {
        let mut st = [0u8; 1];
        self.i2c.write_read(DS3231_ADDR, &[REG_STATUS], &mut st, timeout()).map_err(|e| anyhow!("status: {e}"))?;
        if st[0] & STATUS_OSF != 0 {
            return Ok(None);
        }
        let mut t = [0u8; 7];
        self.i2c.write_read(DS3231_ADDR, &[REG_TIME], &mut t, timeout()).map_err(|e| anyhow!("time: {e}"))?;
        Ok(regs::decode(&t))
    }

    /// Set the time and clear the oscillator-stop flag.
    pub fn write(&mut self, epoch: u64) -> Result<()> {
        let r = regs::encode(epoch);
        let mut buf = [0u8; 8];
        buf[0] = REG_TIME;
        buf[1..].copy_from_slice(&r);
        self.i2c.write(DS3231_ADDR, &buf, timeout()).map_err(|e| anyhow!("write time: {e}"))?;
        let mut st = [0u8; 1];
        self.i2c.write_read(DS3231_ADDR, &[REG_STATUS], &mut st, timeout()).map_err(|e| anyhow!("status: {e}"))?;
        self.i2c.write(DS3231_ADDR, &[REG_STATUS, st[0] & !STATUS_OSF], timeout()).map_err(|e| anyhow!("clear osf: {e}"))?;
        Ok(())
    }
}
