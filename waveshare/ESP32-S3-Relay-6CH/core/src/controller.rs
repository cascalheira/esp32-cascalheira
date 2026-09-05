//! Relay arbiter. Decides who controls the relays (Home Assistant or the stored schedule),
//! enforces the max-on-time safeguard, and reports what the firmware must do as [`Action`]s.
//!
//! The firmware owns all I/O. It calls the `on_*` methods when things happen and
//! [`Controller::tick`] about once a second with the monotonic clock and, if known, the
//! local wall-clock time. Every method returns the actions to apply, in order.

use crate::config::{Config, Mode};
use crate::safeguard::Safeguard;
use crate::schedule::{LocalTime, Schedule};
use crate::CHANNELS;

/// Who is in charge right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Link {
    /// An HA client is connected (and alive): HA commands drive the relays.
    Online,
    /// No HA: the stored schedule drives the relays.
    OfflineSchedule,
    /// No HA and no wall-clock time yet: relays held in their safe state.
    OfflineUnknownTime,
}

impl Link {
    pub fn as_str(self) -> &'static str {
        match self {
            Link::Online => "online",
            Link::OfflineSchedule => "offline_schedule",
            Link::OfflineUnknownTime => "offline_unknown_time",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Drive relay `ch` (0-based) and publish its state.
    SetRelay { ch: usize, on: bool },
    /// The link state changed; publish it.
    LinkChanged(Link),
    /// The max-on-time safeguard turned `ch` off; raise the alarm entity.
    SafeguardTripped { ch: usize },
    /// What the schedule currently wants for `ch` changed (reported even while online).
    ScheduleWants { ch: usize, on: bool },
    /// The rain hold expired; the firmware should persist config and refresh the HA entities.
    RainHoldEnded,
    /// Accumulated on-time for `ch` changed (whole minutes today and lifetime total).
    DailyUsage { ch: usize, minutes: u32, total_minutes: u32 },
    /// `ch` hit its daily cap and was switched off; it stays blocked until local midnight.
    DailyCapReached { ch: usize },
    /// A new local day began: usage counters and cap blocks were reset.
    DayRolled,
}

#[derive(Debug)]
pub struct Controller {
    cfg: Config,
    schedule: Schedule,
    safeguard: Safeguard,
    relays: [bool; CHANNELS],
    sched_prev: [Option<bool>; CHANNELS],
    ha_connected: bool,
    ha_declared_offline: bool,
    last_ha_seen_ms: Option<u64>,
    time_known: bool,
    link: Option<Link>,
    /// Current Unix time in seconds, if known (fed by the firmware on every tick).
    epoch: Option<u64>,
    on_today_ms: [u64; CHANNELS],
    total_ms: [u64; CHANNELS],
    capped: [bool; CHANNELS],
    today: Option<u32>,
    last_account_ms: Option<u64>,
}

/// Snapshot of the on-time counters, persisted by the firmware.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Usage {
    /// Local day key the `on_today_ms` counters belong to (0 = unknown).
    pub day: u32,
    pub on_today_ms: [u64; CHANNELS],
    pub total_ms: [u64; CHANNELS],
}

impl Controller {
    pub fn new(cfg: Config, schedule: Schedule) -> Controller {
        let max_on = core::array::from_fn(|i| cfg.channels[i].max_on_min);
        Controller {
            cfg,
            schedule,
            safeguard: Safeguard::new(max_on),
            relays: [false; CHANNELS],
            sched_prev: [None; CHANNELS],
            ha_connected: false,
            ha_declared_offline: false,
            last_ha_seen_ms: None,
            time_known: false,
            link: None,
            epoch: None,
            on_today_ms: [0; CHANNELS],
            total_ms: [0; CHANNELS],
            capped: [false; CHANNELS],
            today: None,
            last_account_ms: None,
        }
    }

    // ----- read-only accessors -------------------------------------------------------------

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    pub fn schedule(&self) -> &Schedule {
        &self.schedule
    }

    pub fn relays(&self) -> [bool; CHANNELS] {
        self.relays
    }

    pub fn link(&self) -> Link {
        self.link.unwrap_or(Link::OfflineUnknownTime)
    }

    pub fn time_known(&self) -> bool {
        self.time_known
    }

    /// Whole minutes each relay has been on so far today.
    pub fn on_today_min(&self) -> [u32; CHANNELS] {
        core::array::from_fn(|ch| (self.on_today_ms[ch] / 60_000) as u32)
    }

    /// Whole minutes each relay has been on in its lifetime (since the counters were last reset).
    pub fn total_min(&self) -> [u32; CHANNELS] {
        core::array::from_fn(|ch| (self.total_ms[ch] / 60_000) as u32)
    }

    pub fn usage(&self) -> Usage {
        Usage { day: self.today.unwrap_or(0), on_today_ms: self.on_today_ms, total_ms: self.total_ms }
    }

