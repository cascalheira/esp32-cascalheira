//! Crash reporting: read the core-dump summary left in flash by a previous crash, keep a short
//! human-readable report in NVS, and capture Rust panic messages before the chip aborts.

use std::sync::OnceLock;

use esp_idf_svc::nvs::{EspNvs, NvsDefault};
use esp_idf_svc::sys;

static NVS: OnceLock<EspNvs<NvsDefault>> = OnceLock::new();
const KEY_REPORT: &str = "crash";
const KEY_PANIC: &str = "panicmsg";

/// Install the panic hook and remember where to persist. Call once at boot.
pub fn install(nvs: EspNvs<NvsDefault>) {
    let _ = NVS.set(nvs);
    std::panic::set_hook(Box::new(|info| {
        let msg = format!("{info}");
        log::error!("PANIC: {msg}");
        if let Some(nvs) = NVS.get() {
            let short: String = msg.chars().take(200).collect();
            let _ = nvs.set_str(KEY_PANIC, &short);
        }
    }));
}

/// Summarise a core dump from the previous boot (if any), store it, erase the dump, return it.
/// Returns `None` when the last boot did not crash.
pub fn collect(uptime_hint: &str) -> Option<String> {
    let nvs = NVS.get()?;
    let mut buf = [0u8; 256];
    let panic_msg = nvs.get_str(KEY_PANIC, &mut buf).ok().flatten().map(str::to_string);

    let has_dump = unsafe { sys::esp_core_dump_image_check() } == sys::ESP_OK;
    if !has_dump && panic_msg.is_none() {
        return None;
    }
    let mut report = String::new();
    if has_dump {
        let mut summary: sys::esp_core_dump_summary_t = unsafe { std::mem::zeroed() };
        if unsafe { sys::esp_core_dump_get_summary(&mut summary) } == sys::ESP_OK {
            let task = std::ffi::CStr::from_bytes_until_nul(unsafe {
                std::slice::from_raw_parts(summary.exc_task.as_ptr() as *const u8, summary.exc_task.len())
            })
            .map(|c| c.to_string_lossy().into_owned())
            .unwrap_or_default();
            report.push_str(&format!(
                "task {task}, cause {}, pc 0x{:08x}, addr 0x{:08x}",
                summary.ex_info.exc_cause, summary.exc_pc, summary.ex_info.exc_vaddr
            ));
        } else {
            report.push_str("core dump present but unreadable");
        }
        unsafe {
            sys::esp_core_dump_image_erase();
        }
    }
    if let Some(m) = panic_msg {
        if !report.is_empty() {
            report.push_str(" | ");
        }
        report.push_str(&m);
        let _ = nvs.remove(KEY_PANIC);
    }
    let full = format!("{uptime_hint}: {report}");
    let _ = nvs.set_str(KEY_REPORT, &full.chars().take(400).collect::<String>());
    Some(full)
}

/// The last stored report, from this or an earlier boot.
pub fn last_report() -> String {
    let Some(nvs) = NVS.get() else { return "none".into() };
    let mut buf = [0u8; 512];
    nvs.get_str(KEY_REPORT, &mut buf).ok().flatten().map(str::to_string).unwrap_or_else(|| "none".into())
}

pub fn clear() {
    if let Some(nvs) = NVS.get() {
        let _ = nvs.remove(KEY_REPORT);
    }
}
