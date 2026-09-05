//! Wall-clock handling: SNTP as backup, HA-provided time as primary, POSIX TZ for local time.

use std::ffi::CString;
use std::time::{SystemTime, UNIX_EPOCH};

use esp_idf_svc::sntp::EspSntp;
use esp_idf_svc::sys;
use relay_core::{LocalTime, Weekday};

/// Anything before 2024 means the clock was never set.
const MIN_VALID_EPOCH: u64 = 1_704_067_200;

pub struct Clock {
    _sntp: Option<EspSntp<'static>>,
    tz: String,
}

impl Clock {
    pub fn new(tz: Option<String>) -> Clock {
        let sntp = EspSntp::new_default().map_err(|e| log::warn!("sntp: {e}")).ok();
        let mut c = Clock { _sntp: sntp, tz: String::new() };
        if let Some(tz) = tz {
            c.set_tz(&tz);
        }
        c
    }

    /// Apply a POSIX TZ string (e.g. `WET0WEST,M3.5.0/1,M10.5.0`). Returns true if it changed.
    pub fn set_tz(&mut self, tz: &str) -> bool {
        if tz.is_empty() || tz == self.tz {
            return false;
        }
        if let (Ok(k), Ok(v)) = (CString::new("TZ"), CString::new(tz)) {
            unsafe {
                sys::setenv(k.as_ptr(), v.as_ptr(), 1);
                sys::tzset();
            }
        }
        self.tz = tz.to_string();
        log::info!("timezone set to {tz}");
        true
    }

    pub fn set_epoch(&self, epoch_seconds: u32) {
        let tv = sys::timeval { tv_sec: epoch_seconds as _, tv_usec: 0 };
        unsafe {
            sys::settimeofday(&tv, std::ptr::null());
        }
        log::info!("clock set from Home Assistant: epoch {epoch_seconds}");
    }

    pub fn epoch(&self) -> Option<u64> {
        let e = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
        (e >= MIN_VALID_EPOCH).then_some(e)
    }

    pub fn time_known(&self) -> bool {
        self.epoch().is_some()
    }

    pub fn local_time(&self) -> Option<LocalTime> {
        let epoch = self.epoch()? as sys::time_t;
        let mut tm: sys::tm = unsafe { std::mem::zeroed() };
        let ok = unsafe { !sys::localtime_r(&epoch, &mut tm).is_null() };
        ok.then(|| LocalTime::new(Weekday::from_tm_wday(tm.tm_wday), tm.tm_hour as u16, tm.tm_min as u16))
    }

    pub fn local_string(&self) -> String {
        match self.local_time() {
            Some(t) => format!("{:?} {:02}:{:02}", t.weekday, t.minute / 60, t.minute % 60),
            None => "unknown".into(),
        }
    }
}