    /// Restore counters saved before a reboot. Today's counters are kept only if the saved day
    /// matches once the clock is known (checked in `account`); totals are always kept.
    pub fn restore_usage(&mut self, u: Usage) {
        self.total_ms = u.total_ms;
        self.on_today_ms = u.on_today_ms;
        self.today = (u.day != 0).then_some(u.day);
        for ch in 0..CHANNELS {
            let cap = self.cfg.channels[ch].max_daily_min as u64 * 60_000;
            self.capped[ch] = cap > 0 && self.on_today_ms[ch] >= cap;
        }
    }

    pub fn daily_capped(&self) -> [bool; CHANNELS] {
        self.capped
    }

    pub fn set_max_daily_min(&mut self, ch: usize, minutes: u16, now_ms: u64) -> Vec<Action> {
        if ch < CHANNELS {
            self.cfg.channels[ch].max_daily_min = minutes;
            if minutes == 0 && self.capped[ch] {
                self.capped[ch] = false;
                self.sched_prev[ch] = None;
            }
        }
        let mut out = Vec::new();
        self.account(now_ms, None, &mut out);
        out
    }

    // ----- inputs from the firmware -------------------------------------------------------

    pub fn on_ha_connected(&mut self, now_ms: u64) -> Vec<Action> {
        self.ha_connected = true;
        self.ha_declared_offline = false;
        // Optimistic: assume HA is there until the heartbeat grace says otherwise.
        self.last_ha_seen_ms = Some(now_ms);
        self.refresh(now_ms, None)
    }

    pub fn on_ha_disconnected(&mut self, now_ms: u64, local: Option<LocalTime>) -> Vec<Action> {
        self.ha_connected = false;
        self.refresh(now_ms, local)
    }

    /// Any message from HA (birth, command, schedule, heartbeat) proves it is alive.
    pub fn on_ha_seen(&mut self, now_ms: u64) -> Vec<Action> {
        self.ha_declared_offline = false;
        self.last_ha_seen_ms = Some(now_ms);
        self.refresh(now_ms, None)
    }

    /// HA's will message (`homeassistant/status` = `offline`).
    pub fn on_ha_offline(&mut self, now_ms: u64, local: Option<LocalTime>) -> Vec<Action> {
        self.ha_declared_offline = true;
        self.refresh(now_ms, local)
    }

    /// Wall-clock time became valid (SNTP, RTC, or HA fallback).
    pub fn on_time_known(&mut self, now_ms: u64, local: LocalTime) -> Vec<Action> {
        self.time_known = true;
        self.refresh(now_ms, Some(local))
    }

    /// Relay command from HA. Also counts as an HA heartbeat.
    pub fn on_ha_command(&mut self, ch: usize, on: bool, now_ms: u64) -> Vec<Action> {
        let mut out = self.on_ha_seen(now_ms);
        if ch >= CHANNELS || self.cfg.mode == Mode::Off || self.link() != Link::Online {
            return out;
        }
        self.set_relay(ch, on, now_ms, &mut out, true);
        out
    }

    /// Local override (button, console): always honoured except in `Mode::Off`.
    pub fn on_local_command(&mut self, ch: usize, on: bool, now_ms: u64) -> Vec<Action> {
        let mut out = Vec::new();
        if ch < CHANNELS && self.cfg.mode != Mode::Off {
            self.set_relay(ch, on, now_ms, &mut out, false);
        }
        out
    }

    pub fn all_off(&mut self, now_ms: u64) -> Vec<Action> {
        let mut out = Vec::new();
        for ch in 0..CHANNELS {
            self.set_relay(ch, false, now_ms, &mut out, false);
        }
        out
    }

    pub fn set_schedule(&mut self, schedule: Schedule, now_ms: u64, local: Option<LocalTime>) -> Vec<Action> {
        self.schedule = schedule;
        self.sched_prev = [None; CHANNELS];
        self.refresh(now_ms, local)
    }

    pub fn set_max_on_min(&mut self, ch: usize, minutes: u16, now_ms: u64) -> Vec<Action> {
        if ch < CHANNELS {
            self.cfg.channels[ch].max_on_min = minutes;
            self.safeguard.set_max_on_min(ch, minutes);
        }
        self.poll_safeguard(now_ms)
    }

    pub fn set_safe_state(&mut self, ch: usize, on: bool, now_ms: u64) -> Vec<Action> {
        if ch < CHANNELS {
            self.cfg.channels[ch].safe_state = on;
        }
        self.refresh(now_ms, None)
    }

    /// Enable or disable the interlock. Enabling it while several relays are on keeps only the
    /// lowest-numbered one, so the state is consistent with the rule from then on.
    pub fn set_exclusive(&mut self, on: bool, now_ms: u64) -> Vec<Action> {
        self.cfg.exclusive = on;
        let mut out = Vec::new();
        if on {
            if let Some(first) = self.relays.iter().position(|r| *r) {
                for ch in (first + 1)..CHANNELS {
                    self.set_relay(ch, false, now_ms, &mut out, false);
                }
            }
        }
        out
    }

