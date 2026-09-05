# Home Assistant side

## 1. Add the device

The board advertises itself over mDNS as an ESPHome node (`relay6-<mac6>`). Home Assistant
shows it under **Settings → Devices & services → Discovered** and asks for the encryption key:
paste the `NOISE_PSK` value from `firmware/secrets.env`. If discovery does not pop up, add the
**ESPHome** integration manually with the device IP and port 6053.

Entities created (device name prefix omitted): Relay 1..6 (switch), Relay N max on time
(number, minutes, 0 = off), Relay N safeguard tripped (binary sensor), Mode (auto/manual/off), Exclusive mode (switch: only one relay on at a time), Relay N on when clock unknown (switch: the state held after a cold boot with no network and no time), Buzzer (switch), Free heap / Lowest free heap (sensors), Last reset reason,
Link, Device local time, Schedule, Schedule loaded, Schedule revision, Uptime, WiFi signal,
All relays off, Restart. Actions: `esphome.<node>_set_schedule(json)` and
`esphome.<node>_clear_schedule`.

## 2. Schedules that keep running offline

**Preferred: the Garden Irrigation integration.** Since 0.21.0, `ha-garden-irrigation` detects zones
whose switch is one of this board's relays and pushes the weekly plan to the board itself (see its
README, "Offline relay controllers"). Nothing to configure. The blueprint below is only a fallback
for people who schedule with plain HA Schedule helpers instead.

### Fallback: HA Schedule helpers + blueprint

1. Create one **Schedule** helper per relay (Settings → Helpers → Schedule), e.g. `schedule.relay_1`.
2. Import `blueprints/relay6_schedule_sync.yaml` (Settings → Automations → Blueprints → Import,
   or copy it to `config/blueprints/automation/relay6/`).
3. Create an automation from it: pick the device action name (shown on the device page under
   "Actions"), the schedule helpers and the matching relay switches, and the device's **Link**
   sensor.

While the device is online the automation switches the relays from the helpers. Every edit of a
helper, every HA start, every device reconnect and every 6 hours it also pushes the full weekly
plan to the device, which stores it in flash and follows it on its own when HA is unreachable.

## 3. Device logs in Home Assistant

The board forwards its log to clients that subscribe (firmware 0.7.0+). In HA: Settings →
Devices & services → ESPHome → the device entry → **Configure** → enable **Subscribe to logs from
the device**. The lines then appear in HA's own log (Settings → System → Logs, search for the
device name); set the logger level for `homeassistant.components.esphome` to `debug` to see
below warnings. The USB console keeps working as before.
