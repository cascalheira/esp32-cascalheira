//! DS3231 real-time clock register encoding, independent of the I2C transport.
//!
//! The RTC keeps UTC. Registers 0x00..=0x06 hold seconds, minutes, hours, weekday, date,
//! month (bit 7 = century) and year (00-99), all BCD. Bit 7 of the status register (0x0F) is the
//! oscillator-stop flag: when set, the time is not trustworthy (battery missing or flat).

pub const DS3231_ADDR: u8 = 0x68;
pub const REG_TIME: u8 = 0x00;
pub const REG_STATUS: u8 = 0x0F;
pub const STATUS_OSF: u8 = 0x80;

/// Earliest time accepted as valid (2024-01-01), same threshold as the firmware clock.
pub const MIN_VALID_EPOCH: u64 = 1_704_067_200;

fn bcd(v: u8) -> u8 {
    ((v / 10) << 4) | (v % 10)
}

fn unbcd(b: u8) -> Option<u8> {
    let (hi, lo) = (b >> 4, b & 0x0f);
    (hi <= 9 && lo <= 9).then_some(hi * 10 + lo)
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Encode a Unix time (UTC) into the seven time registers. Valid for 2000-2099.
pub fn encode(epoch: u64) -> [u8; 7] {
    let days = (epoch / 86_400) as i64;
    let secs = epoch % 86_400;
    let (y, m, d) = civil_from_days(days);
    // 1970-01-01 was a Thursday; DS3231 weekday is user-defined 1..=7, we use Mon = 1.
    let weekday = ((days + 3).rem_euclid(7) + 1) as u8;
    [
        bcd((secs % 60) as u8),
        bcd(((secs / 60) % 60) as u8),
        bcd((secs / 3600) as u8), // bit 6 clear = 24-hour mode
        weekday,
        bcd(d as u8),
        bcd(m as u8) | if y >= 2100 { 0x80 } else { 0 },
        bcd((y % 100) as u8),
    ]
}

/// Decode the seven time registers into a Unix time (UTC). `None` for impossible values or a
/// time before [`MIN_VALID_EPOCH`] (a fresh or battery-less chip reads 2000-01-01).
pub fn decode(r: &[u8; 7]) -> Option<u64> {
    let sec = unbcd(r[0] & 0x7f)?;
    let min = unbcd(r[1] & 0x7f)?;
    let hour = if r[2] & 0x40 != 0 {
        // 12-hour mode: bit 5 = PM.
        let h12 = unbcd(r[2] & 0x1f)?;
        if !(1..=12).contains(&h12) {
            return None;
        }
        (h12 % 12) + if r[2] & 0x20 != 0 { 12 } else { 0 }
    } else {
        unbcd(r[2] & 0x3f)?
    };
    let day = unbcd(r[4] & 0x3f)?;
    let month = unbcd(r[5] & 0x1f)?;
    let year = 2000 + unbcd(r[6])? as i64 + if r[5] & 0x80 != 0 { 100 } else { 0 };
    if sec > 59 || min > 59 || hour > 23 || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let days = days_from_civil(year, month as u32, day as u32);
    // Reject dates like 31 February that the arithmetic would silently roll over.
    if civil_from_days(days) != (year, month as u32, day as u32) {
        return None;
    }
    let epoch = days * 86_400 + hour as i64 * 3600 + min as i64 * 60 + sec as i64;
    (epoch >= MIN_VALID_EPOCH as i64).then_some(epoch as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Reference values from Python's calendar.timegm.
    const CASES: [(u64, [u8; 7]); 3] = [
        (1_791_290_096, [0x56, 0x34, 0x12, 2, 0x06, 0x10, 0x26]), // 2026-10-06 12:34:56 Tue
        (1_835_417_228, [0x08, 0x07, 0x06, 2, 0x29, 0x02, 0x28]), // 2028-02-29 06:07:08 Tue
        (4_102_444_799, [0x59, 0x59, 0x23, 4, 0x31, 0x12, 0x99]), // 2099-12-31 23:59:59 Thu
    ];

    #[test]
    fn encode_matches_reference_dates() {
        for (epoch, regs) in CASES {
            assert_eq!(encode(epoch), regs, "epoch {epoch}");
            assert_eq!(decode(&regs), Some(epoch), "regs {regs:02x?}");
        }
    }

    #[test]
    fn round_trips_every_hour_for_years() {
        let mut e = MIN_VALID_EPOCH;
        while e < MIN_VALID_EPOCH + 6 * 365 * 86_400 {
            assert_eq!(decode(&encode(e)), Some(e), "epoch {e}");
            e += 3_607; // odd step to hit every second/minute value over time
        }
    }

    #[test]
    fn rejects_invalid_and_unset_clocks() {
        assert_eq!(decode(&[0x00, 0x00, 0x00, 1, 0x01, 0x01, 0x00]), None, "power-on default 2000-01-01");
        assert_eq!(decode(&[0x00, 0x00, 0x00, 1, 0x31, 0x02, 0x26]), None, "31 February");
        assert_eq!(decode(&[0x60, 0x00, 0x00, 1, 0x01, 0x01, 0x26]), None, "60 seconds");
        assert_eq!(decode(&[0x0a, 0x00, 0x00, 1, 0x01, 0x01, 0x26]), None, "not BCD");
        assert_eq!(decode(&[0x00, 0x00, 0x00, 1, 0x01, 0x13, 0x26]), None, "month 13");
    }

    #[test]
    fn decodes_12_hour_mode() {
        // 2026-10-06 12:34:56 written as 12:34:56 PM and 00:34:56 as 12:34:56 AM.
        assert_eq!(decode(&[0x56, 0x34, 0x40 | 0x20 | 0x12, 2, 0x06, 0x10, 0x26]), Some(1_791_290_096));
        assert_eq!(decode(&[0x56, 0x34, 0x40 | 0x12, 2, 0x06, 0x10, 0x26]), Some(1_791_290_096 - 12 * 3600));
    }
}
