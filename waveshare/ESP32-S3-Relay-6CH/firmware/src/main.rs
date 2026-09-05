//! relay-fw: ESPHome-native-API firmware for the Waveshare ESP32-S3-Relay-6CH.

mod buzzer;
mod clock;
mod crash;
mod entities;
mod logger;
mod ota;
mod portal;
mod settings;
mod updater;
mod wifi;

use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::gpio::{AnyIOPin, AnyOutputPin, Input, Output, PinDriver, Pull};
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::hal::task::watchdog::{config::Config as WdtConfig, TWDTDriver};
use esp_idf_svc::mdns::EspMdns;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esphome_api::codec::Codec;
use esphome_api::noise::{parse_psk, NoiseCodec};
use esphome_api::{runner, Command, Device, Server, State, UpdateState};
use crate::updater::Latest;
use relay_core::{Action, Controller, Link, Schedule, CHANNELS};
use smart_leds::{SmartLedsWrite, RGB8};
use ws2812_esp32_rmt_driver::Ws2812Esp32Rmt;

use crate::buzzer::{Buzzer, Tone};
use crate::clock::Clock;
use crate::entities::Keys;
use crate::portal::PortalContext;
use crate::settings::{NetSettings, Store};
use crate::wifi::WifiCmd;

const API_PORT: u16 = 6053;
const MAX_CLIENTS: usize = 3;
const FW_VERSION: &str = env!("CARGO_PKG_VERSION");
const LONG_PRESS: Duration = Duration::from_secs(5);
const RESET_PRESS: Duration = Duration::from_secs(15);
/// Open the setup AP when the station has been down this long (and credentials exist).
const PORTAL_AFTER_OFFLINE: Duration = Duration::from_secs(10 * 60);
/// Close a button/offline-triggered AP this long after the station is back.
const PORTAL_IDLE: Duration = Duration::from_secs(15 * 60);

/// Everything the main loop reacts to.
pub enum Msg {
    Api(Command),
    Tick,
    WifiUp(String),
    WifiDown,
    Rssi(i32),
    OtaStatus(String),
    /// Result of a `latest.json` check.
    UpdateInfo(Result<Latest, String>),
    LongPress,
    FactoryReset,
    PortalScan,
    Provision(NetSettings),
}

#[derive(PartialEq, Eq)]
enum MsgKind {
    Tick,
    Other,
}

impl From<Command> for Msg {
    fn from(c: Command) -> Msg {
        Msg::Api(c)
    }
}

type Relay = PinDriver<'static, Output>;

