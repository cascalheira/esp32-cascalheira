//! Provisioning portal: HTTP setup page (works on the AP and on the LAN) plus a captive DNS
//! responder so phones open the page automatically when joined to the device's AP.

use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Result};
use embedded_svc::http::Headers;
use embedded_svc::io::{Read, Write};
use esp_idf_svc::handle::RawHandle;
use esp_idf_svc::http::server::{Configuration as HttpConfig, EspHttpConnection, EspHttpServer};
use esp_idf_svc::http::Method;
use esp_idf_svc::sys;
use serde::{Deserialize, Serialize};

use crate::settings::NetSettings;
use crate::wifi::{Shared, AP_IP};
use crate::Msg;

const PAGE: &str = include_str!("portal.html");
const REDIRECT_PATHS: &[&str] = &[
    "/generate_204", "/gen_204", "/hotspot-detect.html", "/library/test/success.html",
    "/connecttest.txt", "/ncsi.txt", "/redirect", "/success.txt", "/canonical.html", "/check_network_status.txt",
];

#[derive(Serialize)]
struct Status<'a> {
    name: &'a str,
    version: &'a str,
    sta_connected: bool,
    sta_ip: &'a str,
    sta_ssid: &'a str,
    ap_active: bool,
    last_error: &'a str,
    has_key: bool,
    /// The API key, only disclosed to clients connected through the setup access point.
    api_key: Option<&'a str>,
    via_ap: bool,
}

#[derive(Deserialize)]
struct SaveRequest {
    ssid: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    friendly_name: String,
    #[serde(default)]
    api_key: String,
    #[serde(default)]
    current_key: String,
}

pub struct PortalContext {
    pub node_name: String,
    pub version: String,
    pub wifi: Shared,
    pub current: std::sync::Mutex<NetSettings>,
    pub tx: Sender<Msg>,
    pub ap_active: Arc<AtomicBool>,
}

/// True when the request came in over the setup access point (client in 192.168.4.0/24).
/// Uses the socket's peer address, which unlike the Host header cannot be spoofed from the LAN.
/// lwIP may report the peer as IPv4 or as an IPv4-mapped IPv6 address; both are handled.
fn via_ap(conn: &EspHttpConnection<'_>) -> bool {
    peer_ipv4(conn).map(|ip| ip[..3] == AP_IP.octets()[..3]).unwrap_or(false)
}

fn peer_ipv4(conn: &EspHttpConnection<'_>) -> Option<[u8; 4]> {
    // Big enough for sockaddr_in6; parsed by hand to be independent of struct layouts.
    let mut raw = [0u8; 32];
    let mut len = raw.len() as sys::socklen_t;
    let fd = unsafe { sys::httpd_req_to_sockfd(conn.handle()) };
    if fd < 0 {
        return None;
    }
    let rc = unsafe { sys::lwip_getpeername(fd, raw.as_mut_ptr() as *mut sys::sockaddr, &mut len) };
    if rc != 0 {
        log::debug!("portal: getpeername failed ({rc})");
        return None;
    }
    // Byte 0 = sa_len, byte 1 = sa_family (lwIP layout).
    let family = raw[1] as u32;
    let ip = if family == sys::AF_INET {
        [raw[4], raw[5], raw[6], raw[7]]
    } else if family == sys::AF_INET6 {
        // sin6_addr starts at offset 8; accept only IPv4-mapped (::ffff:a.b.c.d).
        if raw[8..18] != [0u8; 10] || raw[18..20] != [0xff, 0xff] {
            return None;
        }
        [raw[20], raw[21], raw[22], raw[23]]
    } else {
        log::debug!("portal: unknown address family {family}");
        return None;
    };
    log::debug!("portal: peer {}.{}.{}.{} (family {family})", ip[0], ip[1], ip[2], ip[3]);
    Some(ip)
}

