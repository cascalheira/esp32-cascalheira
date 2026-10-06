//! Persisted device settings (stored as one JSON blob in NVS).

use serde::{Deserialize, Serialize};

use crate::CHANNELS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// HA drives the relays while online; the stored schedule drives them while offline.
    #[default]
    Auto,
    /// HA only. The schedule is never applied, even offline.
    Manual,
    /// Everything off; commands are ignored.
    Off,
}

impl Mode {
    pub const ALL: [Mode; 3] = [Mode::Auto, Mode::Manual, Mode::Off];

    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Auto => "auto",
            Mode::Manual => "manual",
            Mode::Off => "off",
        }
    }

    pub fn parse(s: &str) -> Option<Mode> {
        Mode::ALL.into_iter().find(|m| m.as_str().eq_ignore_ascii_case(s.trim()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelConfig {
    /// Maximum time a relay may stay on, in minutes. 0 disables the safeguard.
    #[serde(default)]
    pub max_on_min: u16,
    /// State to hold while offline with unknown time (cold boot, no network).
    #[serde(default)]
    pub safe_state: bool,
    /// Maximum total on-time per local day, in minutes. 0 disables the cap.
    #[serde(default)]
    pub max_daily_min: u16,
    /// Exclusive set: 0 = none, 1..=MAX_EXCLUSIVE_SETS = A, B, C. Within a set only one relay
    /// may be on; switching one on turns the others in the same set off. Relays in different
    /// sets (or none) never affect each other.
    #[serde(default)]
    pub exclusive_set: u8,
    /// Switch on at every boot (power cut, restart, update). Offline, a relay without schedule
    /// blocks is held in this state instead of being switched off.
    #[serde(default)]
    pub power_on: bool,
    /// Pre-0.17 membership flag, read only to migrate old configs (see `Config::from_json`).
    #[serde(default, rename = "exclusive_member", skip_serializing)]
    pub legacy_member: Option<bool>,
}

/// Number of exclusive sets offered (A, B, C): with six relays a fourth set could hold one
/// relay at most and would mean nothing.
pub const MAX_EXCLUSIVE_SETS: u8 = 3;

impl ChannelConfig {
    pub fn set_label(set: u8) -> &'static str {
        match set {
            1 => "A",
            2 => "B",
            3 => "C",
            _ => "none",
        }
    }

    pub fn parse_set(label: &str) -> Option<u8> {
        match label.trim().to_ascii_uppercase().as_str() {
            "NONE" | "-" | "" => Some(0),
            "A" => Some(1),
            "B" => Some(2),
            "C" => Some(3),
            _ => None,
        }
    }
}

impl Default for ChannelConfig {
    fn default() -> Self {
        ChannelConfig { max_on_min: 0, safe_state: false, max_daily_min: 0, exclusive_set: 0, power_on: false, legacy_member: None }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub mode: Mode,
    #[serde(default)]
    pub channels: [ChannelConfig; CHANNELS],
    /// Seconds without any message from HA before the link is considered down while MQTT is
    /// still connected. 0 disables the heartbeat check (MQTT connected == online).
    #[serde(default)]
    pub offline_grace_s: u32,
    /// Pre-0.17 global exclusive switch, read only to migrate old configs into set A.
    #[serde(default, rename = "exclusive", skip_serializing)]
    pub legacy_exclusive: bool,
    /// Audible feedback (boot, portal, safeguard trips, reset, OTA) on the on-board buzzer.
    #[serde(default = "default_true")]
    pub buzzer: bool,
    /// Local rain hold: the stored schedule is ignored until this Unix time (seconds). 0 = none.
    /// Only affects offline operation; Home Assistant keeps its own rain delay.
    #[serde(default)]
    pub rain_hold_until: u64,
}

fn default_true() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Config { mode: Mode::Auto, channels: [ChannelConfig::default(); CHANNELS], offline_grace_s: 0, legacy_exclusive: false, buzzer: true, rain_hold_until: 0 }
    }
}

impl Config {
    pub fn from_json(s: &str) -> Result<Config, String> {
        let mut c: Config = serde_json::from_str(s).map_err(|e| e.to_string())?;
        // Migrate the old single exclusive group: with the global switch on, every member
        // (all relays unless explicitly taken out) goes into set A. New configs never write
        // the legacy fields, so this runs once.
        if c.legacy_exclusive {
            for ch in c.channels.iter_mut() {
                if ch.exclusive_set == 0 && ch.legacy_member != Some(false) {
                    ch.exclusive_set = 1;
                }
            }
        }
        c.legacy_exclusive = false;
        for ch in c.channels.iter_mut() {
            ch.legacy_member = None;
            if ch.exclusive_set > MAX_EXCLUSIVE_SETS {
                ch.exclusive_set = 0;
            }
        }
        Ok(c)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("config serializes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_round_trip() {
        let c = Config::default();
        assert_eq!(c.mode, Mode::Auto);
        assert_eq!(c.channels[3].max_on_min, 0);
        let again = Config::from_json(&c.to_json()).unwrap();
        assert_eq!(c, again);
        // Partial JSON fills in defaults.
        let partial = Config::from_json(r#"{"mode":"manual"}"#).unwrap();
        assert_eq!(partial.mode, Mode::Manual);
        assert_eq!(partial.offline_grace_s, 0);
        assert!(partial.channels.iter().all(|c| c.exclusive_set == 0), "no sets by default");
        assert!(partial.buzzer, "buzzer defaults to on for old configs");
        // 0.14-0.15 config with exclusive on: every relay migrates into set A.
        let old = Config::from_json(r#"{"channels":[{"max_on_min":25},{},{},{},{},{}],"exclusive":true}"#).unwrap();
        assert!(old.channels.iter().all(|c| c.exclusive_set == 1));
        assert_eq!(old.channels[0].max_on_min, 25);
        // 0.16 config: members go to set A, explicit non-members to none.
        let v16 = Config::from_json(
            r#"{"channels":[{},{"exclusive_member":true},{"exclusive_member":false},{},{},{"exclusive_member":false}],"exclusive":true}"#,
        )
        .unwrap();
        assert_eq!(v16.channels.map(|c| c.exclusive_set), [1, 1, 0, 1, 1, 0]);
        // Exclusive off: nothing migrates.
        let off = Config::from_json(r#"{"channels":[{},{},{},{},{},{}],"exclusive":false}"#).unwrap();
        assert!(off.channels.iter().all(|c| c.exclusive_set == 0));
        // New format round-trips and never writes the legacy keys.
        let json = v16.to_json();
        assert!(!json.contains("exclusive_member") && !json.contains("\"exclusive\""), "{json}");
        assert_eq!(Config::from_json(&json).unwrap().channels.map(|c| c.exclusive_set), [1, 1, 0, 1, 1, 0]);
        assert_eq!(ChannelConfig::parse_set(" b "), Some(2));
        assert_eq!(ChannelConfig::set_label(3), "C");
        assert_eq!(Mode::parse(" OFF "), Some(Mode::Off));
        assert_eq!(Mode::parse("nope"), None);
    }
}
