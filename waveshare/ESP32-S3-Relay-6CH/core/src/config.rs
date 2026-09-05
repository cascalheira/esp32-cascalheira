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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
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
    /// Interlock: turning any relay on switches all other relays off first, whatever the
    /// source (HA, schedule, local). Useful for a pump feeding several valves or for loads
    /// that must never run together.
    #[serde(default)]
    pub exclusive: bool,
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
        Config { mode: Mode::Auto, channels: [ChannelConfig::default(); CHANNELS], offline_grace_s: 0, exclusive: false, buzzer: true, rain_hold_until: 0 }
    }
}

impl Config {
    pub fn from_json(s: &str) -> Result<Config, String> {
        serde_json::from_str(s).map_err(|e| e.to_string())
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
        assert!(!partial.exclusive, "old configs without the field default to off");
        assert!(partial.buzzer, "buzzer defaults to on for old configs");
        assert_eq!(Mode::parse(" OFF "), Some(Mode::Off));
        assert_eq!(Mode::parse("nope"), None);
    }
}
