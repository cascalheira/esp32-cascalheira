# Relay board user manual

Six-channel relay controller (Waveshare ESP32-S3-Relay-6CH) running the `relay-fw` firmware. It
appears in Home Assistant as an ESPHome device and keeps running its watering schedule on its own
when Home Assistant is unreachable.

---

## 1. Safety

- The relays are rated 10 A at 250 V AC / 30 V DC. Anything connected to them can carry **mains
  voltage**. Switch power off before touching the terminals.
- Mount the board in a dry enclosure. The relay contacts are not fused; fuse each load circuit.
- Never connect a load that must not run unattended. The board can switch relays by itself from
  its stored schedule.

## 2. Wiring

| Terminal | What to connect |
|---|---|
| DC IN (7–36 V) | Power supply. Preferred over USB: six relay coils plus WiFi can overload a weak USB port. |
| USB-C | Alternative 5 V supply and the console for firmware development. Not needed in daily use. |
| CH1..CH6: NO / COM / NC | Each relay has a normally-open, common and normally-closed contact. Valves and pumps go between NO and COM. |
| RS485 A/B | Not used by this firmware. |
| SMA antenna | **Required.** The module has no internal antenna; without it WiFi will not connect. |

Relays are **off** when the board has no power and immediately after it starts.

## 3. Lights, sounds and buttons

**RGB LED**

| Colour | Meaning |
|---|---|
| Red | No WiFi connection |
| Amber | On WiFi, but Home Assistant is not connected |
| Green | Home Assistant connected: normal operation |
| Blue | Setup network (access point) is open |
| Red/white flashing | Factory reset in progress |

**Buzzer** (can be muted with the *Buzzer* switch in Home Assistant)

| Sound | Meaning |
|---|---|
| Two short rising tones | Board started |
| Three rising notes / three falling notes | Setup network opened / closed |
| Three sharp beeps | A safeguard switched a relay off (it stayed on too long) |
| One long low tone | Factory reset |
| Two short beeps | Firmware update installed, restarting |
| One low tone | Firmware update failed |

**BOOT button** (the small button next to RESET)

| Press | Effect |
|---|---|
| Hold 5 s | Open the setup network (LED blue). Hold 5 s again to close it. |
| Hold 15 s | **Factory reset** (see section 9). Keep holding through the blue stage until the LED flashes red. |

**RESET button**: restarts the board. Nothing is lost.

## 4. First-time setup

1. Attach the antenna, connect power. The LED turns **blue**: the board has no WiFi credentials
   and opens its own network.
