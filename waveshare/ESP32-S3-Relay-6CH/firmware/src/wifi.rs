//! WiFi manager thread: station with automatic reconnect, optional access point for the
//! provisioning portal, scans on request. Non-blocking driver calls, polled every 500 ms.

use std::net::Ipv4Addr;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::ipv4::{self, Mask, RouterConfiguration, Subnet};
use esp_idf_svc::netif::{EspNetif, NetifConfiguration};
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::wifi::{AccessPointConfiguration, AuthMethod, ClientConfiguration, Configuration, EspWifi, WifiDriver};
use serde::Serialize;

use crate::Msg;

pub const AP_IP: Ipv4Addr = Ipv4Addr::new(192, 168, 4, 1);
pub const AP_PASSWORD: &str = "relay6setup";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

pub enum WifiCmd {
    /// Use these station credentials from now on (empty ssid = station off).
    SetStation { ssid: String, pass: String },
    StartAp,
    StopAp,
    Scan,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApSummary {
    pub ssid: String,
    pub rssi: i8,
    pub channel: u8,
    pub secure: bool,
}

#[derive(Default)]
pub struct WifiState {
    pub scan: Vec<ApSummary>,
    pub sta_connected: bool,
    pub sta_ip: String,
    pub sta_ssid: String,
    pub ap_active: bool,
    pub last_error: String,
}

pub type Shared = Arc<Mutex<WifiState>>;

pub fn mac() -> [u8; 6] {
    let mut mac = [0u8; 6];
    unsafe {
        esp_idf_svc::sys::esp_read_mac(mac.as_mut_ptr(), esp_idf_svc::sys::esp_mac_type_t_ESP_MAC_WIFI_STA);
    }
    mac
}

pub fn spawn(
    modem: Modem<'static>,
    sysloop: EspSystemEventLoop,
    nvs: EspDefaultNvsPartition,
    hostname: String,
    ap_ssid: String,
    initial: WifiCmd,
    start_ap: bool,
    cmds: Receiver<WifiCmd>,
    tx: Sender<Msg>,
) -> Result<Shared> {
    let driver = WifiDriver::new(modem, sysloop, Some(nvs))?;
    let sta_conf = NetifConfiguration {
        ip_configuration: Some(ipv4::Configuration::Client(ipv4::ClientConfiguration::DHCP(ipv4::DHCPClientSettings {
            hostname: Some(hostname.as_str().try_into().map_err(|_| anyhow!("hostname too long"))?),
        }))),
        ..NetifConfiguration::wifi_default_client()
    };
    let ap_conf = NetifConfiguration {
        ip_configuration: Some(ipv4::Configuration::Router(RouterConfiguration {
            subnet: Subnet { gateway: AP_IP, mask: Mask(24) },
            dhcp_enabled: true,
            dns: Some(AP_IP), // captive portal: every DNS query lands on us
            secondary_dns: None,
        })),
        ..NetifConfiguration::wifi_default_router()
    };
    let sta = EspNetif::new_with_conf(&sta_conf)?;
    let ap = EspNetif::new_with_conf(&ap_conf)?;
    let mut wifi = EspWifi::wrap_all(driver, sta, ap)?;

    let shared: Shared = Arc::new(Mutex::new(WifiState::default()));
    let shared2 = shared.clone();
    let (mut ssid, mut pass) = match initial {
        WifiCmd::SetStation { ssid, pass } => (ssid, pass),
        _ => (String::new(), String::new()),
    };
    let mut ap_active = start_ap;

    thread::Builder::new().name("wifi".into()).stack_size(10 * 1024).spawn(move || {
        let mut connected = false;
        let mut connecting_since: Option<Instant> = None;
        let mut next_attempt = Instant::now();
        let mut backoff = Duration::from_secs(2);
        let mut rssi_tick = 0u32;
        let mut need_apply = true;
        let mut do_scan = true;

        loop {
            if need_apply {
                need_apply = false;
                connected = false;
                connecting_since = None;
                if let Err(e) = apply(&mut wifi, &ssid, &pass, ap_active, &ap_ssid) {
                    log::error!("wifi: apply config failed: {e}");
                    shared2.lock().unwrap().last_error = e.to_string();
                }
                {
                    let mut s = shared2.lock().unwrap();
                    s.ap_active = ap_active;
                    s.sta_ssid = ssid.clone();
                    s.sta_connected = false;
                    s.sta_ip.clear();
                }
                next_attempt = Instant::now();
                backoff = Duration::from_secs(2);
            }

            if do_scan {
                do_scan = false;
                match wifi.scan() {
                    Ok(aps) => {
                        let mut list: Vec<ApSummary> = aps
                            .iter()
                            .filter(|a| !a.ssid.is_empty())
                            .map(|a| ApSummary {
                                ssid: a.ssid.to_string(),
                                rssi: a.signal_strength,
                                channel: a.channel,
                                secure: !matches!(a.auth_method, Some(AuthMethod::None) | None),
                            })
                            .collect();
                        // Strongest instance of each SSID only, strongest first.
                        list.sort_by(|a, b| a.ssid.cmp(&b.ssid).then(b.rssi.cmp(&a.rssi)));
                        list.dedup_by(|a, b| a.ssid == b.ssid);
                        list.sort_by(|a, b| b.rssi.cmp(&a.rssi));
                        log::info!("wifi: scan found {} networks", list.len());
                        shared2.lock().unwrap().scan = list;
                    }
                    Err(e) => log::warn!("wifi: scan failed: {e}"),
                }
                // A scan drops an in-progress association; try again right away.
                if !ssid.is_empty() && !connected {
                    next_attempt = Instant::now();
                    connecting_since = None;
                }
            }

            // Station state machine.
            if !ssid.is_empty() {
                let is_conn = wifi.is_connected().unwrap_or(false);
                let is_up = is_conn && wifi.sta_netif().is_up().unwrap_or(false);
                if is_up && !connected {
                    connected = true;
                    connecting_since = None;
                    backoff = Duration::from_secs(2);
                    let ip = wifi.sta_netif().get_ip_info().map(|i| i.ip.to_string()).unwrap_or_default();
                    log::info!("wifi: connected to {ssid}, ip {ip}");
                    prefer_station_route(&wifi);
                    {
                        let mut s = shared2.lock().unwrap();
                        s.sta_connected = true;
                        s.sta_ip = ip.clone();
                        s.last_error.clear();
                    }
                    let _ = tx.send(Msg::WifiUp(ip));
                } else if !is_conn && connected {
                    connected = false;
                    log::warn!("wifi: connection lost");
                    {
                        let mut s = shared2.lock().unwrap();
                        s.sta_connected = false;
                        s.sta_ip.clear();
                    }
                    let _ = tx.send(Msg::WifiDown);
                    next_attempt = Instant::now() + Duration::from_secs(1);
                } else if !is_conn {
                    match connecting_since {
                        Some(t) if t.elapsed() > CONNECT_TIMEOUT => {
                            log::warn!("wifi: connect to {ssid} timed out; retry in {}s", backoff.as_secs());
                            shared2.lock().unwrap().last_error = format!("could not join {ssid} (wrong password?)");
                            let _ = wifi.disconnect();
                            connecting_since = None;
                            next_attempt = Instant::now() + backoff;
                            backoff = (backoff * 2).min(Duration::from_secs(60));
                            let _ = tx.send(Msg::WifiDown);
                        }
                        Some(_) => {}
                        None if Instant::now() >= next_attempt => {
                            log::info!("wifi: connecting to {ssid}");
                            match wifi.connect() {
                                Ok(()) => connecting_since = Some(Instant::now()),
                                Err(e) => {
                                    log::warn!("wifi: connect(): {e}");
                                    next_attempt = Instant::now() + backoff;
                                }
                            }
                        }
                        None => {}
                    }
                } else if connected {
                    rssi_tick += 1;
                    if rssi_tick % 60 == 0 {
                        if let Ok(r) = wifi.driver().get_rssi() {
                            let _ = tx.send(Msg::Rssi(r));
                        }
                    }
                }
            }

            match cmds.recv_timeout(Duration::from_millis(500)) {
                Ok(WifiCmd::SetStation { ssid: s, pass: p }) => {
                    ssid = s;
                    pass = p;
                    need_apply = true;
                }
                Ok(WifiCmd::StartAp) => {
                    if !ap_active {
                        ap_active = true;
                        need_apply = true;
                        do_scan = true;
                    }
                }
                Ok(WifiCmd::StopAp) => {
                    if ap_active {
                        ap_active = false;
                        need_apply = true;
                    }
                }
                Ok(WifiCmd::Scan) => do_scan = true,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
    })?;
    Ok(shared)
}

/// With station and AP both up, make sure outbound traffic (OTA, SNTP) leaves via the station.
fn prefer_station_route(wifi: &EspWifi<'static>) {
    use esp_idf_svc::handle::RawHandle;
    let h = wifi.sta_netif().handle();
    if !h.is_null() {
        unsafe {
            let before = esp_idf_svc::sys::esp_netif_get_default_netif();
            esp_idf_svc::sys::esp_netif_set_default_netif(h);
            if before != h {
                log::info!("wifi: default netif switched to station");
            }
        }
    }
}

fn apply(wifi: &mut EspWifi<'static>, ssid: &str, pass: &str, ap_active: bool, ap_ssid: &str) -> Result<()> {
    let client = || -> Result<ClientConfiguration> {
        Ok(ClientConfiguration {
            ssid: ssid.try_into().map_err(|_| anyhow!("ssid too long"))?,
            password: pass.try_into().map_err(|_| anyhow!("password too long"))?,
            auth_method: if pass.is_empty() { AuthMethod::None } else { AuthMethod::WPA2Personal },
            ..Default::default()
        })
    };
    let ap = || -> Result<AccessPointConfiguration> {
        Ok(AccessPointConfiguration {
            ssid: ap_ssid.try_into().map_err(|_| anyhow!("ap ssid too long"))?,
            password: AP_PASSWORD.try_into().unwrap(),
            auth_method: AuthMethod::WPA2Personal,
            channel: 1,
            max_connections: 4,
            ..Default::default()
        })
    };
    // The station side stays enabled together with the AP even without credentials: the
    // ESP32 can only scan for networks while station mode is on, and the setup page needs
    // that list. Without an SSID we simply never call connect().
    let conf = match (ssid.is_empty(), ap_active) {
        (false, false) => Configuration::Client(client()?),
        (_, true) => Configuration::Mixed(client()?, ap()?),
        (true, false) => Configuration::None,
    };
    if wifi.is_started().unwrap_or(false) {
        let _ = wifi.disconnect();
        wifi.stop()?;
    }
    wifi.set_configuration(&conf)?;
    if !matches!(conf, Configuration::None) {
        wifi.start()?;
    }
    log::info!("wifi: mode {} (station {:?}, ap {})", match conf {
        Configuration::Client(_) => "station",
        Configuration::Mixed(..) if ssid.is_empty() => "ap (station idle for scanning)",
        Configuration::Mixed(..) => "station+ap",
        Configuration::AccessPoint(_) => "ap only",
        Configuration::None => "off",
    }, ssid, ap_active);
    Ok(())
}
