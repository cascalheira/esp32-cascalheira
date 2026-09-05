//! Weekly schedule model: per channel, a list of blocks of local minutes-since-midnight on a
//! set of weekdays. Mirrors the JSON pushed from Home Assistant and stored in NVS.

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize, Serializer};
use std::collections::BTreeMap;
use std::fmt;

use crate::CHANNELS;

pub const MINUTES_PER_DAY: u16 = 1440;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Weekday {
    Mon = 0,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

impl Weekday {
    /// From C `tm_wday` (0 = Sunday).
    pub fn from_tm_wday(wday: i32) -> Weekday {
        match wday.rem_euclid(7) {
            1 => Weekday::Mon,
            2 => Weekday::Tue,
            3 => Weekday::Wed,
            4 => Weekday::Thu,
            5 => Weekday::Fri,
            6 => Weekday::Sat,
            _ => Weekday::Sun,
        }
    }
}

/// Bit i set = active on weekday i (Mon = bit 0).
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct DayMask(pub u8);

impl DayMask {
    pub const ALL: DayMask = DayMask(0x7f);
    const LETTERS: [char; 7] = ['M', 'T', 'W', 'T', 'F', 'S', 'S'];

    pub fn contains(self, d: Weekday) -> bool {
        self.0 & (1 << d as u8) != 0
    }

    /// Parse the 7-character form used in the JSON, e.g. `"MTWTF--"`.
    /// Any non-`-` character in a position enables that day.
    pub fn parse(s: &str) -> Result<DayMask, String> {
        let chars: Vec<char> = s.chars().collect();
        if chars.len() != 7 {
            return Err(format!("days must be 7 characters, got {:?}", s));
        }
        let mut m = 0u8;
        for (i, c) in chars.iter().enumerate() {
            match c {
                '-' | '_' | '.' | ' ' => {}
                c if c.eq_ignore_ascii_case(&Self::LETTERS[i]) => m |= 1 << i,
                c => return Err(format!("unexpected {:?} at position {} in {:?}", c, i, s)),
            }
        }
        Ok(DayMask(m))
    }
}

impl fmt::Display for DayMask {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, l) in Self::LETTERS.iter().enumerate() {
            let c = if self.0 & (1 << i) != 0 { *l } else { '-' };
            write!(f, "{}", c)?;
        }
        Ok(())
    }
}

impl fmt::Debug for DayMask {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DayMask({})", self)
    }
}

impl Serialize for DayMask {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for DayMask {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        DayMask::parse(&s).map_err(de::Error::custom)
    }
}

/// One on-interval: `[from, to)` in local minutes since midnight, on the days in `days`.
/// `to` may be 1440 (end of day). Blocks never cross midnight; use two blocks instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Block {
    pub days: DayMask,
    pub from: u16,
    pub to: u16,
}

impl Block {
    pub fn validate(&self) -> Result<(), String> {
        if self.from >= self.to {
            return Err(format!("block from {} must be before to {}", self.from, self.to));
        }
        if self.to > MINUTES_PER_DAY {
            return Err(format!("block to {} exceeds {}", self.to, MINUTES_PER_DAY));
        }
        Ok(())
    }

    pub fn active(&self, t: LocalTime) -> bool {
        self.days.contains(t.weekday) && t.minute >= self.from && t.minute < self.to
    }
}

/// Local wall-clock position, as produced by the firmware from `localtime()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalTime {
    pub weekday: Weekday,
    /// Minutes since local midnight, 0..1440.
    pub minute: u16,
    /// Identifies the local calendar day (any value that changes exactly at local midnight,
    /// e.g. `year * 366 + day_of_year`). Used for daily counters; 0 if unknown.
    pub day: u32,
}

impl LocalTime {
    pub fn new(weekday: Weekday, hour: u16, min: u16) -> LocalTime {
        // Tests and simple callers: derive a day key from the weekday so consecutive weekdays
        // count as different days.
        LocalTime { weekday, minute: hour * 60 + min, day: weekday as u32 + 1 }
    }

