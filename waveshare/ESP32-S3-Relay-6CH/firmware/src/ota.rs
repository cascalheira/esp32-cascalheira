//! Pull-style OTA: download an image over HTTP(S) and write it to the inactive app slot.

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use esp_idf_svc::http::client::{Configuration, EspHttpConnection};
use esp_idf_svc::http::Method;
use esp_idf_svc::ota::EspOta;

/// Download `url` and install it. On success the new image boots on the next restart.
/// `progress` receives bytes written so far.
pub fn install_from_url(url: &str, mut progress: impl FnMut(usize, Option<usize>)) -> Result<usize> {
    let conf = Configuration {
        timeout: Some(Duration::from_secs(30)),
        buffer_size: Some(4096),
        buffer_size_tx: Some(1024),
        crt_bundle_attach: Some(esp_idf_svc::sys::esp_crt_bundle_attach),
        ..Default::default()
    };
    let mut http = EspHttpConnection::new(&conf).context("http client")?;
    http.initiate_request(Method::Get, url, &[]).context("http request")?;
    http.initiate_response().context("http response")?;
    if http.status() != 200 {
        return Err(anyhow!("http status {}", http.status()));
    }
    let size = http.header("content-length").and_then(|v| v.trim().parse::<usize>().ok());

    let mut ota = EspOta::new().context("ota init")?;
    let mut update = match size {
        Some(n) if n > 0 => ota.initiate_update_with_known_size(n),
        _ => ota.initiate_update(),
    }
    .context("ota begin")?;

    let mut buf = vec![0u8; 4096];
    let mut total = 0usize;
    loop {
        let n = http.read(&mut buf).context("http read")?;
        if n == 0 {
            break;
        }
        if let Err(e) = update.write(&buf[..n]) {
            let _ = update.abort();
            return Err(anyhow!("ota write: {e}"));
        }
        total += n;
        progress(total, size);
    }
    if let Some(expected) = size {
        if total != expected {
            let _ = update.abort();
            return Err(anyhow!("short download: {total} of {expected} bytes"));
        }
    }
    update.complete().context("ota complete")?;
    Ok(total)
}

/// Confirm the running image so the bootloader stops considering a rollback.
pub fn mark_valid() {
    match EspOta::new().and_then(|mut o| o.mark_running_slot_valid()) {
        Ok(()) => log::info!("ota: running image marked valid"),
        Err(e) => log::debug!("ota: mark valid: {e}"),
    }
}

pub fn running_slot() -> String {
    EspOta::new()
        .and_then(|o| o.get_running_slot())
        .map(|s| format!("{} ({:?})", s.label, s.state))
        .unwrap_or_else(|e| format!("unknown ({e})"))
}