    pub fn set_buzzer(&mut self, on: bool) {
        self.cfg.buzzer = on;
    }

    /// Feed the wall-clock time (Unix seconds). Needed for the rain hold to expire.
    pub fn set_epoch(&mut self, epoch: Option<u64>) {
        self.epoch = epoch;
    }

    /// Suspend the stored schedule for `hours` (0 clears the hold). Takes effect at once:
    /// schedule-driven relays that are on go off, and resume at the next block edge after expiry.
    pub fn set_rain_hold(&mut self, hours: u32, now_ms: u64, local: Option<LocalTime>) -> Vec<Action> {
        self.cfg.rain_hold_until = match (hours, self.epoch) {
            (0, _) | (_, None) => 0,
            (h, Some(e)) => e + h as u64 * 3600,
        };
        self.sched_prev = [None; CHANNELS];
        self.refresh(now_ms, local)
    }

    pub fn rain_hold_until(&self) -> Option<u64> {
        (self.cfg.rain_hold_until > 0).then_some(self.cfg.rain_hold_until)
    }

    pub fn rain_hold_active(&self) -> bool {
        match (self.cfg.rain_hold_until, self.epoch) {
            (0, _) => false,
            (until, Some(e)) => e < until,
            // Time unknown: assume the hold still stands rather than water into the rain.
            (_, None) => true,
        }
    }

    pub fn set_offline_grace_s(&mut self, secs: u32, now_ms: u64, local: Option<LocalTime>) -> Vec<Action> {
        self.cfg.offline_grace_s = secs;
        self.refresh(now_ms, local)
    }

    pub fn set_mode(&mut self, mode: Mode, now_ms: u64, local: Option<LocalTime>) -> Vec<Action> {
        self.cfg.mode = mode;
        self.sched_prev = [None; CHANNELS];
        let mut out = Vec::new();
        if mode == Mode::Off {
            out.extend(self.all_off(now_ms));
        }
        out.extend(self.refresh(now_ms, local));
        out
    }

    /// Periodic tick, about once per second. `local` is `None` until the clock is valid.
    pub fn tick(&mut self, now_ms: u64, local: Option<LocalTime>) -> Vec<Action> {
        self.refresh(now_ms, local)
    }

    // ----- internals ----------------------------------------------------------------------

    fn compute_link(&self, now_ms: u64) -> Link {
        let ha_alive = self.ha_connected
            && !self.ha_declared_offline
            && (self.cfg.offline_grace_s == 0
                || matches!(self.last_ha_seen_ms,
                    Some(t) if now_ms.saturating_sub(t) < self.cfg.offline_grace_s as u64 * 1000));
        if ha_alive {
            Link::Online
        } else if self.time_known {
            Link::OfflineSchedule
        } else {
            Link::OfflineUnknownTime
        }
    }

    /// Book on-time since the last call, roll the day at local midnight, enforce daily caps.
    fn account(&mut self, now_ms: u64, local: Option<LocalTime>, out: &mut Vec<Action>) {
        if let Some(t) = local {
            if self.today.is_some_and(|d| d != t.day) {
                self.on_today_ms = [0; CHANNELS];
                // Relays that were blocked by the cap re-evaluate their schedule from scratch:
                // a block refused yesterday may still be active and must now apply. Others keep
                // their state (a manual override is not cancelled by midnight).
                for ch in 0..CHANNELS {
                    if self.capped[ch] {
                        self.sched_prev[ch] = None;
                    }
                }
                self.capped = [false; CHANNELS];
                out.push(Action::DayRolled);
                for ch in 0..CHANNELS {
                    out.push(Action::DailyUsage { ch, minutes: 0, total_minutes: (self.total_ms[ch] / 60_000) as u32 });
                }
            }
            self.today = Some(t.day);
        }
        let delta = self.last_account_ms.map(|l| now_ms.saturating_sub(l)).unwrap_or(0);
        self.last_account_ms = Some(now_ms);
        for ch in 0..CHANNELS {
            if !self.relays[ch] {
                continue;
            }
            let before_min = self.on_today_ms[ch] / 60_000;
            let before_total = self.total_ms[ch] / 60_000;
            self.on_today_ms[ch] += delta;
            self.total_ms[ch] += delta;
            if self.on_today_ms[ch] / 60_000 != before_min || self.total_ms[ch] / 60_000 != before_total {
                out.push(Action::DailyUsage {
                    ch,
                    minutes: (self.on_today_ms[ch] / 60_000) as u32,
                    total_minutes: (self.total_ms[ch] / 60_000) as u32,
                });
            }
            let cap = self.cfg.channels[ch].max_daily_min as u64 * 60_000;
            if cap > 0 && self.on_today_ms[ch] >= cap {
                self.capped[ch] = true;
                self.set_relay(ch, false, now_ms, out, false);
                out.push(Action::DailyCapReached { ch });
            }
        }
    }

