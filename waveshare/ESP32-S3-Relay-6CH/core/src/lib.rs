//! Hardware-independent logic for the 6-channel relay controller.
//!
//! Compiled and unit-tested on the host; the firmware crate is a thin I/O shell around it.
//! Nothing in here touches GPIO, WiFi, MQTT or the clock: the firmware feeds in time, link
//! events and commands, and applies the returned [`controller::Action`]s.

pub mod board;
pub mod config;
pub mod controller;
pub mod safeguard;
pub mod schedule;

pub use board::*;
pub use config::{ChannelConfig, Config, Mode};
pub use controller::{Action, Controller, Link, Usage};
pub use schedule::{Block, DayMask, LocalTime, Schedule, ScheduleState, Weekday};

/// Number of relay channels on the board.
pub const CHANNELS: usize = 6;
