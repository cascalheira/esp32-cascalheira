# relay-fw

Rust (esp-idf-svc, ESP-IDF v5.5.3) firmware for the Waveshare/SpotPear ESP32-S3-Relay-6CH.

## Build and flash

```sh
. ~/export-esp.sh                 # Xtensa LLVM/GCC paths written by espup
export RUSTUP_TOOLCHAIN=esp       # needed: the shell profile pins RUSTUP_TOOLCHAIN to stable,
                                  # which overrides rust-toolchain.toml
cargo build --release
espflash flash --flash-size 16mb --partition-table partitions.csv \
    target/xtensa-esp32s3-espidf/release/relay-fw
espflash monitor --non-interactive
```

`espflash.toml` already pins the serial port (`/dev/cu.usbmodem114401`), the 16 MB flash size and the
partition table, so `cargo run --release` also works once the port config has been migrated.

If the project directory is moved or renamed, run `cargo clean -p esp-idf-sys --release` once:
the ESP-IDF CMake build under `target/` caches absolute paths and fails otherwise.

Console is on the native USB-Serial-JTAG. If the port stops enumerating, hold BOOT, tap RESET, release
BOOT, then flash again.

## Layout

- `../core` — hardware-independent logic, tested on the host with `cargo test`.
- `src/main.rs` — firmware entry point.
- `partitions.csv` — 16 MB layout, two 4 MB OTA slots, 7 MB storage.
- `sdkconfig.defaults` — stack sizes, USB console, rollback, task watchdog.

## Publishing a release (one-click install in HA)

```sh
# bump version in Cargo.toml, then:
./release.sh "release notes"
```
Builds, publishes the image as GitHub release `relay-fw-v<version>` and commits
`release/latest.json`, which every board fetches a minute after boot and every 6 h. HA then shows
an Install button on the Firmware entity. Note GitHub's raw CDN can lag about a minute.

## Installing an unreleased build over the air

The device pulls a firmware image over HTTP when its `ota` action is called (HA: Developer tools →
Actions → `esphome.relay6_47abb0_ota` with `url`). Rollback is enabled: if the new image does not
get an HA client (or run 5 minutes) before a reset, the bootloader returns to the previous slot.

```sh
# 1. bump version in Cargo.toml, then build and export a flat image
cargo build --release
espflash save-image --chip esp32s3 --flash-size 16mb \
    target/xtensa-esp32s3-espidf/release/relay-fw /tmp/ota/relay-fw.bin
# 2. serve it from this machine (192.168.88.20 on the home LAN)
(cd /tmp/ota && python3 -m http.server 8000)
# 3. trigger from HA (or the smoke-test client): url = http://192.168.88.20:8000/relay-fw.bin
```
Progress shows in the "Firmware update" diagnostic sensor. USB flashing is only needed for
bootloader or partition-table changes; pass `--bootloader <idf bootloader.bin>` then, because
espflash's bundled bootloader lacks rollback support.

## Provisioning portal

The setup page is always served on port 80 (`http://<device-ip>/`). A setup access point
`relay6-<mac6>` (WPA2 password `relay6setup`, device at 192.168.4.1, captive portal) opens when:

- there are no WiFi credentials in NVS (first boot without `secrets.env`), or
- the BOOT button is held for 5 s (LED turns blue; hold again to close), or
- the home network has been unreachable for 10 minutes.

It closes 15 minutes after the station is back online. The page lets you pick a scanned network or
type a hidden SSID, set the password, device name, friendly name and the Home Assistant API key
(with a generator). Over the LAN, saving requires the current API key. Saving a new WiFi network
reconnects live; a new name or key restarts the device after 25 s.

The page also shows the stored weekly schedule as a grid (`/api/schedule` returns it as JSON).

LED colours: red = no WiFi, amber = WiFi but no Home Assistant, green = online, blue = setup AP.

Buzzer (can be muted with the Buzzer switch in HA): two-tone chirp at boot, rising/falling triad when the setup AP opens/closes, three beeps on a safeguard trip, long low tone on factory reset, double beep when an OTA image is installed, low tone on OTA failure.

## Reset behaviour

- **RESET button / power cycle**: plain reboot. Everything persists (WiFi, API key, names, mode,
  max-on-times, schedule, timezone). Relays start OFF; the schedule resumes once the clock is
  known (from HA, SNTP, or the ESP32's internal clock after a warm reboot).
- **Factory reset (move to another home)**: hold BOOT for 15 s. At 5 s the LED turns blue (setup
  AP), keep holding; at 15 s it flashes red/white and the device erases WiFi credentials, API key,
  names, schedule and settings, generates a **new random API key**, and reboots into the setup
  access point `relay6-<mac6>` (password `relay6setup`). The build-time seed in `secrets.env`
  is not re-applied after a reset.
- On the setup page reached **through the access point**, the API key is displayed with a copy
  button and no current key is needed to save settings. Over the LAN the key is never shown and
  changes require it. Add the device again in Home Assistant with the new key (or use
  "Reconfigure" on the existing ESPHome entry to update the key).

