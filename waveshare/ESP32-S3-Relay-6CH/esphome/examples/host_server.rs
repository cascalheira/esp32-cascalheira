//! Fake 6-relay device on 0.0.0.0:6053 for testing with aioesphomeapi / HA.
//! Set `NOISE_PSK=<base64 32 bytes>` to require encryption, otherwise plaintext.

use std::net::TcpListener;
use std::sync::mpsc::channel;
use std::thread;

use esphome_api::codec::Codec;
use esphome_api::proto::{NumberMode, ServiceArgType};
use esphome_api::{runner, Command, Device, Meta, Registry, Server, State};

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let device = Device {
        name: "relay6-host".into(),
        friendly_name: "Relay6 host test".into(),
        mac_address: "02:00:00:00:00:01".into(),
        model: "host".into(),
        manufacturer: "cascalheira".into(),
        esphome_version: "2026.9.0".into(),
        compilation_time: "".into(),
        project_name: "cascalheira.relay6".into(),
        project_version: "0.1.0".into(),
        suggested_area: "".into(),
        encryption: std::env::var("NOISE_PSK").is_ok(),
    };
    let psk = std::env::var("NOISE_PSK").ok().map(|k| esphome_api::noise::parse_psk(&k).expect("NOISE_PSK"));
    let mut reg = Registry::new();
    let mut relays = Vec::new();
    let mut max_on = Vec::new();
    for i in 1..=6 {
        relays.push(reg.switch(Meta::new(&format!("relay_{i}"), &format!("Relay {i}")).icon("mdi:electric-switch")));
        max_on.push(reg.number(
            Meta::new(&format!("max_on_{i}"), &format!("Relay {i} max on time")).config(),
            0.0, 1440.0, 1.0, "min", NumberMode::Box,
        ));
    }
    let mode = reg.select(Meta::new("mode", "Mode").config(), &["auto", "manual", "off"]);
    let link = reg.text_sensor(Meta::new("link", "Link").diagnostic());
    let all_off = reg.button(Meta::new("all_off", "All off"));
    let set_schedule = reg.service("set_schedule", &[("json", ServiceArgType::String)]);

    let server = Server::new(device, reg, "relay-fw host example");
    for k in &relays {
        server.set_state(*k, State::Bool(false));
    }
    for k in &max_on {
        server.set_state(*k, State::Float(0.0));
    }
    server.set_state(mode, State::Text("auto".into()));
    server.set_state(link, State::Text("online".into()));

    let (cmd_tx, cmd_rx) = channel::<Command>();
    let app = server.clone();
    thread::spawn(move || {
        for cmd in cmd_rx {
            log::info!("command: {:?}", cmd);
            match cmd {
                Command::Switch { key, on } => app.set_state(key, State::Bool(on)),
                Command::Number { key, value } => app.set_state(key, State::Float(value)),
                Command::Select { key, option } => app.set_state(key, State::Text(option)),
                Command::Button { key } if key == all_off => {
                    for k in &relays {
                        app.set_state(*k, State::Bool(false));
                    }
                }
                Command::Service { key, args } if key == set_schedule => {
                    let json = args.first().map(|a| a.string.clone()).unwrap_or_default();
                    log::info!("set_schedule json ({} bytes): {}", json.len(), json);
                    app.set_state(link, State::Text(format!("schedule {} bytes", json.len())));
                }
                Command::Time { epoch_seconds, timezone } => log::info!("time from HA: {epoch_seconds} tz={timezone}"),
                _ => {}
            }
        }
    });

    let listener = TcpListener::bind("0.0.0.0:6053").expect("bind 6053");
    log::info!("listening on 0.0.0.0:6053 ({})", if psk.is_some() { "noise" } else { "plaintext" });
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(e) => {
                log::warn!("accept: {e}");
                continue;
            }
        };
        let server = server.clone();
        let cmd_tx = cmd_tx.clone();
        let codec = match psk {
            Some(k) => Codec::Noise(Box::new(esphome_api::noise::NoiseCodec::new(
                k, &server.device().name, &server.device().mac_compact(),
            ))),
            None => Codec::Plaintext,
        };
        thread::spawn(move || {
            if let Err(e) = runner::serve(stream, &server, codec, &cmd_tx) {
                log::debug!("connection ended: {e}");
            }
        });
    }
}