pub fn start(ctx: Arc<PortalContext>) -> Result<EspHttpServer<'static>> {
    let conf = HttpConfig { max_uri_handlers: 16, stack_size: 12 * 1024, uri_match_wildcard: true, ..Default::default() };
    let mut server = EspHttpServer::new(&conf)?;

    server.fn_handler::<anyhow::Error, _>("/", Method::Get, |req| {
        req.into_response(200, Some("OK"), &[("Content-Type", "text/html; charset=utf-8"), ("Cache-Control", "no-store")])?
            .write_all(PAGE.as_bytes())?;
        Ok(())
    })?;

    let c = ctx.clone();
    server.fn_handler::<anyhow::Error, _>("/api/status", Method::Get, move |mut req| {
        let from_ap = via_ap(&**embedded_svc::http::server::Request::connection(&mut req));
        let body = {
            let w = c.wifi.lock().unwrap();
            let cur = c.current.lock().unwrap();
            let show_key = from_ap && c.ap_active.load(Ordering::Relaxed) && !cur.psk_b64.is_empty();
            serde_json::to_string(&Status {
                name: &c.node_name,
                version: &c.version,
                sta_connected: w.sta_connected,
                sta_ip: &w.sta_ip,
                sta_ssid: &w.sta_ssid,
                ap_active: w.ap_active,
                last_error: &w.last_error,
                has_key: !cur.psk_b64.is_empty(),
                api_key: show_key.then_some(cur.psk_b64.as_str()),
                via_ap: from_ap,
            })?
        };
        req.into_response(200, Some("OK"), &[("Content-Type", "application/json"), ("Cache-Control", "no-store")])?
            .write_all(body.as_bytes())?;
        Ok(())
    })?;

    let c = ctx.clone();
    server.fn_handler::<anyhow::Error, _>("/api/scan", Method::Get, move |req| {
        let _ = c.tx.send(Msg::PortalScan);
        let body = serde_json::to_string(&c.wifi.lock().unwrap().scan)?;
        req.into_response(200, Some("OK"), &[("Content-Type", "application/json"), ("Cache-Control", "no-store")])?
            .write_all(body.as_bytes())?;
        Ok(())
    })?;

    let c = ctx.clone();
    server.fn_handler::<anyhow::Error, _>("/api/save", Method::Post, move |mut req| {
        let from_ap = via_ap(&**embedded_svc::http::server::Request::connection(&mut req));
        let len = req.content_len().unwrap_or(0) as usize;
        if len == 0 || len > 4096 {
            req.into_response(400, Some("Bad Request"), &[])?.write_all(b"body too large or empty")?;
            return Ok(());
        }
        let mut body = vec![0u8; len];
        req.read_exact(&mut body).map_err(|e| anyhow!("read body: {e:?}"))?;
        let save: SaveRequest = match serde_json::from_slice(&body) {
            Ok(s) => s,
            Err(e) => {
                req.into_response(400, Some("Bad Request"), &[])?.write_all(format!("invalid json: {e}").as_bytes())?;
                return Ok(());
            }
        };
        let result = {
            let cur = c.current.lock().unwrap();
            // Over the LAN, changing settings requires the current API key. Through the setup
            // access point (physical button or factory reset) it is not required: whoever can
            // press the button owns the device, and the page shows the key there anyway.
            if !from_ap && !cur.psk_b64.is_empty() && save.current_key.trim() != cur.psk_b64 {
                Err("current API key does not match")
            } else if save.ssid.trim().is_empty() {
                Err("ssid is required")
            } else if !save.api_key.trim().is_empty() && esphome_api::noise::parse_psk(save.api_key.trim()).is_err() {
                Err("API key must be base64 of 32 bytes")
            } else {
                Ok(NetSettings {
                    ssid: save.ssid.trim().to_string(),
                    pass: save.password,
                    psk_b64: if save.api_key.trim().is_empty() { cur.psk_b64.clone() } else { save.api_key.trim().to_string() },
                    name: if save.name.trim().is_empty() { cur.name.clone() } else { sanitize_name(save.name.trim()) },
                    friendly_name: if save.friendly_name.trim().is_empty() { cur.friendly_name.clone() } else { save.friendly_name.trim().to_string() },
                })
            }
        };
        match result {
            Ok(settings) => {
                let _ = c.tx.send(Msg::Provision(settings));
                req.into_response(200, Some("OK"), &[("Content-Type", "application/json")])?
                    .write_all(br#"{"ok":true}"#)?;
            }
            Err(msg) => {
                req.into_response(400, Some("Bad Request"), &[("Content-Type", "application/json")])?
                    .write_all(format!(r#"{{"ok":false,"error":"{msg}"}}"#).as_bytes())?;
            }
        }
        Ok(())
    })?;

    // Captive-portal probes from phones/laptops and any unknown path: bounce to the page while
    // the AP is up, otherwise plain 404.
    let c = ctx.clone();
    server.fn_handler::<anyhow::Error, _>("/*", Method::Get, move |req| {
        let uri = req.uri().to_string();
        let host_is_us = req.header("Host").map(|h| h.starts_with(&AP_IP.to_string())).unwrap_or(false);
        let probe = REDIRECT_PATHS.iter().any(|p| uri.starts_with(p));
        if c.ap_active.load(Ordering::Relaxed) && (probe || !host_is_us) {
            let location = format!("http://{}/", AP_IP);
            req.into_response(302, Some("Found"), &[("Location", &location), ("Cache-Control", "no-store")])?
                .write_all(b"")?;
        } else {
            req.into_response(404, Some("Not Found"), &[])?.write_all(b"not found")?;
        }
        Ok(())
    })?;

    Ok(server)
}

/// Node names must be hostname-safe: lowercase letters, digits and dashes, max 24 chars.
pub fn sanitize_name(s: &str) -> String {
    let mut out: String = s
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    let out = out.trim_matches('-').chars().take(24).collect::<String>();
    if out.is_empty() { "relay6".into() } else { out }
}

/// Minimal DNS responder: answers every A query with the AP address while the AP is active.
pub fn spawn_captive_dns(ap_active: Arc<AtomicBool>) -> Result<()> {
    let sock = UdpSocket::bind(("0.0.0.0", 53))?;
    sock.set_read_timeout(Some(Duration::from_secs(1)))?;
    thread::Builder::new().name("dns".into()).stack_size(4 * 1024).spawn(move || {
        let mut buf = [0u8; 512];
        loop {
            let Ok((n, peer)) = sock.recv_from(&mut buf) else { continue };
            if !ap_active.load(Ordering::Relaxed) || n < 12 || !peer.ip().to_string().starts_with("192.168.4.") {
                continue;
            }
            let q = &buf[..n];
            // Walk the question name to find its end.
            let mut i = 12;
            while i < q.len() && q[i] != 0 {
                i += q[i] as usize + 1;
            }
            let qend = i + 5; // null label + type + class
            if qend > q.len() {
                continue;
            }
            let mut resp = Vec::with_capacity(qend + 16);
            resp.extend_from_slice(&q[..2]); // id
            resp.extend_from_slice(&[0x81, 0x80]); // standard response, recursion available
            resp.extend_from_slice(&[0, 1, 0, 1, 0, 0, 0, 0]); // 1 question, 1 answer
            resp.extend_from_slice(&q[12..qend]);
            resp.extend_from_slice(&[0xc0, 0x0c, 0, 1, 0, 1, 0, 0, 0, 30, 0, 4]);
            resp.extend_from_slice(&AP_IP.octets());
            let _ = sock.send_to(&resp, peer);
        }
    })?;
    Ok(())
}
