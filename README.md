# esp32-cascalheira

Rust firmware for ESP32 boards that plug into Home Assistant as native **ESPHome** devices,
without running ESPHome. Each board lives in its own directory under the vendor name.

| Board | Directory | Status |
|---|---|---|
| Waveshare ESP32-S3-Relay-6CH (also sold by SpotPear) | [`waveshare/ESP32-S3-Relay-6CH`](waveshare/ESP32-S3-Relay-6CH) · [user manual](waveshare/ESP32-S3-Relay-6CH/MANUAL.md) | In use: 6-relay irrigation controller with offline schedules |

## Why Rust instead of ESPHome YAML

The boards need logic that is awkward in YAML: a weekly schedule stored in flash that keeps
running when Home Assistant is unreachable, per-relay safety timers, an interlock, a captive
setup portal with factory reset, and OTA with rollback. The firmware is plain Rust on ESP-IDF
(`esp-idf-svc`), and a small reusable crate speaks the ESPHome native API so Home Assistant
discovers and controls the board exactly like any ESPHome node: encrypted (Noise), with
switches, numbers, selects, sensors, buttons and device actions.

## Repository layout

```
waveshare/ESP32-S3-Relay-6CH/
  core/       relay-core: hardware-independent logic (schedule, safeguard, arbiter), host-tested
  esphome/    esphome-api: ESPHome native API server (framing, Noise, protobuf, entities),
              transport-agnostic, tested on the host against the official aioesphomeapi client
  firmware/   relay-fw: the ESP32-S3 binary (WiFi, mDNS, API server, portal, OTA, NVS)
  ha/         Home Assistant side: notes and a fallback blueprint
  hw/         pinout and partition notes
  PLAN.md     design, decisions and phase log
```

The Home Assistant scheduling UI is the separate HACS integration
[garden-irrigation](https://github.com/cascalheira/garden-irrigation), which detects the
board's relays and pushes each setup's weekly plan to the device automatically.

## Working on the relay board

Prerequisites (macOS): `rustup`, then `cargo install espup ldproxy espflash` and
`espup install -t esp32s3`, plus `brew install cmake ninja dfu-util libusb ccache`.
Details, the `RUSTUP_TOOLCHAIN=esp` gotcha, flashing, OTA releases and the setup portal are
in [`firmware/README.md`](waveshare/ESP32-S3-Relay-6CH/firmware/README.md).

```sh
cd waveshare/ESP32-S3-Relay-6CH
(cd core && cargo test)                     # logic, on the host
(cd esphome && cargo test)                  # protocol, on the host
cd firmware && . ~/export-esp.sh && RUSTUP_TOOLCHAIN=esp cargo build --release
```

Credentials and the API key are read from a git-ignored `firmware/secrets.env`
(see `secrets.env.example`); on a board without them the firmware opens a setup access point.

## Features of the relay board firmware

- Appears in Home Assistant as an ESPHome device: 6 relay switches, per-relay max-on-time
  safeguards, exclusive interlock, mode (auto / manual / off), diagnostics, restart and all-off
  buttons, and actions to set or clear the stored schedule and to start an OTA update.
- Home Assistant drives the relays while connected; when it is gone the board follows the
  schedule stored in flash, with the clock kept by SNTP or Home Assistant.
- Captive setup portal (hold BOOT 5 s) for WiFi, names and API key; factory reset (hold 15 s)
  generates a fresh key and shows it only to clients on the setup network.
- OTA over HTTP with bootloader rollback, task watchdog, RGB status LED.
