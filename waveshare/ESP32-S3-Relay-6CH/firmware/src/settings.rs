//! Persistent settings in NVS: network/API credentials, controller config, schedule, timezone.

use anyhow::{Context, Result};
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};
use relay_core::{Config, Schedule, Usage};

const NS_NET: &str = "net";
const NS_RELAY: &str = "relay";

#[derive(Debug, Clone)]
pub struct NetSettings {
    pub ssid: String,
    pub pass: String,
    /// Base64 ESPHome API encryption key; empty = plaintext API.
    pub psk_b64: String,
    /// Device name prefix; the final node name is `<name>-<mac6>`.
    pub name: String,
    pub friendly_name: String,
}

pub struct Store {
    net: EspNvs<NvsDefault>,
    relay: EspNvs<NvsDefault>,
}

impl Store {
    pub fn open(part: EspDefaultNvsPartition) -> Result<Store> {
        Ok(Store {
            net: EspNvs::new(part.clone(), NS_NET, true).context("nvs net")?,
            relay: EspNvs::new(part, NS_RELAY, true).context("nvs relay")?,
        })
    }

    fn get_str(nvs: &EspNvs<NvsDefault>, key: &str) -> Option<String> {
        let mut buf = [0u8; 256];
        nvs.get_str(key, &mut buf).ok().flatten().map(str::to_string)
    }

    pub fn load_net(&self) -> Option<NetSettings> {
        let ssid = Self::get_str(&self.net, "ssid")?;
        Some(NetSettings {
            ssid,
            pass: Self::get_str(&self.net, "pass").unwrap_or_default(),
            psk_b64: Self::get_str(&self.net, "psk").unwrap_or_default(),
            name: Self::get_str(&self.net, "name").unwrap_or_else(|| "relay6".into()),
            friendly_name: Self::get_str(&self.net, "friendly").unwrap_or_else(|| "Relay board".into()),
        })
    }

    /// True when the compiled-in secrets differ from the ones last seeded into NVS.
    pub fn seed_changed(&self) -> bool {
        match option_env!("SEED_HASH") {
            Some(h) => Self::get_str(&self.net, "seedhash").as_deref() != Some(h),
            None => false,
        }
    }

    pub fn mark_seeded(&self) -> Result<()> {
        if let Some(h) = option_env!("SEED_HASH") {
            self.net.set_str("seedhash", h)?;
        }
        Ok(())
    }

    pub fn save_net(&self, s: &NetSettings) -> Result<()> {
        self.net.set_str("ssid", &s.ssid)?;
        self.net.set_str("pass", &s.pass)?;
        self.net.set_str("psk", &s.psk_b64)?;
        self.net.set_str("name", &s.name)?;
        self.net.set_str("friendly", &s.friendly_name)?;
        Ok(())
    }

    /// Credentials compiled in from `secrets.env`, used only when NVS is empty.
    pub fn seed_net() -> Option<NetSettings> {
        let ssid = option_env!("SEED_WIFI_SSID")?;
        Some(NetSettings {
            ssid: ssid.into(),
            pass: option_env!("SEED_WIFI_PASS").unwrap_or("").into(),
            psk_b64: option_env!("SEED_NOISE_PSK").unwrap_or("").into(),
            name: option_env!("SEED_DEVICE_NAME").unwrap_or("relay6").into(),
            friendly_name: option_env!("SEED_FRIENDLY_NAME").unwrap_or("Relay board").into(),
        })
    }

    /// Wipe everything a new owner should not inherit: network credentials, API key, names,
    /// controller config, schedule and timezone. The build-time seed is marked as applied so
    /// it does not silently re-provision the device after the reset.
    pub fn factory_reset(&self) -> Result<()> {
        for key in ["ssid", "pass", "psk", "name", "friendly"] {
            let _ = self.net.remove(key);
        }
        for key in ["cfg", "sched", "tz"] {
            let _ = self.relay.remove(key);
        }
        self.mark_seeded()
    }

    pub fn load_config(&self) -> Config {
        Self::get_str(&self.relay, "cfg")
            .and_then(|s| Config::from_json(&s).map_err(|e| log::warn!("bad cfg in nvs: {e}")).ok())
            .unwrap_or_default()
    }

    pub fn save_config(&self, c: &Config) -> Result<()> {
        self.relay.set_str("cfg", &c.to_json())?;
        Ok(())
    }

    pub fn load_schedule(&self) -> Schedule {
        let mut buf = vec![0u8; 16 * 1024];
        match self.relay.get_blob("sched", &mut buf) {
            Ok(Some(bytes)) => match std::str::from_utf8(bytes).ok().and_then(|s| Schedule::from_json(s).ok()) {
                Some(s) => s,
                None => {
                    log::warn!("bad schedule in nvs, ignoring");
                    Schedule::default()
                }
            },
            _ => Schedule::default(),
        }
    }

    pub fn save_schedule(&self, s: &Schedule) -> Result<()> {
        self.relay.set_blob("sched", s.to_json().as_bytes())?;
        Ok(())
    }

    pub fn load_usage(&self) -> Option<Usage> {
        let mut buf = [0u8; 512];
        let json = self.relay.get_str("usage", &mut buf).ok().flatten()?;
        serde_json::from_str(json).map_err(|e| log::warn!("bad usage in nvs: {e}")).ok()
    }

    pub fn save_usage(&self, u: &Usage) -> Result<()> {
        self.relay.set_str("usage", &serde_json::to_string(u)?)?;
        Ok(())
    }

    pub fn load_tz(&self) -> Option<String> {
        Self::get_str(&self.relay, "tz").filter(|s| !s.is_empty())
    }

    pub fn save_tz(&self, tz: &str) -> Result<()> {
        self.relay.set_str("tz", tz)?;
        Ok(())
    }
}