    /// Re-evaluate link, schedule and safeguard. The heart of the arbiter.
    fn refresh(&mut self, now_ms: u64, local: Option<LocalTime>) -> Vec<Action> {
        let mut out = Vec::new();
        self.account(now_ms, local, &mut out);
        let new_link = self.compute_link(now_ms);
        let link_changed = self.link != Some(new_link);
        if link_changed {
            self.link = Some(new_link);
            out.push(Action::LinkChanged(new_link));
        }

        if self.cfg.mode == Mode::Off {
            out.extend(self.poll_safeguard(now_ms));
            return out;
        }

        // A hold that just expired must re-evaluate every channel, not wait for the next edge.
        if self.cfg.rain_hold_until > 0 && !self.rain_hold_active() {
            self.cfg.rain_hold_until = 0;
            self.sched_prev = [None; CHANNELS];
            out.push(Action::RainHoldEnded);
        }

        // Report schedule wishes (edge-triggered) and apply them when offline in Auto mode.
        // During a rain hold the stored schedule counts as empty.
        if let Some(t) = local {
            let wanted = if self.rain_hold_active() { [false; CHANNELS] } else { self.schedule.wanted(t) };
            let apply = new_link == Link::OfflineSchedule && self.cfg.mode == Mode::Auto;
            for ch in 0..CHANNELS {
                let edge = self.sched_prev[ch] != Some(wanted[ch]);
                if edge {
                    self.sched_prev[ch] = Some(wanted[ch]);
                    out.push(Action::ScheduleWants { ch, on: wanted[ch] });
                }
                // On entering offline-schedule mode snap every channel to the schedule;
                // afterwards only follow schedule edges so a safeguard trip or a local
                // override is not undone until the next block boundary.
                if apply && (edge || link_changed) {
                    self.set_relay(ch, wanted[ch], now_ms, &mut out, false);
                }
            }
        }

        // With no clock and no HA there is nothing sensible to do but hold the safe state.
        if new_link == Link::OfflineUnknownTime && link_changed {
            for ch in 0..CHANNELS {
                let on = self.cfg.channels[ch].safe_state;
                self.set_relay(ch, on, now_ms, &mut out, false);
            }
        }

        out.extend(self.poll_safeguard(now_ms));
        out
    }

    fn poll_safeguard(&mut self, now_ms: u64) -> Vec<Action> {
        let mut out = Vec::new();
        for ch in self.safeguard.expired(now_ms) {
            self.set_relay(ch, false, now_ms, &mut out, false);
            out.push(Action::SafeguardTripped { ch });
        }
        out
    }

