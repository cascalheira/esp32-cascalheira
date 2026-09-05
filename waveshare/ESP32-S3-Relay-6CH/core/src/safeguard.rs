//! Per-channel maximum on-time. A timer starts whenever a relay turns on, whoever turned it on;
//! when it expires the relay must be turned off. Time is fed in as monotonic milliseconds.

use crate::CHANNELS;

#[derive(Debug, Clone, Default)]
pub struct Safeguard {
    max_on_ms: [u64; CHANNELS],
    on_since_ms: [Option<u64>; CHANNELS],
}

impl Safeguard {
    pub fn new(max_on_min: [u16; CHANNELS]) -> Safeguard {
        let mut s = Safeguard::default();
        for (ch, m) in max_on_min.iter().enumerate() {
            s.set_max_on_min(ch, *m);
        }
        s
    }

    /// 0 disables the safeguard for that channel.
    pub fn set_max_on_min(&mut self, ch: usize, minutes: u16) {
        self.max_on_ms[ch] = minutes as u64 * 60_000;
    }

    pub fn max_on_min(&self, ch: usize) -> u16 {
        (self.max_on_ms[ch] / 60_000) as u16
    }

    /// Call whenever the physical relay state changes.
    pub fn relay_changed(&mut self, ch: usize, on: bool, now_ms: u64) {
        self.on_since_ms[ch] = if on { Some(now_ms) } else { None };
    }

    /// Milliseconds the channel has been on, if it is on.
    pub fn on_for_ms(&self, ch: usize, now_ms: u64) -> Option<u64> {
        self.on_since_ms[ch].map(|t| now_ms.saturating_sub(t))
    }

    /// Channels whose timer has expired. The caller must turn them off and report
    /// the change back through [`Safeguard::relay_changed`].
    pub fn expired(&self, now_ms: u64) -> Vec<usize> {
        (0..CHANNELS)
            .filter(|&ch| {
                self.max_on_ms[ch] > 0
                    && matches!(self.on_for_ms(ch, now_ms), Some(t) if t >= self.max_on_ms[ch])
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trips_after_limit() {
        let mut s = Safeguard::new([0; CHANNELS]);
        s.set_max_on_min(2, 10);
        s.relay_changed(2, true, 1_000);
        s.relay_changed(3, true, 1_000); // no limit on ch4
        assert!(s.expired(1_000 + 9 * 60_000).is_empty());
        assert_eq!(s.expired(1_000 + 10 * 60_000), vec![2]);
        s.relay_changed(2, false, 1_000 + 10 * 60_000);
        assert!(s.expired(1_000 + 60 * 60_000).is_empty());
    }

    #[test]
    fn re_on_restarts_timer_and_zero_disables() {
        let mut s = Safeguard::new([5; CHANNELS]);
        s.relay_changed(0, true, 0);
        s.relay_changed(0, true, 4 * 60_000); // e.g. repeated ON command: restart
        assert!(s.expired(5 * 60_000).is_empty());
        assert_eq!(s.expired(9 * 60_000), vec![0]);
        s.set_max_on_min(0, 0);
        assert!(s.expired(999 * 60_000).is_empty());
        assert_eq!(s.max_on_min(1), 5);
    }
}