    pub fn with_day(mut self, day: u32) -> LocalTime {
        self.day = day;
        self
    }
}

/// The whole weekly schedule as received from Home Assistant.
///
/// Wire format:
/// ```json
/// { "rev": 42, "tz": "WET0WEST,M3.5.0/1,M10.5.0",
///   "channels": { "1": [ {"days": "MTWTF--", "from": 360, "to": 420} ] } }
/// ```
/// Channel keys are 1-based in JSON; missing channels have no blocks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Schedule {
    #[serde(default)]
    pub rev: u32,
    /// POSIX TZ string applied on the device so DST is handled locally.
    #[serde(default)]
    pub tz: String,
    #[serde(default, with = "channel_map")]
    pub channels: [Vec<Block>; CHANNELS],
}

impl Schedule {
    pub fn from_json(s: &str) -> Result<Schedule, String> {
        let sched: Schedule = serde_json::from_str(s).map_err(|e| e.to_string())?;
        sched.validate()?;
        Ok(sched)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("schedule serializes")
    }

    pub fn validate(&self) -> Result<(), String> {
        for (i, blocks) in self.channels.iter().enumerate() {
            for b in blocks {
                b.validate().map_err(|e| format!("channel {}: {}", i + 1, e))?;
            }
        }
        Ok(())
    }

    pub fn block_count(&self) -> usize {
        self.channels.iter().map(Vec::len).sum()
    }

    /// Does the schedule want channel `ch` (0-based) on at local time `t`?
    pub fn wants_on(&self, ch: usize, t: LocalTime) -> bool {
        self.channels[ch].iter().any(|b| b.active(t))
    }

    pub fn wanted(&self, t: LocalTime) -> [bool; CHANNELS] {
        let mut out = [false; CHANNELS];
        for (ch, slot) in out.iter_mut().enumerate() {
            *slot = self.wants_on(ch, t);
        }
        out
    }

    pub fn state(&self, ok: bool) -> ScheduleState {
        ScheduleState { rev: self.rev, ok, blocks: self.block_count() }
    }
}

/// Published back to HA on `.../schedule/state` as an acknowledgement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleState {
    pub rev: u32,
    pub ok: bool,
    pub blocks: usize,
}

mod channel_map {
    use super::*;