    /// `restart_timer` restarts the safeguard on a repeated ON command (HA re-sending ON
    /// means "I still want this on", so give it a fresh window).
    fn set_relay(&mut self, ch: usize, on: bool, now_ms: u64, out: &mut Vec<Action>, restart_timer: bool) {
        if on && self.capped[ch] {
            return; // daily cap reached: stays off until midnight, whoever asks
        }
        if on && self.cfg.exclusive {
            // Interlock: others off before this one goes on (break-before-make).
            for other in 0..CHANNELS {
                if other != ch && self.relays[other] {
                    self.relays[other] = false;
                    self.safeguard.relay_changed(other, false, now_ms);
                    out.push(Action::SetRelay { ch: other, on: false });
                }
            }
        }
        if self.relays[ch] == on {
            if on && restart_timer {
                self.safeguard.relay_changed(ch, true, now_ms);
            }
            return;
        }
        self.relays[ch] = on;
        self.safeguard.relay_changed(ch, on, now_ms);
        out.push(Action::SetRelay { ch, on });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::Weekday;

    const MIN: u64 = 60_000;

    fn sched() -> Schedule {
        // ch1 weekdays 06:00-07:00, ch2 every day 20:00-21:00
        Schedule::from_json(
            r#"{"rev":1,"channels":{
                "1":[{"days":"MTWTF--","from":360,"to":420}],
                "2":[{"days":"MTWTFSS","from":1200,"to":1260}]}}"#,
        )
        .unwrap()
    }

    fn t(h: u16, m: u16) -> LocalTime {
        LocalTime::new(Weekday::Tue, h, m)
    }

    fn relays(acts: &[Action]) -> Vec<(usize, bool)> {
        acts.iter()
            .filter_map(|a| match a {
                Action::SetRelay { ch, on } => Some((*ch, *on)),
                _ => None,
            })
            .collect()
    }

    fn has_link(acts: &[Action], l: Link) -> bool {
        acts.contains(&Action::LinkChanged(l))
    }

    #[test]
    fn cold_boot_without_network_holds_safe_state() {
        let mut cfg = Config::default();
        cfg.channels[4].safe_state = true;
        let mut c = Controller::new(cfg, sched());
        let acts = c.tick(0, None);
        assert!(has_link(&acts, Link::OfflineUnknownTime));
        assert_eq!(relays(&acts), vec![(4, true)]);
        // Nothing changes while we keep not knowing the time.
        assert!(c.tick(1000, None).is_empty());
    }

    #[test]
    fn offline_with_time_follows_schedule_edges() {
        let mut c = Controller::new(Config::default(), sched());
        c.tick(0, None);
        // Time arrives at 05:59 on a Tuesday: schedule wants everything off.
        let acts = c.on_time_known(1000, t(5, 59));
        assert!(has_link(&acts, Link::OfflineSchedule));
        assert!(relays(&acts).is_empty(), "already off, no relay action");
        // 06:00 -> ch1 on
        let acts = c.tick(2000, Some(t(6, 0)));
        assert_eq!(relays(&acts), vec![(0, true)]);
        assert!(acts.contains(&Action::ScheduleWants { ch: 0, on: true }));
        // Steady state inside the block: nothing to do.
        assert!(c.tick(3000, Some(t(6, 30))).is_empty());
        // 07:00 -> ch1 off
        assert_eq!(relays(&c.tick(4000, Some(t(7, 0)))), vec![(0, false)]);
    }

    #[test]
    fn online_ha_drives_and_schedule_is_only_reported() {
        let mut c = Controller::new(Config::default(), sched());
        c.on_time_known(0, t(5, 0));
        let acts = c.on_ha_connected(1000);
        assert!(has_link(&acts, Link::Online));
        // Schedule says ch1 on at 06:00 but we are online: report only, do not switch.
        let acts = c.tick(2000, Some(t(6, 0)));
        assert!(acts.contains(&Action::ScheduleWants { ch: 0, on: true }));
        assert!(relays(&acts).is_empty());
        // HA command is honoured.
        assert_eq!(relays(&c.on_ha_command(2, true, 3000)), vec![(2, true)]);
        assert_eq!(c.relays(), [false, false, true, false, false, false]);
        // Repeated identical command produces no action.
        assert!(relays(&c.on_ha_command(2, true, 3500)).is_empty());
    }

    #[test]
    fn losing_ha_snaps_to_schedule_and_regaining_keeps_state() {
        let mut c = Controller::new(Config::default(), sched());
        c.on_time_known(0, t(6, 30));
        assert!(c.relays()[0], "schedule switched ch1 on while offline");
        c.on_ha_connected(1000);
        c.on_ha_command(0, false, 1500); // HA overrides ch1 off while online
        c.on_ha_command(2, true, 2000); // HA turned ch3 on manually
        assert_eq!(c.relays(), [false, false, true, false, false, false]);
        // WiFi drops at 06:31: schedule wants ch1 on, ch3 off.
        let acts = c.on_ha_disconnected(3000, Some(t(6, 31)));
        assert!(has_link(&acts, Link::OfflineSchedule));
        let mut r = relays(&acts);
        r.sort();
        assert_eq!(r, vec![(0, true), (2, false)]);
        // Back online at 06:40: keep relays as they are, just report the link.
        let acts = c.on_ha_connected(4000);
        assert!(has_link(&acts, Link::Online));
        assert!(relays(&acts).is_empty());
        assert_eq!(c.relays(), [true, false, false, false, false, false]);
    }

    #[test]
    fn ha_commands_ignored_while_offline() {
        let mut c = Controller::new(Config::default(), sched());
        c.on_time_known(0, t(12, 0));
        // Not connected: a stray command must not act (also proves link guard).
        assert!(relays(&c.on_ha_command(0, true, 1000)).is_empty());
        assert_eq!(c.relays(), [false; CHANNELS]);
    }

    #[test]
    fn safeguard_trips_in_any_mode_and_restarts_on_repeat_command() {
        let mut cfg = Config::default();
        cfg.channels[0].max_on_min = 10;
        let mut c = Controller::new(cfg, sched());
        c.on_time_known(0, t(12, 0));
        c.on_ha_connected(0);
        c.on_ha_command(0, true, 0);
        assert!(relays(&c.tick(9 * MIN, Some(t(12, 9)))).is_empty());
        // HA re-sends ON at minute 9: fresh window.
        c.on_ha_command(0, true, 9 * MIN);
        assert!(relays(&c.tick(15 * MIN, Some(t(12, 15)))).is_empty());
        let acts = c.tick(19 * MIN, Some(t(12, 19)));
        assert_eq!(relays(&acts), vec![(0, false)]);
        assert!(acts.contains(&Action::SafeguardTripped { ch: 0 }));
        assert!(!c.relays()[0]);
    }

    #[test]
    fn safeguard_applies_to_schedule_driven_relays_and_is_not_undone() {
        let mut cfg = Config::default();
        cfg.channels[1].max_on_min = 30;
        let mut c = Controller::new(cfg, sched());
        // Offline, 20:00 -> ch2 on by schedule.
        assert_eq!(relays(&c.on_time_known(0, t(20, 0))), vec![(1, true)]);
        // 20:30 -> tripped.
        let acts = c.tick(30 * MIN, Some(t(20, 30)));
        assert!(acts.contains(&Action::SafeguardTripped { ch: 1 }));
        assert_eq!(relays(&acts), vec![(1, false)]);
        // Still inside the block at 20:45: the schedule must not turn it back on.
        assert!(relays(&c.tick(45 * MIN, Some(t(20, 45)))).is_empty());
        // 21:00 block ends: already off, nothing to do but the wish edge.
        let acts = c.tick(60 * MIN, Some(t(21, 0)));
        assert!(relays(&acts).is_empty());
        assert!(acts.contains(&Action::ScheduleWants { ch: 1, on: false }));
    }

    #[test]
    fn changing_max_on_below_elapsed_trips_immediately() {
        let mut c = Controller::new(Config::default(), sched());
        c.on_time_known(0, t(12, 0));
        c.on_ha_connected(0);
        c.on_ha_command(3, true, 0);
        let acts = c.set_max_on_min(3, 5, 6 * MIN);
        assert!(acts.contains(&Action::SafeguardTripped { ch: 3 }));
        assert_eq!(c.config().channels[3].max_on_min, 5);
    }

    #[test]
    fn heartbeat_grace_detects_dead_ha_behind_live_broker() {
        let mut cfg = Config::default();
        cfg.offline_grace_s = 120;
        let mut c = Controller::new(cfg, sched());
        c.on_time_known(0, t(5, 0));
        assert!(has_link(&c.on_ha_connected(0), Link::Online));
        c.on_ha_seen(60_000);
        assert!(c.tick(170_000, Some(t(5, 2))).is_empty(), "within grace");
        let acts = c.tick(181_000, Some(t(5, 3)));
        assert!(has_link(&acts, Link::OfflineSchedule));
        // HA's will message also flips us offline immediately.
        let mut c2 = Controller::new(Config::default(), sched());
        c2.on_time_known(0, t(5, 0));
        c2.on_ha_connected(0);
        assert!(has_link(&c2.on_ha_offline(1000, Some(t(5, 0))), Link::OfflineSchedule));
        assert!(has_link(&c2.on_ha_seen(2000), Link::Online));
    }

    #[test]
    fn new_schedule_is_applied_immediately_when_offline() {
        let mut c = Controller::new(Config::default(), Schedule::default());
        c.on_time_known(0, t(6, 30));
        assert_eq!(c.relays(), [false; CHANNELS]);
        let acts = c.set_schedule(sched(), 1000, Some(t(6, 30)));
        assert_eq!(relays(&acts), vec![(0, true)]);
        assert_eq!(c.schedule().rev, 1);
    }

    #[test]
    fn manual_mode_never_applies_schedule_and_off_mode_kills_everything() {
        let mut c = Controller::new(Config::default(), sched());
        c.on_time_known(0, t(6, 30));
        assert!(c.relays()[0]);
        let acts = c.set_mode(Mode::Manual, 1000, Some(t(6, 30)));
        assert!(relays(&acts).is_empty(), "manual keeps current state");
        assert!(relays(&c.tick(2000, Some(t(7, 0)))).is_empty(), "no schedule edges applied");
        assert!(relays(&c.tick(3000, Some(t(6, 0)))).is_empty());
        let acts = c.set_mode(Mode::Off, 4000, Some(t(6, 0)));
        assert_eq!(relays(&acts), vec![(0, false)]);
        c.on_ha_connected(5000);
        assert!(relays(&c.on_ha_command(0, true, 6000)).is_empty(), "off ignores HA");
        assert!(relays(&c.on_local_command(0, true, 6000)).is_empty());
        // Back to auto: schedule re-applies (offline) immediately.
        c.on_ha_disconnected(7000, Some(t(6, 0)));
        assert_eq!(relays(&c.set_mode(Mode::Auto, 8000, Some(t(6, 0)))), vec![(0, true)]);
    }

    #[test]
    fn exclusive_interlock_turns_others_off_first() {
        let mut cfg = Config::default();
        cfg.exclusive = true;
        let mut c = Controller::new(cfg, Schedule::default());
        c.on_time_known(0, t(12, 0));
        c.on_ha_connected(0);
        assert_eq!(relays(&c.on_ha_command(0, true, 1000)), vec![(0, true)]);
        // Turning relay 3 on switches relay 1 off first, then relay 3 on.
        assert_eq!(relays(&c.on_ha_command(2, true, 2000)), vec![(0, false), (2, true)]);
        assert_eq!(c.relays(), [false, false, true, false, false, false]);
        // Turning something off never touches the others.
        assert_eq!(relays(&c.on_ha_command(2, false, 3000)), vec![(2, false)]);
        // Local commands and the schedule obey the same rule.
        c.on_local_command(4, true, 4000);
        assert_eq!(relays(&c.on_local_command(5, true, 5000)), vec![(4, false), (5, true)]);
        // Losing HA with an empty schedule snaps everything off; a new schedule wanting ch1
        // then turns it on (nothing else is on, so no interlock action).
        assert_eq!(relays(&c.on_ha_disconnected(6000, Some(t(6, 30)))), vec![(5, false)]);
        assert_eq!(relays(&c.set_schedule(sched(), 7000, Some(t(6, 30)))), vec![(0, true)]);
        // Schedule edge with another relay on: interlock applies to schedule-driven changes too.
        c.on_local_command(3, true, 8000);
        assert_eq!(c.relays(), [false, false, false, true, false, false], "ch1 off, ch4 on");
        assert_eq!(relays(&c.tick(9000, Some(t(20, 0)))), vec![(3, false), (1, true)]);
    }

    #[test]
    fn enabling_exclusive_with_several_on_keeps_lowest() {
        let mut c = Controller::new(Config::default(), Schedule::default());
        c.on_time_known(0, t(12, 0));
        c.on_ha_connected(0);
        c.on_ha_command(1, true, 0);
        c.on_ha_command(3, true, 0);
        c.on_ha_command(5, true, 0);
        let mut r = relays(&c.set_exclusive(true, 1000));
        r.sort();
        assert_eq!(r, vec![(3, false), (5, false)]);
        assert_eq!(c.relays(), [false, true, false, false, false, false]);
        assert!(c.config().exclusive);
        assert!(c.set_exclusive(false, 2000).is_empty());
    }

    #[test]
    fn rain_hold_suspends_offline_schedule_and_expires() {
        let mut c = Controller::new(Config::default(), sched());
        c.set_epoch(Some(1_000_000));
        // Offline at 06:30: schedule turns ch1 on.
        assert_eq!(relays(&c.on_time_known(0, t(6, 30))), vec![(0, true)]);
        // Hold for 2 h: ch1 goes off immediately.
        let acts = c.set_rain_hold(2, 1000, Some(t(6, 30)));
        assert_eq!(relays(&acts), vec![(0, false)]);
        assert!(c.rain_hold_active());
        assert_eq!(c.rain_hold_until(), Some(1_000_000 + 7200));
        // Still inside the block later: nothing happens while held.
        c.set_epoch(Some(1_000_000 + 3600));
        assert!(relays(&c.tick(2000, Some(t(6, 45)))).is_empty());
        // Hold expires while the 20:00 block is active: schedule resumes at once.
        c.set_epoch(Some(1_000_000 + 7201));
        let acts = c.tick(3000, Some(t(20, 10)));
        assert!(acts.contains(&Action::RainHoldEnded));
        assert_eq!(relays(&acts), vec![(1, true)]);
        assert!(!c.rain_hold_active());
        assert_eq!(c.config().rain_hold_until, 0, "cleared after expiry");
        // Clearing by hand.
        c.set_rain_hold(5, 4000, Some(t(20, 10)));
        assert!(c.rain_hold_active());
        let acts = c.set_rain_hold(0, 5000, Some(t(20, 10)));
        assert_eq!(relays(&acts), vec![(1, true)], "schedule re-applied on clear");
    }

    #[test]
    fn rain_hold_never_blocks_ha_commands() {
        let mut c = Controller::new(Config::default(), sched());
        c.set_epoch(Some(1_000_000));
        c.on_time_known(0, t(12, 0));
        c.on_ha_connected(0);
        c.set_rain_hold(24, 0, Some(t(12, 0)));
        assert_eq!(relays(&c.on_ha_command(0, true, 1000)), vec![(0, true)]);
    }

    #[test]
    fn rain_hold_survives_reboot_with_unknown_time() {
        let mut cfg = Config::default();
        cfg.rain_hold_until = 2_000_000; // persisted from before the reboot
        let mut c = Controller::new(cfg, sched());
        // Time not known yet: hold assumed active; once known and inside the hold, still held.
        assert!(c.rain_hold_active());
        c.set_epoch(Some(1_999_000));
        assert!(relays(&c.on_time_known(0, t(6, 30))).is_empty(), "held: ch1 not switched on");
        c.set_epoch(Some(2_000_001));
        assert_eq!(relays(&c.tick(1000, Some(t(6, 31)))), vec![(0, true)]);
    }

    #[test]
    fn daily_cap_accumulates_blocks_and_resets_at_midnight() {
        let mut cfg = Config::default();
        cfg.channels[0].max_daily_min = 30;
        let mut c = Controller::new(cfg, Schedule::default());
        c.on_time_known(0, t(10, 0));
        c.on_ha_connected(0);
        c.on_ha_command(0, true, 0);
        // 20 minutes on: usage reported per minute, no cap yet.
        let acts = c.tick(20 * MIN, Some(t(10, 20)));
        assert!(acts.contains(&Action::DailyUsage { ch: 0, minutes: 20, total_minutes: 20 }));
        assert!(relays(&acts).is_empty());
        c.on_ha_command(0, false, 20 * MIN);
        assert_eq!(c.on_today_min()[0], 20);
        // Second run: cap hits after 10 more minutes even though the run itself is short.
        c.on_ha_command(0, true, 25 * MIN);
        assert!(relays(&c.tick(30 * MIN, Some(t(10, 30)))).is_empty());
        let acts = c.tick(35 * MIN, Some(t(10, 35)));
        assert_eq!(relays(&acts), vec![(0, false)]);
        assert!(acts.contains(&Action::DailyCapReached { ch: 0 }));
        assert!(c.daily_capped()[0]);
        // Any attempt to switch it on is refused: HA, local, schedule.
        assert!(relays(&c.on_ha_command(0, true, 36 * MIN)).is_empty());
        assert!(relays(&c.on_local_command(0, true, 36 * MIN)).is_empty());
        c.on_ha_disconnected(37 * MIN, Some(t(10, 37)));
        assert!(relays(&c.set_schedule(sched(), 38 * MIN, Some(t(6, 30)))).is_empty(), "schedule wants ch1 but it is capped");
        // Other relays are unaffected.
        assert_eq!(relays(&c.on_local_command(1, true, 39 * MIN)), vec![(1, true)]);
        // Midnight: counters and block reset; the schedule (Tue 06:30 -> ch1) applies again.
        let acts = c.tick(40 * MIN, Some(LocalTime::new(Weekday::Wed, 6, 30)));
        assert!(acts.contains(&Action::DayRolled));
        assert!(!c.daily_capped()[0]);
        assert_eq!(c.on_today_min()[0], 0);
        assert_eq!(relays(&acts), vec![(0, true)]);
    }

    #[test]
    fn usage_totals_persist_and_today_resets_only_on_a_new_day() {
        let mut cfg = Config::default();
        cfg.channels[0].max_daily_min = 30;
        let mut c = Controller::new(cfg.clone(), Schedule::default());
        c.on_time_known(0, t(10, 0)); // day key = Tue
        c.on_ha_connected(0);
        c.on_ha_command(0, true, 0);
        c.tick(25 * MIN, Some(t(10, 25)));
        c.on_ha_command(0, false, 25 * MIN);
        let saved = c.usage();
        assert_eq!(saved.on_today_ms[0], 25 * MIN);
        assert_eq!(saved.total_ms[0], 25 * MIN);
        assert_eq!(saved.day, LocalTime::new(Weekday::Tue, 0, 0).day);

        // Reboot the same day: today's minutes and the cap state come back.
        let mut c2 = Controller::new(cfg.clone(), Schedule::default());
        c2.restore_usage(saved);
        assert_eq!(c2.on_today_min()[0], 25);
        c2.on_time_known(0, t(11, 0));
        c2.on_ha_connected(0);
        c2.on_ha_command(0, true, 0);
        let acts = c2.tick(5 * MIN, Some(t(11, 5)));
        assert!(acts.contains(&Action::DailyCapReached { ch: 0 }), "25 + 5 reaches the 30 min cap");
        assert_eq!(c2.total_min()[0], 30);

        // Reboot on another day: today resets, the total is kept, the cap is lifted.
        let mut c3 = Controller::new(cfg, Schedule::default());
        c3.restore_usage(c2.usage());
        assert!(c3.daily_capped()[0], "before the clock is known the saved cap state stands");
        let acts = c3.on_time_known(0, LocalTime::new(Weekday::Wed, 9, 0));
        assert!(acts.contains(&Action::DayRolled));
        assert_eq!(c3.on_today_min()[0], 0);
        assert_eq!(c3.total_min()[0], 30);
        assert!(!c3.daily_capped()[0]);
    }

    #[test]
    fn daily_cap_disabled_by_zero_and_clearing_unblocks() {
        let mut cfg = Config::default();
        cfg.channels[2].max_daily_min = 1;
        let mut c = Controller::new(cfg, Schedule::default());
        c.on_time_known(0, t(12, 0));
        c.on_ha_connected(0);
        c.on_ha_command(2, true, 0);
        assert!(c.tick(MIN, Some(t(12, 1))).contains(&Action::DailyCapReached { ch: 2 }));
        // Raising the cap to 0 (off) lifts the block.
        c.set_max_on_min(2, 0, MIN);
        c.set_max_daily_min(2, 0, MIN);
        assert_eq!(relays(&c.on_ha_command(2, true, 2 * MIN)), vec![(2, true)]);
    }

    #[test]
    fn local_override_survives_until_next_schedule_edge() {
        let mut c = Controller::new(Config::default(), sched());
        c.on_time_known(0, t(6, 0));
        assert!(c.relays()[0]);
        assert_eq!(relays(&c.on_local_command(0, false, 1000)), vec![(0, false)]);
        assert!(relays(&c.tick(2000, Some(t(6, 30)))).is_empty());
        assert!(relays(&c.tick(3000, Some(t(7, 0)))).is_empty(), "already off at block end");
        assert_eq!(relays(&c.all_off(4000)), vec![]);
    }
}
