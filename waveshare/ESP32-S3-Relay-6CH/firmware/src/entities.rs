//! The entities this device exposes to Home Assistant, and their keys.

use esphome_api::proto::{NumberMode, SensorStateClass, ServiceArgType};
use esphome_api::{Meta, Registry};
use relay_core::{Mode, CHANNELS};

/// Labels shown in Home Assistant's Mode dropdown. The stored value is still `auto|manual|off`.
pub const MODE_LABELS: [(Mode, &str); 3] = [
    (Mode::Auto, "Auto: HA controls, stored schedule when HA is offline"),
    (Mode::Manual, "Manual: HA controls only, schedule never runs"),
    (Mode::Off, "Off: all relays off, commands ignored"),
];

pub fn mode_label(mode: Mode) -> &'static str {
    MODE_LABELS.iter().find(|(m, _)| *m == mode).map(|(_, l)| *l).unwrap_or("Auto")
}

/// Accepts a label or the bare value (`auto`, `manual`, `off`).
pub fn mode_from_option(option: &str) -> Option<Mode> {
    let o = option.trim();
    MODE_LABELS
        .iter()
        .find(|(_, l)| l.eq_ignore_ascii_case(o))
        .map(|(m, _)| *m)
        .or_else(|| Mode::parse(o))
        .or_else(|| MODE_LABELS.iter().find(|(m, _)| o.to_lowercase().starts_with(m.as_str())).map(|(m, _)| *m))
}

pub struct Keys {
    pub relay: [u32; CHANNELS],
    pub max_on: [u32; CHANNELS],
    pub tripped: [u32; CHANNELS],
    pub safe_state: [u32; CHANNELS],
    pub mode: u32,
    pub exclusive: u32,
    pub link: u32,
    pub local_time: u32,
    pub schedule_loaded: u32,
    pub schedule_rev: u32,
    pub schedule_info: u32,
    pub uptime: u32,
    pub rssi: u32,
    pub all_off: u32,
    pub restart: u32,
    pub set_schedule: u32,
    pub clear_schedule: u32,
    pub ota: u32,
    pub ota_status: u32,
    pub buzzer: u32,
    pub heap: u32,
    pub heap_min: u32,
    pub reset_reason: u32,
    pub rain_hold_hours: u32,
    pub rain_hold: u32,
}

impl Keys {
    pub fn relay_channel(&self, key: u32) -> Option<usize> {
        self.relay.iter().position(|k| *k == key)
    }
    pub fn max_on_channel(&self, key: u32) -> Option<usize> {
        self.max_on.iter().position(|k| *k == key)
    }
    pub fn safe_state_channel(&self, key: u32) -> Option<usize> {
        self.safe_state.iter().position(|k| *k == key)
    }
}

pub fn build() -> (Registry, Keys) {
    let mut r = Registry::new();
    let mut relay = [0u32; CHANNELS];
    let mut max_on = [0u32; CHANNELS];
    let mut tripped = [0u32; CHANNELS];
    let mut safe_state = [0u32; CHANNELS];
    for i in 0..CHANNELS {
        let n = i + 1;
        relay[i] = r.switch(Meta::new(&format!("relay_{n}"), &format!("Relay {n}")).icon("mdi:electric-switch"));
        max_on[i] = r.number(
            Meta::new(&format!("relay_{n}_max_on"), &format!("Relay {n} max on time")).icon("mdi:timer-off-outline").config(),
            0.0,
            1440.0,
            1.0,
            "min",
            NumberMode::Box,
        );
        tripped[i] = r.binary_sensor(
            Meta::new(&format!("relay_{n}_safeguard"), &format!("Relay {n} safeguard tripped"))
                .device_class("problem")
                .diagnostic(),
        );
        safe_state[i] = r.switch(
            Meta::new(&format!("relay_{n}_safe_state"), &format!("Relay {n} on when clock unknown"))
                .icon("mdi:shield-half-full")
                .config(),
        );
    }
    let keys = Keys {
        relay,
        max_on,
        tripped,
        safe_state,
        mode: r.select(
            Meta::new("mode", "Mode").icon("mdi:auto-mode").config(),
            &[MODE_LABELS[0].1, MODE_LABELS[1].1, MODE_LABELS[2].1],
        ),
        exclusive: r.switch(Meta::new("exclusive", "Exclusive mode").icon("mdi:swap-horizontal-bold").config()),
        link: r.text_sensor(Meta::new("link", "Link").icon("mdi:lan-connect").diagnostic()),
        local_time: r.text_sensor(Meta::new("local_time", "Device local time").icon("mdi:clock-outline").diagnostic()),
        schedule_loaded: r.binary_sensor(Meta::new("schedule_loaded", "Schedule loaded").icon("mdi:calendar-check").diagnostic()),
        schedule_rev: r.sensor(Meta::new("schedule_rev", "Schedule revision").icon("mdi:counter").diagnostic(), "", 0, SensorStateClass::StateClassNone),
        schedule_info: r.text_sensor(Meta::new("schedule_info", "Schedule").icon("mdi:calendar-clock").diagnostic()),
        uptime: r.sensor(
            Meta::new("uptime", "Uptime").device_class("duration").diagnostic(),
            "s",
            0,
            SensorStateClass::StateClassTotalIncreasing,
        ),
        rssi: r.sensor(
            Meta::new("rssi", "WiFi signal").device_class("signal_strength").diagnostic(),
            "dBm",
            0,
            SensorStateClass::StateClassMeasurement,
        ),
        all_off: r.button(Meta::new("all_off", "All relays off").icon("mdi:power-off")),
        restart: r.button(Meta::new("restart", "Restart").device_class("restart").config()),
        set_schedule: r.service("set_schedule", &[("json", ServiceArgType::String)]),
        clear_schedule: r.service("clear_schedule", &[]),
        ota: r.service("ota", &[("url", ServiceArgType::String)]),
        ota_status: r.text_sensor(Meta::new("ota_status", "Firmware update").icon("mdi:update").diagnostic()),
        buzzer: r.switch(Meta::new("buzzer", "Buzzer").icon("mdi:volume-high").config()),
        heap: r.sensor(
            Meta::new("heap", "Free heap").icon("mdi:memory").device_class("data_size").diagnostic(),
            "B",
            0,
            SensorStateClass::StateClassMeasurement,
        ),
        rain_hold_hours: r.number(
            Meta::new("rain_hold_hours", "Rain hold").icon("mdi:weather-pouring"),
            0.0,
            72.0,
            1.0,
            "h",
            NumberMode::Box,
        ),
        rain_hold: r.text_sensor(Meta::new("rain_hold", "Rain hold until").icon("mdi:weather-pouring")),
        reset_reason: r.text_sensor(Meta::new("reset_reason", "Last reset reason").icon("mdi:restart-alert").diagnostic()),
        heap_min: r.sensor(
            Meta::new("heap_min", "Lowest free heap").icon("mdi:memory").device_class("data_size").diagnostic(),
            "B",
            0,
            SensorStateClass::StateClassMeasurement,
        ),
    };
    (r, keys)
}