struct Led(Ws2812Esp32Rmt<'static>);

impl Led {
    fn set(&mut self, c: RGB8) {
        // The board's WS2812B-0807 takes R,G,B byte order while the driver emits the usual
        // G,R,B, so red and green arrive swapped unless we pre-swap them here.
        let wire = RGB8 { r: c.g, g: c.r, b: c.b };
        let _ = self.0.write([wire].into_iter());
    }
}

const LED_NO_WIFI: RGB8 = RGB8 { r: 24, g: 0, b: 0 };
const LED_WIFI_NO_HA: RGB8 = RGB8 { r: 24, g: 12, b: 0 };
const LED_ONLINE: RGB8 = RGB8 { r: 0, g: 16, b: 0 };
const LED_PORTAL: RGB8 = RGB8 { r: 0, g: 0, b: 28 };

fn main() -> Result<()> {
    esp_idf_svc::sys::link_patches();
    logger::install();
    let reset_reason = reset_reason();
    log::info!("relay-fw {FW_VERSION} starting, slot {}, reset reason: {reset_reason}, free heap {} B", ota::running_slot(), unsafe {
        esp_idf_svc::sys::esp_get_free_heap_size()
    });

    let p = Peripherals::take()?;
    // Task watchdog on the main loop; the 1 s ticker guarantees regular feeding.
    let mut wdt_cfg = WdtConfig::new();
    wdt_cfg.duration = Duration::from_secs(30);
    wdt_cfg.panic_on_trigger = true;
    let mut twdt = TWDTDriver::new(p.twdt, &wdt_cfg)?;
    let mut wdt = twdt.watch_current_task()?;

    let sysloop = EspSystemEventLoop::take()?;
    let nvs_part = EspDefaultNvsPartition::take()?;
    let store = Store::open(nvs_part.clone())?;
    crash::install(esp_idf_svc::nvs::EspNvs::new(nvs_part.clone(), "crash", true)?);
    let crash_report = crash::collect(&format!("{} on {}", reset_reason, FW_VERSION));
    if let Some(r) = &crash_report {
        log::error!("previous boot crashed: {r}");
    }

    // Relays first, all off, before anything else can take time.
    let mut relays: Vec<Relay> = vec![
        PinDriver::output(AnyOutputPin::from(p.pins.gpio1))?,
        PinDriver::output(AnyOutputPin::from(p.pins.gpio2))?,
        PinDriver::output(AnyOutputPin::from(p.pins.gpio41))?,
        PinDriver::output(AnyOutputPin::from(p.pins.gpio42))?,
        PinDriver::output(AnyOutputPin::from(p.pins.gpio45))?,
        PinDriver::output(AnyOutputPin::from(p.pins.gpio46))?,
    ];
    for r in relays.iter_mut() {
        r.set_low()?;
    }
    #[allow(deprecated)]
    let mut led = Led(Ws2812Esp32Rmt::new(p.rmt.channel0, p.pins.gpio38)?);
    led.set(LED_NO_WIFI);

    // Settings: NVS, unless empty or the compiled-in seed (secrets.env) changed since it was
    // last applied. The provisioning portal writes the same keys.
    // The seed is applied only while its fingerprint differs from the one recorded in NVS, i.e.
    // on a never-seeded device or after secrets.env changed. A factory reset records the current
    // fingerprint, so a wiped device stays wiped and opens the setup access point instead.
    let stored = store.load_net();
    let net = match (stored, store.seed_changed(), Store::seed_net()) {
        (_, true, Some(seed)) => {
            log::info!("seeding NVS with build-time credentials (ssid {})", seed.ssid);
            store.save_net(&seed)?;
            store.mark_seeded()?;
            seed
        }
        (Some(n), _, _) => n,
        (None, _, _) => {
            log::warn!("no WiFi credentials: starting setup access point");
            NetSettings { ssid: String::new(), pass: String::new(), psk_b64: String::new(), name: "relay6".into(), friendly_name: "Relay board".into() }
        }
    };
    // Never run without encryption: a fresh or factory-reset device gets a random key, shown on
    // the setup page while the access point is active.
    let net = if net.psk_b64.is_empty() {
        let mut n = net;
        n.psk_b64 = esphome_api::noise::generate_psk_b64();
        log::info!("generated a new API encryption key");
        if let Err(e) = store.save_net(&n) {
            log::error!("save generated key: {e}");
        }
        n
    } else {
        net
    };

    let mac = wifi::mac();
    let mac6: String = mac[3..].iter().map(|b| format!("{:02x}", b)).collect();
    let node_name = format!("{}-{}", net.name, mac6);
    let device = Device {
        name: node_name.clone(),
        friendly_name: net.friendly_name.clone(),
        mac_address: esphome_api::device::mac_from_bytes(&mac),
        model: "esp32-s3-relay-6ch".into(),
        manufacturer: "Waveshare".into(),
        esphome_version: "2026.9.0".into(),
        compilation_time: String::new(),
        project_name: "cascalheira.relay6".into(),
        project_version: FW_VERSION.into(),
        suggested_area: String::new(),
        encryption: !net.psk_b64.is_empty(),
    };
    let psk = if net.psk_b64.is_empty() {
        None
    } else {
        Some(parse_psk(&net.psk_b64).map_err(|e| anyhow::anyhow!("bad NOISE_PSK: {e}"))?)
    };
    log::info!("node {node_name}, mac {}, api {}", device.mac_address, if psk.is_some() { "encrypted" } else { "PLAINTEXT" });

    // Network stack first: the WiFi driver initialises esp_netif/lwIP, which SNTP, sockets and
    // the HTTP server need to exist.
    let (tx, rx) = channel::<Msg>();
    let (wifi_tx, wifi_rx) = channel::<WifiCmd>();
    let mut ap_active = net.ssid.is_empty();
    let wifi_shared = wifi::spawn(
        p.modem,
        sysloop,
        nvs_part,
        node_name.clone(),
        node_name.clone(),
        WifiCmd::SetStation { ssid: net.ssid.clone(), pass: net.pass.clone() },
        ap_active,
        wifi_rx,
        tx.clone(),
    )?;

    // Provisioning portal (HTTP on 80, captive DNS on 53). Always on; the AP comes and goes.
    let portal_ctx = Arc::new(PortalContext {
        node_name: node_name.clone(),
        version: FW_VERSION.into(),
        wifi: wifi_shared,
        current: Mutex::new(net.clone()),
        tx: tx.clone(),
        ap_active: Arc::new(AtomicBool::new(ap_active)),
    });
    let _http = portal::start(portal_ctx.clone())?;
    portal::spawn_captive_dns(portal_ctx.ap_active.clone())?;

    // Core logic.
    let cfg = store.load_config();
    let schedule = store.load_schedule();
    let mut clock = Clock::new(store.load_tz().or_else(|| (!schedule.tz.is_empty()).then(|| schedule.tz.clone())));
    let mut ctl = Controller::new(cfg, schedule);
    if let Some(u) = store.load_usage() {
        log::info!("restored on-time counters: today {:?} min, total {:?} min", u.on_today_ms.map(|m| m / 60_000), u.total_ms.map(|m| m / 60_000));
        ctl.restore_usage(u);
    }
    let buzzer = Buzzer::spawn(p.ledc.timer0, p.ledc.channel0, AnyOutputPin::from(p.pins.gpio21), ctl.config().buzzer)?;

    // API server and entities.
    let (registry, keys) = entities::build();
    let server = Server::new(device, registry, &format!("relay-fw {FW_VERSION}"));
    logger::attach(server.clone());
    publish_all(&server, &keys, &ctl, &clock, None);
    spawn_ticker(tx.clone(), PinDriver::input(AnyIOPin::from(p.pins.gpio0), Pull::Up)?);
    spawn_api_listener(server.clone(), psk, tx.clone())?;
    if ap_active {
        led.set(LED_PORTAL);
    }
    buzzer.play(Tone::Boot);

    // Main loop.
    let boot = Instant::now();
    let mut mdns: Option<EspMdns> = None;
    let mut wifi_up = false;
    let mut sta_down_since: Option<Instant> = Some(Instant::now());
    let mut portal_since: Option<Instant> = ap_active.then(Instant::now);
    let mut last_scan = Instant::now() - Duration::from_secs(60);
    let mut restart_at: Option<Instant> = None;
    let mut ha_clients = 0usize;
    let mut time_known = false;
    let mut tick_count = 0u64;
    let mut tripped = [false; CHANNELS];
    let mut image_confirmed = false;
    let mut ota_running = false;
    let mut latest: Option<Latest> = None;
    let mut update_checked_at: Option<u64> = None;
    let mut update_state = UpdateState { current_version: FW_VERSION.into(), latest_version: FW_VERSION.into(), title: "relay-fw".into(), ..Default::default() };
    server.set_state(keys.update, State::Update(update_state.clone()));
    server.set_state(keys.ota_status, State::Text(format!("{FW_VERSION} idle")));

    for msg in rx {
        wdt.feed()?;
        let now_ms = boot.elapsed().as_millis() as u64;
        let local = clock.local_time();
        ctl.set_epoch(clock.epoch());
        let mut actions: Vec<Action> = Vec::new();
        let creds_exist = !portal_ctx.current.lock().unwrap().ssid.is_empty();
        let msg_kind = if matches!(msg, Msg::Tick) { MsgKind::Tick } else { MsgKind::Other };

        match msg {
            Msg::Tick => {
                tick_count += 1;
                if !time_known && clock.time_known() {
                    time_known = true;
                    log::info!("time became valid via SNTP: {}", clock.local_string());
                    if let Some(l) = local {
                        actions.extend(ctl.on_time_known(now_ms, l));
                    }
                }
                actions.extend(ctl.tick(now_ms, local));
                if tick_count % 30 == 0 {
                    server.publish_state(keys.uptime, State::Float((now_ms / 1000) as f32));
                    server.set_state(keys.local_time, State::Text(clock.local_string()));
                    let (free, min) = publish_heap(&server, &keys);
                    if ctl.rain_hold_until().is_some() {
                        publish_rain_hold(&server, &keys, &ctl, &clock, None);
                    }
                    if tick_count % 600 == 0 {
                        log::info!("heap free {free} B, lowest {min} B, uptime {} s", now_ms / 1000);
                    }
                }
                // Look for new firmware a minute after the network is up, then every 6 hours.
                if wifi_up && !ota_running {
                    let due = match update_checked_at {
                        None => now_ms > 60_000,
                        Some(t) => now_ms.saturating_sub(t) > 6 * 3600 * 1000,
                    };
                    if due {
                        update_checked_at = Some(now_ms);
                        spawn_update_check(tx.clone());
                    }
                }
                // The image proved itself: an HA client got through, or we ran 5 minutes.
                if !image_confirmed && (ha_clients > 0 || tick_count > 300) {
                    image_confirmed = true;
                    ota::mark_valid();
                }
                // Portal policy: open after a long outage, close after a while once back online.
                if !ap_active && creds_exist && matches!(sta_down_since, Some(t) if t.elapsed() > PORTAL_AFTER_OFFLINE) {
                    log::warn!("station down for 10 min: opening setup access point");
                    buzzer.play(Tone::PortalOpen);
                    ap_active = true;
                    portal_since = Some(Instant::now());
                    portal_ctx.ap_active.store(true, Ordering::Relaxed);
                    let _ = wifi_tx.send(WifiCmd::StartAp);
                    led.set(LED_PORTAL);
                }
                if ap_active && creds_exist && wifi_up && matches!(portal_since, Some(t) if t.elapsed() > PORTAL_IDLE) {
                    log::info!("closing setup access point");
                    ap_active = false;
                    portal_since = None;
                    portal_ctx.ap_active.store(false, Ordering::Relaxed);
                    let _ = wifi_tx.send(WifiCmd::StopAp);
                    led.set(if ha_clients > 0 { LED_ONLINE } else { LED_WIFI_NO_HA });
                }
                if matches!(restart_at, Some(t) if Instant::now() >= t) {
                    log::warn!("restarting to apply new settings");
                    thread::sleep(Duration::from_millis(200));
                    unsafe { esp_idf_svc::sys::esp_restart() };
                }
            }
            Msg::WifiUp(ip) => {
                wifi_up = true;
                sta_down_since = None;
                if mdns.is_none() {
                    match start_mdns(&server) {
                        Ok(m) => mdns = Some(m),
                        Err(e) => log::error!("mdns: {e}"),
                    }
                }
                log::info!("api reachable at {ip}:{API_PORT}, setup page at http://{ip}/");
                if !ap_active {
                    led.set(if ha_clients > 0 { LED_ONLINE } else { LED_WIFI_NO_HA });
                }
            }
            Msg::WifiDown => {
                if wifi_up || sta_down_since.is_none() {
                    sta_down_since = Some(Instant::now());
                }
                wifi_up = false;
                if !ap_active {
                    led.set(LED_NO_WIFI);
                }
            }
            Msg::LongPress => {
                ap_active = !ap_active;
                portal_ctx.ap_active.store(ap_active, Ordering::Relaxed);
                buzzer.play(if ap_active { Tone::PortalOpen } else { Tone::PortalClose });
                if ap_active {
                    log::warn!("button: opening setup access point {node_name} (password {})", wifi::AP_PASSWORD);
                    portal_since = Some(Instant::now());
                    let _ = wifi_tx.send(WifiCmd::StartAp);
                    led.set(LED_PORTAL);
                } else {
                    log::info!("button: closing setup access point");
                    portal_since = None;
                    let _ = wifi_tx.send(WifiCmd::StopAp);
                    led.set(if wifi_up { if ha_clients > 0 { LED_ONLINE } else { LED_WIFI_NO_HA } } else { LED_NO_WIFI });
                }
            }
            Msg::FactoryReset => {
                log::warn!("button held 15 s: FACTORY RESET");
                buzzer.play(Tone::FactoryReset);
                for _ in 0..6 {
                    led.set(RGB8 { r: 40, g: 0, b: 0 });
                    thread::sleep(Duration::from_millis(120));
                    led.set(RGB8 { r: 30, g: 30, b: 30 });
                    thread::sleep(Duration::from_millis(120));
                }
                if let Err(e) = store.factory_reset() {
                    log::error!("factory reset: {e}");
                }
                unsafe { esp_idf_svc::sys::esp_restart() };
            }
            Msg::PortalScan => {
                if last_scan.elapsed() > Duration::from_secs(10) {
                    last_scan = Instant::now();
                    let _ = wifi_tx.send(WifiCmd::Scan);
                }
            }
            Msg::Provision(new) => {
                let old = portal_ctx.current.lock().unwrap().clone();
                log::info!("provisioning: ssid {:?}, name {:?}, key {}", new.ssid, new.name, if new.psk_b64 == old.psk_b64 { "unchanged" } else { "changed" });
                if let Err(e) = store.save_net(&new).and_then(|_| store.mark_seeded()) {
                    log::error!("save settings: {e}");
                }
                let _ = wifi_tx.send(WifiCmd::SetStation { ssid: new.ssid.clone(), pass: new.pass.clone() });
                sta_down_since = Some(Instant::now());
                if new.name != old.name || new.psk_b64 != old.psk_b64 || new.friendly_name != old.friendly_name {
                    restart_at = Some(Instant::now() + Duration::from_secs(25));
                }
                *portal_ctx.current.lock().unwrap() = new;
            }
            Msg::Rssi(v) => server.publish_state(keys.rssi, State::Float(v as f32)),
            Msg::UpdateInfo(res) => {
                match res {
                    Ok(l) => {
                        let newer = updater::is_newer(&l.version, FW_VERSION);
                        log::info!("update check: latest {} ({}), running {FW_VERSION}", l.version, if newer { "newer" } else { "not newer" });
                        update_state.latest_version = if newer { l.version.clone() } else { FW_VERSION.into() };
                        update_state.release_summary = if newer { l.summary.clone() } else { String::new() };
                        update_state.release_url = l.release_url.clone();
                        latest = Some(l);
                    }
                    Err(e) => log::warn!("update check failed: {e}"),
                }
                server.set_state(keys.update, State::Update(update_state.clone()));
            }
            Msg::OtaStatus(text) => {
                log::info!("ota: {text}");
                // Mirror progress into the update entity.
                if let Some(pct) = text.strip_prefix("downloading ").and_then(|r| r.strip_suffix('%')).and_then(|p| p.parse::<f32>().ok()) {
                    update_state.in_progress = true;
                    update_state.has_progress = true;
                    update_state.progress = pct;
                } else if text.starts_with("downloading") {
                    update_state.in_progress = true;
                    update_state.has_progress = false;
                } else if text.starts_with("installed") {
                    update_state.progress = 100.0;
                } else if text.starts_with("failed") || text.starts_with("rejected") {
                    update_state.in_progress = false;
                    update_state.has_progress = false;
                    update_state.progress = 0.0;
                }
                server.set_state(keys.update, State::Update(update_state.clone()));
                if text.starts_with("installed") {
                    buzzer.play(Tone::OtaDone);
                } else if text.starts_with("failed") || text.starts_with("rejected") {
                    // A failed attempt must not block the next one.
                    ota_running = false;
                    buzzer.play(Tone::Error);
                }
                server.set_state(keys.ota_status, State::Text(text));
            }
            Msg::Api(cmd) => match cmd {
                Command::ClientSubscribed { client_info } => {
                    ha_clients += 1;
                    log::info!("HA client subscribed: {client_info} ({ha_clients} total)");
                    if ha_clients == 1 {
                        actions.extend(ctl.on_ha_connected(now_ms));
                    }
                    if !ap_active {
                        led.set(LED_ONLINE);
                    }
                }
                Command::ClientGone { subscribed } => {
                    if subscribed {
                        ha_clients = ha_clients.saturating_sub(1);
                        if ha_clients == 0 {
                            log::warn!("last HA client gone");
                            actions.extend(ctl.on_ha_disconnected(now_ms, local));
                            if !ap_active {
                                led.set(if wifi_up { LED_WIFI_NO_HA } else { LED_NO_WIFI });
                            }
                        }
                    }
                }
                Command::Switch { key, on } if key == keys.buzzer => {
                    ctl.set_buzzer(on);
                    buzzer.set_enabled(on);
                    if let Err(e) = store.save_config(ctl.config()) {
                        log::error!("save config: {e}");
                    }
                    server.set_state(key, State::Bool(on));
                    if on {
                        buzzer.play(Tone::Boot);
                    }
                }
                Command::Switch { key, on } if keys.safe_state_channel(key).is_some() => {
                    let ch = keys.safe_state_channel(key).unwrap();
                    log::info!("relay {} safe state (clock unknown) = {}", ch + 1, if on { "ON" } else { "OFF" });
                    actions.extend(ctl.set_safe_state(ch, on, now_ms));
                    if let Err(e) = store.save_config(ctl.config()) {
                        log::error!("save config: {e}");
                    }
                    server.set_state(key, State::Bool(on));
                }
                Command::Switch { key, on } if key == keys.exclusive => {
                    log::info!("exclusive mode {}", if on { "ON" } else { "OFF" });
                    actions.extend(ctl.set_exclusive(on, now_ms));
                    if let Err(e) = store.save_config(ctl.config()) {
                        log::error!("save config: {e}");
                    }
                    server.set_state(key, State::Bool(on));
                }
                Command::Switch { key, on } => {
                    if let Some(ch) = keys.relay_channel(key) {
                        actions.extend(ctl.on_ha_command(ch, on, now_ms));
                        // Echo the real state so HA never shows a phantom toggle.
                        server.publish_state(key, State::Bool(ctl.relays()[ch]));
                    }
                }
                Command::Number { key, value } if key == keys.rain_hold_hours => {
                    let hours = value.clamp(0.0, 72.0).round() as u32;
                    if hours > 0 && !clock.time_known() {
                        log::warn!("rain hold requested but the clock is not set yet");
                    }
                    actions.extend(ctl.set_rain_hold(hours, now_ms, local));
                    if let Err(e) = store.save_config(ctl.config()) {
                        log::error!("save config: {e}");
                    }
                    log::info!("rain hold {}", if hours > 0 { format!("{hours} h") } else { "cleared".into() });
                    publish_rain_hold(&server, &keys, &ctl, &clock, Some(hours));
                }
                Command::Number { key, value } if keys.max_daily_channel(key).is_some() => {
                    let ch = keys.max_daily_channel(key).unwrap();
                    let minutes = value.clamp(0.0, 1440.0).round() as u16;
                    actions.extend(ctl.set_max_daily_min(ch, minutes, now_ms));
                    if let Err(e) = store.save_config(ctl.config()) {
                        log::error!("save config: {e}");
                    }
                    server.set_state(key, State::Float(minutes as f32));
                    server.set_state(keys.daily_capped[ch], State::Bool(ctl.daily_capped()[ch]));
                    log::info!("relay {} max per day = {} min", ch + 1, minutes);
                }
                Command::Number { key, value } => {
                    if let Some(ch) = keys.max_on_channel(key) {
                        let minutes = value.clamp(0.0, 1440.0).round() as u16;
                        actions.extend(ctl.set_max_on_min(ch, minutes, now_ms));
                        if let Err(e) = store.save_config(ctl.config()) {
                            log::error!("save config: {e}");
                        }
                        server.set_state(key, State::Float(minutes as f32));
                        log::info!("relay {} max on time = {} min", ch + 1, minutes);
                    }
                }
                Command::Select { key, option } if key == keys.mode => {
                    if let Some(mode) = entities::mode_from_option(&option) {
                        log::info!("mode -> {}", mode.as_str());
                        actions.extend(ctl.set_mode(mode, now_ms, local));
                        if let Err(e) = store.save_config(ctl.config()) {
                            log::error!("save config: {e}");
                        }
                        server.set_state(key, State::Text(entities::mode_label(mode).into()));
                    } else {
                        log::warn!("unknown mode option {option:?}");
                    }
                }
                Command::Select { .. } => {}
                Command::Button { key } if key == keys.all_off => actions.extend(ctl.all_off(now_ms)),
                Command::Button { key } if key == keys.clear_crash => {
                    crash::clear();
                    server.set_state(keys.last_crash, State::Text("none".into()));
                }
                Command::Button { key } if key == keys.restart => {
                    log::warn!("restart requested from HA");
                    thread::sleep(Duration::from_millis(200));
                    unsafe { esp_idf_svc::sys::esp_restart() };
                }
                Command::Button { .. } => {}
                Command::Service { key, args } if key == keys.set_schedule => {
                    let json = args.first().map(|a| a.string.clone()).unwrap_or_default();
                    match Schedule::from_json(&json) {
                        Ok(s) => {
                            log::info!("schedule rev {} with {} blocks received", s.rev, s.block_count());
                            if !s.tz.is_empty() && clock.set_tz(&s.tz) {
                                let _ = store.save_tz(&s.tz);
                            }
                            if let Err(e) = store.save_schedule(&s) {
                                log::error!("save schedule: {e}");
                            }
                            actions.extend(ctl.set_schedule(s, now_ms, clock.local_time()));
                            publish_schedule(&server, &keys, ctl.schedule(), None);
                        }
                        Err(e) => {
                            log::error!("rejected schedule: {e}");
                            publish_schedule(&server, &keys, ctl.schedule(), Some(&e));
                        }
                    }
                }
                Command::Service { key, .. } if key == keys.clear_schedule => {
                    let _ = store.save_schedule(&Schedule::default());
                    actions.extend(ctl.set_schedule(Schedule::default(), now_ms, local));
                    publish_schedule(&server, &keys, ctl.schedule(), None);
                }
                Command::Service { key, args } if key == keys.ota => {
                    let url = args.first().map(|a| a.string.trim().to_string()).unwrap_or_default();
                    if ota_running {
                        log::warn!("ota already running");
                    } else if !url.starts_with("http") {
                        let _ = tx.send(Msg::OtaStatus(format!("rejected url {url:?}")));
                    } else {
                        ota_running = true;
                        spawn_ota(url, tx.clone());
                    }
                }
                Command::Service { key, .. } if key == keys.debug_crash => {
                    log::warn!("debug_crash requested: panicking on purpose");
                    thread::sleep(Duration::from_millis(300));
                    panic!("debug_crash requested from Home Assistant");
                }
                Command::Service { .. } => {}
                Command::Update { key, command } if key == keys.update => match command {
                    2 => {
                        log::info!("update check requested from HA");
                        update_checked_at = Some(now_ms);
                        spawn_update_check(tx.clone());
                    }
                    1 => match &latest {
                        Some(l) if updater::is_newer(&l.version, FW_VERSION) && !ota_running => {
                            log::warn!("installing firmware {} from {}", l.version, l.url);
                            ota_running = true;
                            spawn_ota(l.url.clone(), tx.clone());
                        }
                        Some(_) => log::info!("install requested but no newer version is known"),
                        None => log::warn!("install requested before any update check"),
                    },
                    _ => {}
                },
                Command::Update { .. } => {}
                Command::Time { epoch_seconds, timezone } => {
                    clock.set_epoch(epoch_seconds);
                    if clock.set_tz(&timezone) {
                        let _ = store.save_tz(&timezone);
                    }
                    server.set_state(keys.local_time, State::Text(clock.local_string()));
                    if !time_known {
                        time_known = true;
                        if let Some(l) = clock.local_time() {
                            actions.extend(ctl.on_time_known(now_ms, l));
                        }
                    }
                }
            },
        }

        let any_on = ctl.relays().iter().any(|r| *r);
        let periodic = matches!(msg_kind, MsgKind::Tick) && any_on && tick_count % 300 == 0;
        let relay_off = actions.iter().any(|a| matches!(a, Action::SetRelay { on: false, .. } | Action::DayRolled | Action::DailyCapReached { .. }));
        if periodic || relay_off {
            if let Err(e) = store.save_usage(&ctl.usage()) {
                log::error!("save usage: {e}");
            }
        }
        if actions.iter().any(|a| matches!(a, Action::RainHoldEnded)) {
            log::info!("rain hold ended; schedule resumes");
            if let Err(e) = store.save_config(ctl.config()) {
                log::error!("save config: {e}");
            }
            publish_rain_hold(&server, &keys, &ctl, &clock, Some(0));
        }
        apply(&mut relays, &server, &keys, &mut tripped, &buzzer, actions);
    }
    Ok(())
}

/// Publish the rain-hold number (hours, as set) and its human-readable end time.
fn publish_rain_hold(server: &Server, keys: &Keys, ctl: &Controller, clock: &Clock, hours: Option<u32>) {
    let text = match ctl.rain_hold_until() {
        None => "off".to_string(),
        Some(until) => match clock.epoch() {
            Some(now) if until > now => {
                let left = until - now;
                format!("{} ({}h{:02}m left)", clock.format_epoch(until), left / 3600, (left % 3600) / 60)
            }
            Some(_) => "expiring".to_string(),
            None => "until the clock is known".to_string(),
        },
    };
    server.set_state(keys.rain_hold, State::Text(text));
    if let Some(h) = hours {
        server.set_state(keys.rain_hold_hours, State::Float(h as f32));
    }
}

fn apply(relays: &mut [Relay], server: &Server, keys: &Keys, tripped: &mut [bool; CHANNELS], buzzer: &Buzzer, actions: Vec<Action>) {
    for a in actions {
        match a {
            Action::SetRelay { ch, on } => {
                let res = if on { relays[ch].set_high() } else { relays[ch].set_low() };
                if let Err(e) = res {
                    log::error!("relay {}: {e}", ch + 1);
                }
                log::info!("relay {} -> {}", ch + 1, if on { "ON" } else { "OFF" });
                server.set_state(keys.relay[ch], State::Bool(on));
                if on && tripped[ch] {
                    tripped[ch] = false;
                    server.set_state(keys.tripped[ch], State::Bool(false));
                }
            }
            Action::LinkChanged(link) => {
                log::info!("link: {}", link.as_str());
                server.set_state(keys.link, State::Text(link.as_str().into()));
            }
            Action::SafeguardTripped { ch } => {
                log::warn!("safeguard tripped on relay {}", ch + 1);
                buzzer.play(Tone::Safeguard);
                tripped[ch] = true;
                server.set_state(keys.tripped[ch], State::Bool(true));
            }
            Action::ScheduleWants { ch, on } => log::debug!("schedule wants relay {} {}", ch + 1, on),
            Action::RainHoldEnded => {}
            Action::DailyUsage { ch, minutes, total_minutes } => {
                server.set_state(keys.on_today[ch], State::Float(minutes as f32));
                server.set_state(keys.total[ch], State::Float(total_minutes as f32));
            }
            Action::DailyCapReached { ch } => {
                log::warn!("relay {} reached its daily limit; blocked until midnight", ch + 1);
                buzzer.play(Tone::Safeguard);
                server.set_state(keys.daily_capped[ch], State::Bool(true));
            }
            Action::DayRolled => {
                log::info!("new day: daily on-time counters reset");
                for ch in 0..CHANNELS {
                    server.set_state(keys.daily_capped[ch], State::Bool(false));
                }
            }
        }
    }
}

fn publish_all(server: &Server, keys: &Keys, ctl: &Controller, clock: &Clock, link: Option<Link>) {
    let cfg = ctl.config();
    for ch in 0..CHANNELS {
        server.set_state(keys.relay[ch], State::Bool(ctl.relays()[ch]));
        server.set_state(keys.max_on[ch], State::Float(cfg.channels[ch].max_on_min as f32));
        server.set_state(keys.tripped[ch], State::Bool(false));
        server.set_state(keys.safe_state[ch], State::Bool(cfg.channels[ch].safe_state));
        server.set_state(keys.max_daily[ch], State::Float(cfg.channels[ch].max_daily_min as f32));
        server.set_state(keys.on_today[ch], State::Float(ctl.on_today_min()[ch] as f32));
        server.set_state(keys.total[ch], State::Float(ctl.total_min()[ch] as f32));
        server.set_state(keys.daily_capped[ch], State::Bool(ctl.daily_capped()[ch]));
    }
    server.set_state(keys.mode, State::Text(entities::mode_label(cfg.mode).into()));
    server.set_state(keys.exclusive, State::Bool(cfg.exclusive));
    server.set_state(keys.buzzer, State::Bool(cfg.buzzer));
    server.set_state(keys.link, State::Text(link.unwrap_or(ctl.link()).as_str().into()));
    server.set_state(keys.local_time, State::Text(clock.local_string()));
    server.set_state(keys.uptime, State::Float(0.0));
    server.set_state(keys.reset_reason, State::Text(reset_reason()));
    server.set_state(keys.last_crash, State::Text(crash::last_report()));
    // Number shows remaining hours (rounded up) so the UI reflects a hold restored from flash.
    let hours = ctl.rain_hold_until().zip(clock.epoch()).map(|(u, n)| if u > n { ((u - n) + 3599) / 3600 } else { 0 }).unwrap_or(0) as u32;
    publish_rain_hold(server, keys, ctl, clock, Some(hours));
    publish_heap(server, keys);
    publish_schedule(server, keys, ctl.schedule(), None);
}

/// Why the chip last (re)started, as ESP-IDF reports it.
#[allow(non_upper_case_globals)]
fn reset_reason() -> String {
    use esp_idf_svc::sys::*;
    let r = unsafe { esp_reset_reason() };
    match r {
        esp_reset_reason_t_ESP_RST_POWERON => "power on".into(),
        esp_reset_reason_t_ESP_RST_EXT => "external reset pin".into(),
        esp_reset_reason_t_ESP_RST_SW => "software restart (OTA, button, HA)".into(),
        esp_reset_reason_t_ESP_RST_PANIC => "PANIC (crash)".into(),
        esp_reset_reason_t_ESP_RST_INT_WDT => "interrupt watchdog".into(),
        esp_reset_reason_t_ESP_RST_TASK_WDT => "TASK WATCHDOG (main loop stalled)".into(),
        esp_reset_reason_t_ESP_RST_WDT => "other watchdog".into(),
        esp_reset_reason_t_ESP_RST_DEEPSLEEP => "deep sleep wake".into(),
        esp_reset_reason_t_ESP_RST_BROWNOUT => "BROWNOUT (power supply)".into(),
        esp_reset_reason_t_ESP_RST_SDIO => "sdio".into(),
        esp_reset_reason_t_ESP_RST_USB => "USB reset".into(),
        esp_reset_reason_t_ESP_RST_JTAG => "JTAG reset".into(),
        esp_reset_reason_t_ESP_RST_EFUSE => "efuse error".into(),
        esp_reset_reason_t_ESP_RST_PWR_GLITCH => "POWER GLITCH".into(),
        esp_reset_reason_t_ESP_RST_CPU_LOCKUP => "CPU LOCKUP".into(),
        other => format!("unknown ({other})"),
    }
}

fn publish_heap(server: &Server, keys: &Keys) -> (u32, u32) {
    let (free, min) = unsafe { (esp_idf_svc::sys::esp_get_free_heap_size(), esp_idf_svc::sys::esp_get_minimum_free_heap_size()) };
    server.set_state(keys.heap, State::Float(free as f32));
    server.set_state(keys.heap_min, State::Float(min as f32));
    (free, min)
}

fn publish_schedule(server: &Server, keys: &Keys, s: &Schedule, error: Option<&str>) {
    server.set_state(keys.schedule_loaded, State::Bool(s.block_count() > 0));
    server.set_state(keys.schedule_rev, State::Float(s.rev as f32));
    let info = match error {
        Some(e) => format!("rejected: {e}"),
        None if s.block_count() == 0 => "none".to_string(),
        None => format!("rev {}, {} blocks, tz {}", s.rev, s.block_count(), if s.tz.is_empty() { "-" } else { &s.tz }),
    };
    server.set_state(keys.schedule_info, State::Text(info));
}

fn start_mdns(server: &Server) -> Result<EspMdns> {
    let dev = server.device();
    let mut mdns = EspMdns::take()?;
    mdns.set_hostname(&dev.name)?;
    mdns.set_instance_name(&dev.name)?;
    let txt = dev.mdns_txt();
    let pairs: Vec<(&str, &str)> = txt.iter().map(|(k, v)| (*k, v.as_str())).collect();
    mdns.add_service(Some(&dev.name), "_esphomelib", "_tcp", API_PORT, &pairs)?;
    log::info!("mdns: {}.local advertising _esphomelib._tcp:{API_PORT}", dev.name);
    Ok(mdns)
}

fn spawn_update_check(tx: Sender<Msg>) {
    let res = thread::Builder::new().name("upd-check".into()).stack_size(12 * 1024).spawn(move || {
        let r = updater::fetch_latest(updater::LATEST_URL).map_err(|e| format!("{e:#}"));
        let _ = tx.send(Msg::UpdateInfo(r));
    });
    if let Err(e) = res {
        log::error!("could not spawn update check: {e}");
    }
}

fn spawn_ota(url: String, tx: Sender<Msg>) {
    let _ = tx.send(Msg::OtaStatus(format!("downloading {url}")));
    let err_tx = tx.clone();
    let res = thread::Builder::new().name("ota".into()).stack_size(16 * 1024).spawn(move || {
        let mut last_pct = 0usize;
        let progress_tx = tx.clone();
        let result = ota::install_from_url(&url, |done, total| {
            if let Some(t) = total {
                let pct = done * 100 / t.max(1);
                if pct >= last_pct + 10 {
                    last_pct = pct;
                    let _ = progress_tx.send(Msg::OtaStatus(format!("downloading {pct}%")));
                }
            }
        });
        match result {
            Ok(bytes) => {
                let _ = tx.send(Msg::OtaStatus(format!("installed {bytes} bytes, rebooting")));
                thread::sleep(Duration::from_secs(2));
                unsafe { esp_idf_svc::sys::esp_restart() };
            }
            Err(e) => {
                let _ = tx.send(Msg::OtaStatus(format!("failed: {e:#}")));
            }
        }
    });
    if let Err(e) = res {
        let _ = err_tx.send(Msg::OtaStatus(format!("failed to start: {e}")));
    }
}

/// 1 s ticks for the main loop, plus the BOOT button: held 5 s -> `LongPress` (setup AP),
/// kept held to 15 s -> `FactoryReset`. Each fires once per press.
fn spawn_ticker(tx: Sender<Msg>, button: PinDriver<'static, Input>) {
    thread::Builder::new()
        .name("tick".into())
        .stack_size(3072)
        .spawn(move || {
            let mut held_ms = 0u32;
            let mut fired_long = false;
            let mut fired_reset = false;
            let mut n = 0u32;
            loop {
                thread::sleep(Duration::from_millis(100));
                if button.is_low() {
                    held_ms += 100;
                    if held_ms >= LONG_PRESS.as_millis() as u32 && !fired_long {
                        fired_long = true;
                        let _ = tx.send(Msg::LongPress);
                    }
                    if held_ms >= RESET_PRESS.as_millis() as u32 && !fired_reset {
                        fired_reset = true;
                        let _ = tx.send(Msg::FactoryReset);
                    }
                } else {
                    held_ms = 0;
                    fired_long = false;
                    fired_reset = false;
                }
                n += 1;
                if n % 10 == 0 && tx.send(Msg::Tick).is_err() {
                    break;
                }
            }
        })
        .expect("tick thread");
}

fn spawn_api_listener(server: Server, psk: Option<[u8; 32]>, tx: Sender<Msg>) -> Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", API_PORT))?;
    let active = Arc::new(AtomicUsize::new(0));
    thread::Builder::new().name("api-accept".into()).stack_size(4 * 1024).spawn(move || {
        for stream in listener.incoming() {
            let stream = match stream {
                Ok(s) => s,
                Err(e) => {
                    log::warn!("accept: {e}");
                    thread::sleep(Duration::from_millis(500));
                    continue;
                }
            };
            if active.load(Ordering::SeqCst) >= MAX_CLIENTS {
                log::warn!("too many API clients, refusing");
                drop(stream);
                continue;
            }
            let codec = match psk {
                Some(k) => Codec::Noise(Box::new(NoiseCodec::new(k, &server.device().name, &server.device().mac_compact()))),
                None => Codec::Plaintext,
            };
            let server = server.clone();
            let tx = tx.clone();
            let active2 = active.clone();
            active.fetch_add(1, Ordering::SeqCst);
            let spawned = thread::Builder::new().name("api-conn".into()).stack_size(24 * 1024).spawn(move || {
                if let Err(e) = runner::serve(stream, &server, codec, &tx) {
                    log::debug!("api connection ended: {e}");
                }
                active2.fetch_sub(1, Ordering::SeqCst);
            });
            if spawned.is_err() {
                active.fetch_sub(1, Ordering::SeqCst);
                log::error!("could not spawn connection thread");
            }
        }
    })?;
    Ok(())
}
