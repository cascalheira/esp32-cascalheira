# ESP32-S3 6-channel relay controller in Rust, integrated with Home Assistant

Research date: 2026-09-05. Board: SpotPear "ESP32-S3-WROOM-1U-N8 WiFi RS485 Bluetooth Industrial 6-Channel Relay", which is the Waveshare **ESP32-S3-Relay-6CH** design (SpotPear's wiki mirrors Waveshare's, and the schematic PDF is titled Waveshare ESP32-S3-Relay-6CH).

---

## 1. Answers to the questions

### Q1. Can the device store the schedule and run it with WiFi down?

**Yes.** This unit has **16 MB flash** (N16 module, confirmed with `espflash board-info` on 2026-09-05; SpotPear's listing said N8). ESP-IDF's NVS partition holds key/value blobs of up to about 4 KB each, which is plenty for six weekly schedules plus per-channel settings; LittleFS is also available if you want files. Writes survive power loss and OTA updates.

**The real limitation is time, not storage.** The board has **no hardware RTC**. After a power cut with no WiFi the ESP32 does not know the wall-clock time, so it cannot run a time-of-day schedule until it syncs. Options, in order of preference:

1. Accept it: after a cold boot without network, hold the relays in a configured "safe" state (default OFF) until SNTP or HA supplies the time. With WiFi merely flaky (the common case), the ESP32 keeps time internally across the outage, so the schedule keeps running. Soft reboots also keep time in the RTC domain.
2. Add a DS3231 RTC on the Pico header (Waveshare's Pico-RTC-DS3231 HAT, I2C SDA=GPIO4, SCL=GPIO5, addr 0x68, ~3 EUR). Then the device runs the schedule immediately after a cold boot with no network. Recommended if the relays control something that must run (irrigation, heating).

### Q2. (question was cut off in the request; add it and I'll answer)

### Decision 2026-09-05: no RTC HAT for now. Cold boot without network holds the per-channel safe state until SNTP or HA supplies time.

### Q3. WiFi provisioning: can it come up as an access point with DHCP and a setup page?

**Yes.** ESP-IDF's SoftAP netif runs a DHCP server by default (device at 192.168.4.1), `EspHttpServer` serves the page, and `EspWifi::scan()` lists nearby SSIDs for a dropdown. `esp-idf-svc` supports AP+STA mixed mode, so the device can keep trying the stored network while the portal is up, and the offline scheduler keeps running the relays meanwhile. Captive-portal behaviour (phone pops the page automatically) needs a tiny DNS responder that answers every query with the AP address; the `edge-captive` crate does this, or it is ~50 lines over a UDP socket. Design in section 4, "Provisioning".

### "ESPHome" clarification

ESPHome is a C++ firmware generated from YAML. A Rust firmware **cannot be an ESPHome device** unless it re-implements the ESPHome native API (protobuf over TCP 6053, Noise encryption, mDNS `_esphomelib._tcp`). That is feasible (a Rust crate `esphome-native-api` proves HA accepts a non-ESPHome server, but it needs tokio/std and there is no embedded port) and would take 1-2 weeks plus ongoing maintenance: the protocol changed four times in the last 18 months (password auth removed in 2026.1, MAC added to the Noise hello, API minor bumped to 16, timezone message replaced in 2026.9).

**Decision 2026-09-05 (user): implement the ESPHome native API in Rust.** The device will appear under HA's ESPHome integration, needs no MQTT broker, and gets wall-clock time from HA. MQTT discovery is dropped from the plan (it remains a fallback idea only). Consequences:
- Phase 2 becomes "ESPHome native API server" (see section 4a). Protocol source of truth: `esphome/proto/api.proto` vendored from aioesphomeapi; re-diff it a few times a year.
- Schedule push from HA uses an ESPHome **user-defined service** (`esphome.<device>_set_schedule` with a JSON string argument) instead of an MQTT topic. Offline recovery relies on the copy in NVS; HA re-pushes on device reconnect (blueprint triggered on the device becoming available).
- Time: `GetTimeRequest` to HA on every connection plus SNTP as backup.
- A pure-Rust `esphome-api` crate (framing, Noise handshake, protobuf, entity model) is tested on the Mac against the official `aioesphomeapi` Python client before touching the board.

---

## 2. Hardware facts (confirmed from the Waveshare schematic and demo code)

| Function | GPIO | Notes |
|---|---|---|
| Relay CH1..CH6 | 1, 2, 41, 42, 45, 46 | **Active-high**. 100K pull-down on each driver base, so relays are OFF at boot and while the pin floats. GPIO45/46 are strapping pins; safe as wired, do not add pull-ups. |
| RGB LED | 38 | Single WS2812B, 3V3 |
| Buzzer | 21 | Passive, needs PWM (LEDC 1 kHz) |
| RS485 | TX 17, RX 18 (UART1) | Direction is **hardware automatic**, no DE/RE pin. 120R terminator via jumper H3, off by default. Galvanically isolated. |
| BOOT button | 0 | Usable as user button after boot |
| RESET button | EN | |
| USB-C | 19/20 | **Native USB-Serial-JTAG**, no CH343/CP2102. Console must go over USB. Download mode: hold BOOT, tap RESET. |
| UART0 | 43/44 | Only on the Pico header |
| I2C (for RTC HAT) | SDA 4, SCL 5 | Used by Waveshare's DS3231 HAT |
| Hardware RTC | none | |
| Flash / PSRAM | 16 MB flash (N16), no PSRAM | Board sits on `/dev/cu.usbmodem114401`, MAC 10:51:db:47:ab:b0 |

Power: 7-36 V DC screw terminal or 5 V USB-C. Relays HLS8L-DC5V, SPDT, 10 A 250 VAC / 10 A 30 VDC. Use the DC terminal in deployment; six coils plus WiFi may brown out a weak USB port.

Existing ESPHome configs confirming the pin map: https://devices.esphome.io/devices/waveshare-6ch-relay/ and https://github.com/ryansch/esphome-config/blob/main/waveshare-esp32-s3-relay-6ch.yaml

---

## 3. Rust stack decision

**Use the std path: `esp-idf-svc` 0.52 / `esp-idf-hal` 0.46 on ESP-IDF v5.5.x.**

Why not `esp-hal` + embassy (no_std)? esp-hal 1.2 is stable for GPIO/UART/SPI/I2C only; RMT (needed for the WS2812), watchdog, and timers are still `unstable`; the WiFi driver `esp-radio` is at 1.0.0-beta.0; OTA is a do-it-yourself image writer; TLS is a young crate. For a WiFi + MQTT + SNTP + NVS + OTA device in 2026, the std path maps every need to a production-grade Espressif component with an existing Rust wrapper and example. Revisit no_std when esp-radio 1.0 ships.

| Need | Crate / API |
|---|---|
| WiFi STA | `esp_idf_svc::wifi::{EspWifi, BlockingWifi}` |
| MQTT (+TLS optional) | `esp_idf_svc::mqtt::client::EspMqttClient`, LWT for availability |
| SNTP | `esp_idf_svc::sntp::EspSntp` + POSIX `TZ` (Portugal: `WET0WEST,M3.5.0/1,M10.5.0`) |
| Settings + schedule storage | `esp_idf_svc::nvs::EspNvs` (blobs); LittleFS via `joltwallet/littlefs` component if files are wanted |
| OTA | `esp_idf_svc::ota::EspOta`, rollback via `CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE=y` |
| Watchdog | `esp_idf_hal::task::watchdog::TWDTDriver` |
| Relays | `esp_idf_hal::gpio::PinDriver::output` |
| WS2812 | `ws2812-esp32-rmt-driver` 0.14 |
| RS485 | `esp_idf_hal::uart::UartDriver` (plain UART; board handles direction) |
| Buzzer | `esp_idf_hal::ledc` |
| Optional RTC | `ds323x` crate over `esp_idf_hal::i2c` |
| JSON | `serde` + `serde_json` |

Toolchain (macOS Apple Silicon; rustup already present, espup/espflash/ldproxy missing):

```sh
brew install cmake ninja dfu-util libusb ccache
cargo install espup ldproxy espflash cargo-espflash cargo-generate --locked
espup install -t esp32s3          # writes ~/export-esp.sh
. ~/export-esp.sh                 # add to ~/.zshrc
cargo generate esp-rs/esp-idf-template cargo   # MCU esp32s3, ESP-IDF v5.5.x
cargo build --release
espflash flash --monitor target/xtensa-esp32s3-espidf/release/<bin>
```

Partition table for the 16 MB module with two 4 MB OTA slots (a std WiFi+MQTT+TLS app is ~1.2-1.8 MB):

```
# Name,   Type, SubType, Offset,  Size
nvs,      data, nvs,     0x9000,  0x6000
otadata,  data, ota,     0xf000,  0x2000
phy_init, data, phy,     0x11000, 0x1000
ota_0,    app,  ota_0,   0x20000, 0x400000
ota_1,    app,  ota_1,   ,        0x400000
storage,  data, spiffs,  ,        0x700000
```

Gotchas: pin `ESP_IDF_VERSION=v5.5.3`; first build compiles ESP-IDF (5-15 min); set `ESP_IDF_TOOLS_INSTALL_DIR=global` to share across projects; give Rust threads bigger stacks than the C defaults; `rustflags = ["--cfg", "espidf_time64"]`.

---

## 4. System design

### Operating modes and arbitration

The two modes are not exclusive. The device has one **scheduler** and one **relay arbiter**, and a single **link state**:

- `Online`: MQTT connected and HA seen alive (HA birth message or any command within the last N minutes).
- `Offline`: MQTT disconnected, or no HA heartbeat for `offline_grace` seconds (default 120 s, configurable from HA).

Behaviour:

| Situation | Relay control |
|---|---|
| Online | HA commands only (`chN/set ON/OFF`). The local schedule is evaluated but only **reported** (sensor "schedule wants: on/off"), not applied. |
| Offline, time known | Local schedule applied at every minute boundary and on every transition Online -> Offline. |
| Offline, time unknown (cold boot, no RTC, no SNTP) | Relays held in per-channel `safe_state` (default OFF). |
| Offline -> Online | Device keeps current relay state and publishes it. HA reconciles (see add-on / automation below, which re-sends desired state when the device becomes available). |
| Always | Per-channel **max-on-time** safeguard: a timer starts when a relay turns on, from any source; on expiry the relay turns off and a `binary_sensor` "safeguard tripped" is raised. Value 0 = disabled. |
| Always | **Exclusive mode** (HA switch, persisted, added 2026-09-05 in fw 0.4.0): when on, switching any relay on first switches every other relay off, for HA, schedule and local sources alike. Enabling it while several relays are on keeps the lowest-numbered one. |
| Always | Manual override: BOOT button long-press opens/closes the setup AP. |

Persisted in NVS: WiFi credentials, MQTT broker/credentials, per-channel `max_on_min`, `safe_state`, `mode` (auto/manual/off), schedule blob + revision, TZ string, `offline_grace`.

### 4a. ESPHome native API server (replaces MQTT)

Crate `esphome/` (`esphome-api`): std, no ESP dependencies, generic over `std::io::Read + Write` streams.
- **Framing**: plaintext (`0x00`, varint len, varint type) and Noise (`0x01`, u16 BE len). Noise pattern `Noise_NNpsk0_25519_ChaChaPoly_SHA256`, prologue `NoiseAPIInit\0\0`, PSK = base64 key stored in NVS and shown in the provisioning portal. Crates: `noise-protocol` + `noise-rust-crypto` (pure Rust, no_std capable).
- **Protobuf**: `prost` with code generated at build time from `esphome/proto/api.proto` (protoc via `protoc-bin-vendored` or brew). Only the messages we serve are wired into the dispatcher; unknown message types are ignored.
- **Messages served**: Hello, (Connect if still sent), Disconnect, Ping, DeviceInfo, ListEntities* + Done, SubscribeStates, Switch (6 relays), Number (6 max-on-time, minutes), Select (mode), BinarySensor (safeguard tripped per channel, schedule loaded), Sensor (uptime, RSSI, schedule revision), TextSensor (link state, schedule summary), Button (all off, reload schedule, restart), user-defined Services (`set_schedule(json)`, `clear_schedule()`), GetTime (device asks HA on connect), SubscribeLogs (served since fw 0.7.0: a tee logger forwards console log lines to subscribed clients; API-layer chatter excluded to avoid feedback).
- **mDNS**: `_esphomelib._tcp` on 6053 with TXT `friendly_name`, `version`, `mac`, `board`, `platform=ESP32`, `network=wifi`, `api_encryption=Noise_NNpsk0_25519_ChaChaPoly_SHA256` via `esp_idf_svc::mdns` (espressif/mdns component).
- **Multiple clients**: HA reconnects aggressively; support at least 2 concurrent connections, each on its own thread with a bounded stack.
- **Keepalive**: answer PingRequest; send PingRequest after 60 s idle and drop after ~150 s without reply.
- **Host test**: `cargo run --example host-server` on the Mac exposes a fake device on 127.0.0.1:6053; verified with `aioesphomeapi` (installed in `/tmp/esphome-venv`) and then by adding it to HA manually by IP.

### MQTT topics (superseded by 4a, kept for reference only; device id = `relay6_<mac6>`)

| Topic | Dir | Payload |
|---|---|---|
| `relay6/<id>/status` | dev -> HA | `online` / `offline` (LWT, retained) |
| `relay6/<id>/chN/set` | HA -> dev | `ON` / `OFF` |
| `relay6/<id>/chN/state` | dev -> HA | `ON` / `OFF` (retained) |
| `relay6/<id>/chN/max_on/set` and `/state` | both | minutes, integer |
| `relay6/<id>/schedule/set` | HA -> dev | JSON (retained) |
| `relay6/<id>/schedule/state` | dev -> HA | `{"rev":42,"ok":true,"blocks":7}` (retained) |
| `relay6/<id>/time/set` | HA -> dev | `{"epoch":..., "tz":"..."}` fallback when SNTP is blocked |
| `relay6/<id>/link` | dev -> HA | `online` / `offline_schedule` / `offline_unknown_time` |
| `relay6/<id>/cmd` | HA -> dev | `reload_schedule`, `all_off`, `ota:<url>`, `restart` |

Discovery: one retained **device-based discovery** message on `homeassistant/device/<id>/config` (HA >= 2024.11) declaring 6 `switch`, 6 `number` (max on time, `entity_category: config`, unit min, 0-1440), `select` mode, `binary_sensor` safeguard-tripped and schedule-loaded, `sensor` link state / schedule revision / RSSI / uptime, `button` reload schedule and all-off. Re-publish discovery when HA publishes `online` on `homeassistant/status`.

### Schedule payload (retained on the broker, mirrored in NVS)

```json
{
  "rev": 42,
  "tz": "WET0WEST,M3.5.0/1,M10.5.0",
  "channels": {
    "1": [ {"days": "MTWTF--", "from": 360, "to": 420} ],
    "2": [ {"days": "-----SS", "from": 480, "to": 540},
           {"days": "MTWTFSS", "from": 1200, "to": 1260} ]
  }
}
```

`from`/`to` are local minutes since midnight; DST is handled on the device via the TZ string, so HA never needs to re-publish at DST changes. The device acks with `rev`. Because the message is retained, a device that reboots while HA is down still receives the last schedule from the broker.

### Schedule sync from HA: two options

**Option A (zero code, do first):** one `schedule.*` helper per channel (`schedule.relay_ch1` ...). An HA automation triggered on any change of those helpers, on HA start, and on the device's `link` sensor becoming `online`, calls `schedule.get_schedule` with `response_variable` and publishes the JSON above with `mqtt.publish` (`retain: true`). A second automation, triggered by the same helpers' `on`/`off` state changes, sends `chN/set` while online. Ship this as an HA **blueprint** in the repo.

**Option B (the add-on you described):** a small Rust service packaged as a Supervisor add-on (`homeassistant_api: true`, `services: [mqtt:need]`). It authenticates to `ws://supervisor/core/websocket` with `SUPERVISOR_TOKEN`, calls `schedule/list`, subscribes to changes, validates, diffs and publishes the retained JSON, and re-sends desired relay states when a device comes back online. Gains over A: per-block `data` (e.g. mapping any schedule to any channel), validation with a clear error, multiple devices, a small config UI. Costs: add-on repo, Docker image build for aarch64/amd64, HA OS/Supervised only.

Recommendation: A for the MVP, B once the firmware is stable. Share the schedule schema as a Rust crate used by both firmware and add-on.

### Provisioning (AP mode)

Entry conditions, any of:
- No WiFi credentials in NVS (first boot).
- Stored network not reachable for `provision_after` minutes (default 10), STA keeps retrying underneath (AP+STA mode).
- BOOT button held for 5 s **after** boot (not during reset: GPIO0 low at reset enters USB download mode). LED shows a distinct colour, buzzer chirps.
- Three power cycles within 10 s (counter kept in RTC memory), as a button-free fallback.

While provisioning:
- SoftAP `relay6-<mac6>`, WPA2 with a per-device default password printed on a label, or open if you prefer; DHCP server built into ESP-IDF, device at 192.168.4.1.
- Captive DNS answering every name with 192.168.4.1 (`edge-captive` crate or a small UDP responder), plus HTTP redirects for the Android/iOS/Windows connectivity-check URLs so the portal pops automatically.
- Single static page served by `EspHttpServer` (HTML embedded with `include_str!`): SSID dropdown from `EspWifi::scan()` with a "rescan" button and a free-text field for hidden networks, password, optional static IP, MQTT host/user/password, device name. POST saves to NVS, device tries STA; page polls a `/status` JSON endpoint and reports success (with the new IP) or failure (wrong password / not found), then reboots into normal mode.
- Relays keep following the stored schedule and safeguard timers during provisioning; nothing is interrupted.
- Factory reset (added fw 0.5.0, verified end to end in 0.5.2 on 2026-09-05; fixes: seed no longer re-applies after reset, AP runs APSTA so scanning works, peer detection handles IPv4-mapped IPv6): BOOT held 15 s wipes credentials/key/names/schedule/config, generates a new API key, reboots into the AP; the key is shown on the portal only to AP clients (peer address check), which also need no current key to save.
- Portal times out after 15 min without a client and the device goes back to retrying the stored network.

Also expose the same page (without the AP) as a local settings page on the STA address, protected by the MQTT password or a device PIN, so credentials can be changed later without entering AP mode.

Alternative considered: ESP-IDF's `wifi_provisioning` component (SoftAP or BLE with the Espressif phone app). Rejected for now: needs the vendor app and has no Rust wrapper in esp-idf-svc; the web portal is friendlier and fully under our control.

### Time

SNTP on boot and every hour; TZ string from the schedule payload; HA fallback epoch topic (automation publishes `{{ now().timestamp() | int }}` on device birth). Optional DS3231: firmware reads it at boot, writes it after each SNTP sync.

---

## 5. Repository layout

```
esp32-cascalheira/
  core/                    # relay-core: host-testable schedule model, evaluator,
                           # arbiter state machine, max-on-time timers, JSON schema
  firmware/                # relay-fw: esp-idf-svc binary, own .cargo/config.toml
                           # (xtensa target), depends on ../core by path
  addon/                   # HA Supervisor add-on service (tokio), phase 6
  ha/
    blueprints/              # schedule sync + relay drive automations (Option A)
    addon/                   # config.yaml, Dockerfile, repository.json (Option B)
  hw/                        # pinout.md, schematic link, partitions.csv
  PLAN.md
```

`relay-core` is compiled and unit-tested on the Mac with plain `cargo test` (no hardware), including DST edge cases and the online/offline transitions. The firmware crate is a thin I/O shell around it.

---

## 6. Phases

| # | Milestone | Deliverable / acceptance |
|---|---|---|
| 0 ✅ 2026-09-05 | Toolchain | espup + espflash installed; template project blinks the WS2812 and toggles relay 1 from USB console. Confirm flash size from module label. |
| 1 ✅ 2026-09-05 | Core logic (host) | `relay-core` with schedule parsing, evaluator, arbiter, safeguard timers; `cargo test` green on macOS. |
| 2 ✅ 2026-09-05 (device passes aioesphomeapi smoke test at 192.168.88.50; HA pairing pending user) | ESPHome native API | `esphome-api` crate passes host tests against aioesphomeapi (plaintext + Noise). Firmware: WiFi STA from NVS, mDNS, API server; device is discovered by HA 2026.9, 6 switches + 6 numbers + mode select + diagnostics appear; time arrives via GetTime; max-on-time enforced and persisted. |
| 2b ✅ 2026-09-05 (AP+captive DNS+redirects+scan+save validation verified from a Mac joined to the AP; OTA in AP+STA mode verified on 0.3.1) | Provisioning portal | AP+STA mode, DHCP, captive DNS, scan-and-pick setup page; entry via missing credentials, long BOOT press, or connection failure; verified from a phone. |
| 3 ✅ 2026-09-05 (HA integration disabled: relay followed stored schedule, state kept on reconnect) | Offline mode | Schedule topic stored in NVS and acked; link-state machine; unplug the router and watch relays follow the stored schedule; cold boot without network holds safe state until time is known. |
| 4 ✅ 2026-09-05 device sync implemented in the user's HACS integration ha-garden-irrigation (plan.py + device_sync.py, v0.21.0); verified with mocked HA and against the real board; awaiting deployment in the user's HA. Blueprint kept as fallback. | HA side | Blueprint: on any `schedule.*` helper change or device reconnect, call `schedule.get_schedule` and the device action `esphome.<device>_set_schedule` with the JSON; per-channel schedule helpers drive the switches while online. |
| 5 ✅ 2026-09-05: OTA via HTTP pull + rollback, task watchdog, RSSI/uptime/heap diagnostics, buzzer feedback (fw 0.6.1) | Ops | OTA via `EspOta` from an HTTP URL sent on `cmd`, rollback enabled, task watchdog, RSSI/uptime diagnostics, buzzer feedback, LED status pages. |
| 6 | Add-on, Option B | Rust add-on publishing schedules and reconciling state; multi-device. |
| 7 | Optional | DS3231 RTC HAT support; RS485 Modbus master for sensors/meters; MQTT transport as a second path if ever wanted. |

---

## 7. Risks and open points

- **No RTC**: cold boot with no network cannot run a time-of-day schedule. Decide between "hold safe state" and the DS3231 HAT.
- **Relay 5/6 on strapping pins** GPIO45/46: safe as wired; never add external pull-ups, and keep those pins low during reset (the board's pull-downs do this).
- **WiFi needs the SMA antenna** attached (1U module, no PCB antenna).
- **First ESP-IDF build** is slow; disk usage a few GB.
- **HA version**: device-based discovery needs HA 2024.11+; `schedule.get_schedule` needs 2024.x+. Both are old enough by now.
- **Mixed AP+STA mode**: after toggling the AP the default netif stuck on the AP interface and broke outbound HTTP (OTA); fixed in 0.3.1 by pinning the station netif as default (`esp_netif_set_default_netif`).

## Sources

Board: https://spotpear.com/wiki/ESP32-S3-WROOM-1U-N8-WIFI-RS485-Bluetooth-Industrial-6-Channel-Relay-IOT.html , https://www.waveshare.com/wiki/ESP32-S3-Relay-6CH , schematic https://files.waveshare.com/wiki/ESP32-S3-Relay-6CH/ESP32-S3-Relay-6CH.pdf , demo code https://files.waveshare.com/wiki/ESP32-S3-Relay-6CH/ESP32-S3-Relay-6CH-Demo.zip
Rust: https://docs.espressif.com/projects/rust/book/ , https://github.com/esp-rs/esp-idf-svc , https://github.com/esp-rs/esp-idf-hal , https://github.com/esp-rs/esp-idf-template , https://github.com/esp-rs/espup , https://github.com/cat-in-136/ws2812-esp32-rmt-driver , https://developer.espressif.com/blog/2025/10/esp-hal-1/
HA: https://www.home-assistant.io/integrations/mqtt/ , https://www.home-assistant.io/integrations/schedule/ , https://www.home-assistant.io/actions/schedule.get_schedule/ , https://developers.home-assistant.io/docs/apps/communication/ , https://developers.esphome.io/architecture/api/protocol_details/ , https://github.com/UbiHome/esphome-native-api
