//! Video counters at the media boundaries. Rates use elapsed wall time, not call ticks.
use protocol::VideoStats;
use std::time::Instant;

#[derive(Default)]
pub struct Meter {
    since: Option<Instant>,
    sent: (u64, u32),
    received: (u64, u32),
    rendered: u32,
    last: Option<Instant>,
    size: (u32, u32),
    watching: Option<Instant>,
}

impl Meter {
    pub fn expect(&mut self, expected: bool, now: Instant) {
        if expected {
            self.watching.get_or_insert(now);
        } else {
            self.watching = None;
        }
    }
    /// One video packet accepted by UDP or the TCP transport after the WebRTC pacer.
    pub fn sent(&mut self, bytes: usize, ends_frame: bool, now: Instant) {
        self.since.get_or_insert(now);
        self.sent.0 += bytes as u64;
        self.sent.1 += u32::from(ends_frame);
    }

    pub fn received(&mut self, bytes: usize, now: Instant) {
        self.since.get_or_insert(now);
        self.received.0 += bytes as u64;
        self.received.1 += 1;
    }

    pub fn rendered(&mut self, width: u32, height: u32, now: Instant) {
        self.since.get_or_insert(now);
        self.rendered += 1;
        self.last = Some(now);
        self.size = (width, height);
    }

    pub fn clear(&mut self) {
        self.last = None;
        self.size = (0, 0);
        self.rendered = 0;
        self.watching = None;
    }

    pub fn sample(&mut self, now: Instant) -> VideoStats {
        let elapsed = now.saturating_duration_since(self.since.replace(now).unwrap_or(now)).as_secs_f64().max(0.001);
        let rate = |count: u64| (count as f64 / elapsed).round().min(u32::MAX as f64) as u32;
        let stats = VideoStats {
            receive_expected: self.watching.is_some(),
            watching_ms: self.watching.map_or(0, |at| now.saturating_duration_since(at).as_millis().min(u32::MAX as u128) as u32),
            send_kbps: rate(self.sent.0 * 8 / 1000),
            receive_kbps: rate(self.received.0 * 8 / 1000),
            send_fps: rate(self.sent.1 as u64),
            receive_fps: rate(self.received.1 as u64),
            rendered_fps: rate(self.rendered as u64),
            width: self.size.0,
            height: self.size.1,
            frame_age_ms: self.last.map(|at| now.saturating_duration_since(at).as_millis().min(u32::MAX as u128) as u32),
        };
        self.sent = (0, 0);
        self.received = (0, 0);
        self.rendered = 0;
        stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn rates_use_elapsed_time_and_a_stalled_picture_keeps_aging() {
        let now = Instant::now();
        let mut meter = Meter::default();
        meter.sample(now);
        for _ in 0..60 {
            meter.sent(6_000, false, now);
            meter.sent(4_000, true, now);
            meter.received(5_000, now);
            meter.rendered(1920, 1080, now);
        }
        let stats = meter.sample(now + Duration::from_secs(2));
        assert_eq!((stats.send_fps, stats.receive_fps, stats.rendered_fps), (30, 30, 30));
        assert_eq!((stats.send_kbps, stats.receive_kbps), (2400, 1200));
        let idle = meter.sample(now + Duration::from_secs(3));
        assert_eq!(idle.rendered_fps, 0);
        assert_eq!(idle.frame_age_ms, Some(3000));
        meter.clear();
        let cleared = meter.sample(now + Duration::from_secs(4));
        assert_eq!((cleared.width, cleared.height, cleared.frame_age_ms), (0, 0, None));
    }
}