2. On a phone or laptop, join the WiFi network **`relay6-xxxxxx`** (the six characters are the end
   of the board's MAC address). Password: **`relay6setup`**.
3. A setup page opens automatically. If it does not, browse to **http://192.168.4.1/**.
4. On the page:
   - Pick your home WiFi network from the list (press *Rescan* if the list is still empty) or type
     a hidden network name, and enter its password.
   - Optionally change the device name (used as hostname, letters, digits and dashes) and the
     friendly name shown in Home Assistant.
   - Note the **Home Assistant API key** shown at the top and copy it. You will paste it into Home
     Assistant. It is only displayed on the setup network.
   - Press **Save and connect**. The page reports when the board has joined your network and shows
     its IP address. If it says the network could not be joined, check the password and save again.
5. The setup network closes by itself 15 minutes after the board is online, or hold BOOT for 5 s.

## 5. Adding the board to Home Assistant

1. Home Assistant discovers the board within a minute. Go to **Settings → Devices & services**;
   a **Discovered: ESPHome** card shows the device. Press **Configure**.
2. Paste the API key from the setup page, then Submit and choose an area.
3. If nothing is discovered (for example the board is on a different network segment), press
   **Add integration → ESPHome**, enter the board's IP address and port **6053**, then the key.

The device page shows everything the board offers. What each control does:

| Control | Meaning |
|---|---|
| Relay 1..6 | The relays. On/off. |
| Relay N max on time | Safety limit in minutes. A relay that stays on longer is switched off by the board itself, whoever turned it on. 0 disables the limit. Garden Irrigation sets these automatically from your schedule. |
| Relay N safeguard tripped | Turns on when the limit above fired; clears when the relay is switched on again. |
| Relay N max per day | Daily budget in minutes (0 = none). When a relay's total on-time for the day reaches it, the relay is switched off and **cannot be switched on again by anyone** until local midnight. Protects against a stuck schedule or a runaway automation watering in many short runs. |
| Relay N on today | Minutes the relay has been on since local midnight. Kept across restarts. |
| Relay N total on time | Lifetime minutes the relay has been on. Kept across restarts. Home Assistant's Statistics dashboard can chart it per day, week or month (it is a "total increasing" sensor). |
| Relay N daily limit reached | On while the relay is blocked by its daily budget. |
| Mode | *Auto*: Home Assistant controls the relays, and the stored schedule takes over when Home Assistant is offline. *Manual*: Home Assistant only; the schedule never runs. *Off*: all relays off, commands ignored. |
| Exclusive mode | Only one relay on at a time: switching a relay on switches all others off first. Leave off if one relay is a master valve that must run together with a zone. |
| Relay N on when clock unknown | What the relay does after a power cut when the board has no network and does not know the time yet. Default off. |
| Rain hold / Rain hold until | Suspends the board's **stored schedule** for the given number of hours (0 = off), for example when it is raining and Home Assistant is down. Relays that the schedule had switched on go off at once. It never blocks commands from Home Assistant, which keeps its own rain delay. Survives reboots. |
| Buzzer | Mutes the buzzer. |
| All relays off | Emergency stop. |
| Restart | Restarts the board (nothing is lost). |
| Link, Device local time, Schedule, Schedule loaded, Schedule revision | Status of the connection to Home Assistant and of the stored schedule. |
| Uptime, WiFi signal, Free heap, Lowest free heap, Last reset reason, Firmware update | Health information. |
| Last crash / Clear crash report | If the board ever crashes, this holds a one-line report (reason, firmware version, failing task and program address, and the error message when available) until you press Clear. "none" means no crash recorded. |

## 6. Schedules and what happens when Home Assistant is down

You do not program the board directly. Set up watering in the **Garden Irrigation** integration
as usual and choose the board's relay switches (`Relay 1`..`Relay 6`) as the zones' switches.
Every time you change a schedule, Garden Irrigation sends the resulting weekly plan to the board,
which stores it in its memory. The Garden Irrigation card shows "Offline plan on relay6-…" with
the time of the last transfer.

While Home Assistant is running, **it** switches the relays, with rain and frost skips, soak
cycles, notifications and so on. The board only listens.

If it is raining and Home Assistant is down, set **Rain hold** (hours) on the device page, or via the
setup page later, to pause the stored schedule.

If Home Assistant becomes unreachable (server down, router down, network cable out), the board
notices within about two minutes, switches to its stored plan and turns relays on and off at the
scheduled times by itself. Skips that depend on weather data are not applied offline. When Home
Assistant returns, it takes over again without disturbing relays that are on.

If the board loses power and comes back while Home Assistant is still unreachable, it does not
know the time until it can reach a time server or Home Assistant. Until then each relay holds
its "on when clock unknown" setting (default: off).

## 7. Everyday use

- Switch relays from the device page, the dashboard, or the Garden Irrigation card.
- A relay switched on by hand stays on until you switch it off, the schedule's next change, or
  the max-on-time safeguard, whichever comes first.
- Green LED means all is well. Amber for more than a few minutes means Home Assistant cannot reach
  the board: check the ESPHome integration and the API key.

## 8. Firmware updates

Updates arrive over WiFi; no cable is needed.

- The board checks for a new firmware a minute after it connects and every six hours. When one
  exists, Home Assistant shows it under **Settings** (an update badge) and on the device page as
  **Firmware** with an **Install** button and the release notes.
- Press **Install**. The board downloads the image (progress is shown), beeps twice and restarts.
  About 20 seconds later the device page shows the new version. A single low beep means the
  download failed; the board keeps running the current firmware.
- If the new firmware fails to start properly, the board returns to the previous version by itself.
- Developer tools → Actions → `esphome.relay6_xxxxxx_ota` with an image URL still works for
  installing a test build that is not a published release.

## 9. Factory reset and moving the board to another home

Hold **BOOT for 15 seconds**: the LED turns blue at 5 s, keep holding, and at 15 s it flashes
red and white while the board erases WiFi credentials, API key, names, schedule and settings. It
then restarts with a **new API key** and opens the setup network.

Then follow section 4 at the new location, and section 5 to add it to the new Home Assistant.
At the old Home Assistant, delete the device from the ESPHome integration.

If you keep the same Home Assistant but reset the board anyway: open the ESPHome integration
entry, choose **Reconfigure**, and enter the new key shown on the setup page.

**Lost the key but the board still works?** Hold BOOT 5 s, join the setup network and read the
key on the page. The key is only ever shown there.

## 10. Troubleshooting

| Symptom | What to check |
|---|---|
| LED stays red | No WiFi. Antenna attached? Correct password? The board tries again forever; after 10 minutes it also opens the setup network so you can fix the credentials. |
| LED amber, device "unavailable" in Home Assistant | Wrong or changed API key (Reconfigure the ESPHome entry), or Home Assistant cannot reach the board's IP (different VLAN, firewall). |
| Setup page shows no networks | Press Rescan and wait a few seconds; scanning takes a moment. |
| Setup page asks for a "current API key" | You reached the page over the home network, not the setup network. Either enter the key, or hold BOOT 5 s and use the setup network. |
| A relay switches off by itself after a while | Its max on time fired ("safeguard tripped"). Raise the limit or set it to 0. |
| Only one relay ever stays on | Exclusive mode is on. |
| Relays never move while Home Assistant is down | Mode is *Manual*, or no schedule was stored ("Schedule loaded" off). Edit a schedule in Garden Irrigation to resend it. |
| Board restarts by itself | Read *Last reset reason*: "BROWNOUT" means a weak power supply; "TASK WATCHDOG" or "PANIC" is a firmware fault. *Last crash* then holds the details to report. |
| Forgot everything | Factory reset (section 9). |

## 11. Reference

| Item | Value |
|---|---|
| Setup network | `relay6-xxxxxx`, password `relay6setup`, page at http://192.168.4.1/ |
| Setup page on the home network | http://\<board IP\>/ (changes need the current API key) |
| Home Assistant port | 6053 (ESPHome native API, encrypted) |
| Device name | `relay6-xxxxxx` (hostname `relay6-xxxxxx.local`) |
| Offline switch-over delay | about 2 minutes after Home Assistant disconnects |
| Setup network auto-close | 15 minutes after the board is online |
