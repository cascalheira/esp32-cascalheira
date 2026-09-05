//! Static device identity, as reported in `DeviceInfoResponse` and advertised over mDNS.

use std::collections::BTreeMap;

use crate::proto;

pub const NOISE_CIPHER_NAME: &str = "Noise_NNpsk0_25519_ChaChaPoly_SHA256";

#[derive(Debug, Clone)]
pub struct Device {
    /// mDNS host name and ESPHome node name: lowercase, `[a-z0-9-]`, e.g. `relay6-47abb0`.
    pub name: String,
    pub friendly_name: String,
    /// `aa:bb:cc:dd:ee:ff`, lowercase. HA uses it as the unique id.
    pub mac_address: String,
    pub model: String,
    pub manufacturer: String,
    /// Reported as `esphome_version`. HA parses it as a version, so keep it `x.y.z`.
    pub esphome_version: String,
    pub compilation_time: String,
    pub project_name: String,
    pub project_version: String,
    pub suggested_area: String,
    /// Whether Noise encryption is available (a PSK is configured).
    pub encryption: bool,
}

impl Device {
    #[allow(deprecated)]
    pub fn device_info(&self) -> proto::DeviceInfoResponse {
        proto::DeviceInfoResponse {
            uses_password: false,
            name: self.name.clone(),
            // HA stores this as the unique id; ESPHome sends it uppercase with colons.
            mac_address: self.mac_address.to_uppercase(),
            esphome_version: self.esphome_version.clone(),
            compilation_time: self.compilation_time.clone(),
            model: self.model.clone(),
            has_deep_sleep: false,
            project_name: self.project_name.clone(),
            project_version: self.project_version.clone(),
            webserver_port: 0,
            manufacturer: self.manufacturer.clone(),
            friendly_name: self.friendly_name.clone(),
            suggested_area: self.suggested_area.clone(),
            api_encryption_supported: self.encryption,
            ..Default::default()
        }
    }

    /// TXT records for the `_esphomelib._tcp` mDNS service.
    pub fn mdns_txt(&self) -> BTreeMap<&'static str, String> {
        let mut t = BTreeMap::new();
        t.insert("friendly_name", self.friendly_name.clone());
        t.insert("version", self.esphome_version.clone());
        t.insert("mac", self.mac_compact());
        t.insert("platform", "ESP32".to_string());
        t.insert("board", self.model.clone());
        t.insert("network", "wifi".to_string());
        t.insert("project_name", self.project_name.clone());
        t.insert("project_version", self.project_version.clone());
        if self.encryption {
            t.insert("api_encryption", NOISE_CIPHER_NAME.to_string());
        }
        t
    }

    /// `mac` in the compact form HA expects in TXT records and unique ids.
    pub fn mac_compact(&self) -> String {
        self.mac_address.replace(':', "").to_lowercase()
    }
}

pub fn mac_from_bytes(mac: &[u8; 6]) -> String {
    mac.iter().map(|b| format!("{:02x}", b)).collect::<Vec<_>>().join(":")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_and_txt() {
        let mac = mac_from_bytes(&[0x10, 0x51, 0xdb, 0x47, 0xab, 0xb0]);
        assert_eq!(mac, "10:51:db:47:ab:b0");
        let d = Device {
            name: "relay6-47abb0".into(),
            friendly_name: "Relays".into(),
            mac_address: mac,
            model: "esp32-s3-relay-6ch".into(),
            manufacturer: "Waveshare".into(),
            esphome_version: "2026.9.0".into(),
            compilation_time: "".into(),
            project_name: "cascalheira.relay6".into(),
            project_version: "0.1.0".into(),
            suggested_area: "".into(),
            encryption: true,
        };
        let txt = d.mdns_txt();
        assert_eq!(txt["mac"], "1051db47abb0");
        assert_eq!(txt["api_encryption"], NOISE_CIPHER_NAME);
        assert!(d.device_info().api_encryption_supported);
        assert_eq!(d.device_info().mac_address, "10:51:DB:47:AB:B0");
    }
}
