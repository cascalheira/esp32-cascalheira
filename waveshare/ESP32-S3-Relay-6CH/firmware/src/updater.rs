//! Firmware update discovery: fetch `latest.json` from the project repository and compare with
//! the running version. Installing reuses the OTA downloader.

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use esp_idf_svc::http::client::{Configuration, EspHttpConnection};
use esp_idf_svc::http::Method;
use serde::Deserialize;

pub const LATEST_URL: &str =
    "https://raw.githubusercontent.com/cascalheira/esp32-cascalheira/main/waveshare/ESP32-S3-Relay-6CH/firmware/release/latest.json";

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Latest {
    pub version: String,
    pub url: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub release_url: String,
}

pub fn fetch_latest(url: &str) -> Result<Latest> {
    let conf = Configuration {
        timeout: Some(Duration::from_secs(20)),
        buffer_size: Some(2048),
        crt_bundle_attach: Some(esp_idf_svc::sys::esp_crt_bundle_attach),
        ..Default::default()
    };
    let mut http = EspHttpConnection::new(&conf).context("http client")?;
    http.initiate_request(Method::Get, url, &[("Accept", "application/json")]).context("request")?;
    http.initiate_response().context("response")?;
    if http.status() != 200 {
        return Err(anyhow!("http status {}", http.status()));
    }
    let mut body = Vec::with_capacity(1024);
    let mut buf = [0u8; 512];
    loop {
        let n = http.read(&mut buf).context("read")?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&buf[..n]);
        if body.len() > 8192 {
            return Err(anyhow!("latest.json too large"));
        }
    }
    serde_json::from_slice(&body).context("parse latest.json")
}

fn parse_version(v: &str) -> Option<(u32, u32, u32)> {
    let mut it = v.trim().trim_start_matches('v').split('.').map(|p| p.split('-').next().unwrap_or("0").parse::<u32>());
    Some((it.next()?.ok()?, it.next()?.ok()?, it.next().unwrap_or(Ok(0)).ok()?))
}

/// True when `latest` is strictly newer than `current` (semantic version compare).
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}