    pub fn serialize<S: Serializer>(v: &[Vec<Block>; CHANNELS], s: S) -> Result<S::Ok, S::Error> {
        let map: BTreeMap<String, &Vec<Block>> = v
            .iter()
            .enumerate()
            .filter(|(_, b)| !b.is_empty())
            .map(|(i, b)| ((i + 1).to_string(), b))
            .collect();
        map.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[Vec<Block>; CHANNELS], D::Error> {
        let map: BTreeMap<String, Vec<Block>> = BTreeMap::deserialize(d)?;
        let mut out: [Vec<Block>; CHANNELS] = Default::default();
        for (k, v) in map {
            let n: usize = k
                .parse()
                .map_err(|_| de::Error::custom(format!("channel key {:?} is not a number", k)))?;
            if n == 0 || n > CHANNELS {
                return Err(de::Error::custom(format!("channel {} out of range 1..={}", n, CHANNELS)));
            }
            out[n - 1] = v;
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "rev": 42,
      "tz": "WET0WEST,M3.5.0/1,M10.5.0",
      "channels": {
        "1": [ {"days": "MTWTF--", "from": 360, "to": 420} ],
        "2": [ {"days": "-----SS", "from": 480, "to": 540},
               {"days": "MTWTFSS", "from": 1200, "to": 1260} ],
        "6": [ {"days": "MTWTFSS", "from": 0, "to": 1440} ]
      }
    }"#;

    #[test]
    fn parses_sample() {
        let s = Schedule::from_json(SAMPLE).unwrap();
        assert_eq!(s.rev, 42);
        assert_eq!(s.tz, "WET0WEST,M3.5.0/1,M10.5.0");
        assert_eq!(s.channels[0].len(), 1);
        assert_eq!(s.channels[1].len(), 2);
        assert!(s.channels[2].is_empty());
        assert_eq!(s.channels[5].len(), 1);
        assert_eq!(s.block_count(), 4);
        assert_eq!(s.channels[0][0].days, DayMask(0b0001_1111));
    }

    #[test]
    fn evaluates_blocks() {
        let s = Schedule::from_json(SAMPLE).unwrap();
        // ch1 weekdays 06:00-07:00
        assert!(s.wants_on(0, LocalTime::new(Weekday::Mon, 6, 0)));
        assert!(s.wants_on(0, LocalTime::new(Weekday::Fri, 6, 59)));
        assert!(!s.wants_on(0, LocalTime::new(Weekday::Fri, 7, 0)), "end is exclusive");
        assert!(!s.wants_on(0, LocalTime::new(Weekday::Sat, 6, 30)), "weekend excluded");
        // ch2 weekend mornings + every evening
        assert!(s.wants_on(1, LocalTime::new(Weekday::Sun, 8, 0)));
        assert!(!s.wants_on(1, LocalTime::new(Weekday::Mon, 8, 0)));
        assert!(s.wants_on(1, LocalTime::new(Weekday::Wed, 20, 30)));
        // ch3 has no blocks
        assert!(!s.wants_on(2, LocalTime::new(Weekday::Wed, 20, 30)));
        // ch6 always on, including the last minute of the day
        assert!(s.wants_on(5, LocalTime::new(Weekday::Sun, 23, 59)));
        assert_eq!(s.wanted(LocalTime::new(Weekday::Mon, 6, 30)), [true, false, false, false, false, true]);
    }

    #[test]
    fn round_trips_json() {
        let s = Schedule::from_json(SAMPLE).unwrap();
        let again = Schedule::from_json(&s.to_json()).unwrap();
        assert_eq!(s, again);
        assert!(s.to_json().contains("\"MTWTF--\""));
    }

    #[test]
    fn rejects_bad_input() {
        assert!(Schedule::from_json(r#"{"channels":{"7":[]}}"#).is_err(), "channel out of range");
        assert!(Schedule::from_json(r#"{"channels":{"0":[]}}"#).is_err());
        assert!(Schedule::from_json(r#"{"channels":{"1":[{"days":"MTWTF--","from":600,"to":600}]}}"#).is_err(), "empty block");
        assert!(Schedule::from_json(r#"{"channels":{"1":[{"days":"MTWTF--","from":600,"to":1441}]}}"#).is_err(), "past midnight");
        assert!(Schedule::from_json(r#"{"channels":{"1":[{"days":"MTWTF","from":0,"to":10}]}}"#).is_err(), "short days");
        assert!(Schedule::from_json(r#"{"channels":{"1":[{"days":"XTWTF--","from":0,"to":10}]}}"#).is_err(), "bad letter");
        assert!(Schedule::from_json("not json").is_err());
    }

    #[test]
    fn empty_schedule_is_valid() {
        let s = Schedule::from_json("{}").unwrap();
        assert_eq!(s.block_count(), 0);
        assert_eq!(s.wanted(LocalTime::new(Weekday::Mon, 12, 0)), [false; CHANNELS]);
        assert_eq!(s.to_json(), r#"{"rev":0,"tz":"","channels":{}}"#);
    }

    #[test]
    fn day_mask_parsing() {
        assert_eq!(DayMask::parse("MTWTFSS").unwrap(), DayMask::ALL);
        assert_eq!(DayMask::parse("-------").unwrap(), DayMask(0));
        assert_eq!(DayMask::parse("mtwtfss").unwrap(), DayMask::ALL, "case-insensitive");
        assert_eq!(DayMask::parse("M-----S").unwrap().to_string(), "M-----S");
        assert_eq!(Weekday::from_tm_wday(0), Weekday::Sun);
        assert_eq!(Weekday::from_tm_wday(1), Weekday::Mon);
        assert_eq!(Weekday::from_tm_wday(6), Weekday::Sat);
    }
}
