//! Logger that prints to the ESP-IDF console as usual and also forwards lines to Home
//! Assistant clients that subscribed to logs over the native API (ESPHome's log view).

use std::sync::OnceLock;

use esp_idf_svc::log::{EspIdfLogFilter, EspIdfLogger, EspLogger};
use esphome_api::Server;
use log::{Level, Log, Metadata, Record};

static SERVER: OnceLock<Server> = OnceLock::new();

struct Tee {
    esp: EspLogger,
}

impl Log for Tee {
    fn enabled(&self, metadata: &Metadata) -> bool {
        self.esp.enabled(metadata)
    }

    fn log(&self, record: &Record) {
        self.esp.log(record);
        let Some(server) = SERVER.get() else { return };
        // Never forward the API layer's own chatter: sending it would generate more of it.
        if record.target().starts_with("esphome_api") || record.level() > Level::Debug {
            return;
        }
        let (level, marker) = match record.level() {
            Level::Error => (1, 'E'),
            Level::Warn => (2, 'W'),
            Level::Info => (3, 'I'),
            Level::Debug => (5, 'D'),
            Level::Trace => (6, 'V'),
        };
        let target = record.target().rsplit("::").next().unwrap_or("fw");
        let line = format!("[{marker}][{target}]: {}", record.args());
        server.log(level, line.into_bytes());
    }

    fn flush(&self) {}
}

/// Install as the global logger. Call once, before any logging.
pub fn install() {
    let filter = EspIdfLogFilter::new();
    filter.initialize(); // sets log::max_level from the ESP-IDF log config
    let tee = Tee { esp: EspIdfLogger::new(filter) };
    if log::set_boxed_logger(Box::new(tee)).is_err() {
        // Logger already set (should not happen); fall back to the plain ESP logger.
        EspLogger::initialize_default();
    }
}

/// Start forwarding to API clients.
pub fn attach(server: Server) {
    let _ = SERVER.set(server);
}
