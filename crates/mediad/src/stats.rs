//! How the connection is doing (`MediaRequest::Stats`), over the last second: round trip,
//! jitter, loss, what we send and receive, and the path. Each call loop counts its own bytes
//! and keeps one [`Meter`]; once a second it publishes a [`CallStats`] to `Controls`, which the
//! request reads without touching the loop.
//!
//! Jitter is RFC 3550's interarrival jitter, kept per voice by `media::Stream`; the worst voice
//! is reported. Loss is the packets the jitter buffers found missing, of all they expected.
//! Round trip: a Discord call times the voice server's echo of our keepalive; a Tiny call takes
//! str0m's, from RTCP or the ICE checks on the chosen pair.

use crate::audio::Troubles;
use crate::RATE;
use protocol::{CallPath, CallStats};
use std::time::{Duration, Instant};

/// RFC 3550 interarrival jitter for one sender, in samples at 48 kHz.
#[derive(Debug, Default, Clone)]
pub struct Arrivals {
    last: Option<(Instant, u32)>,
    jitter: f64,
}

impl Arrivals {
    /// A packet stamped `rtp` arrived at `at`.
    pub fn arrived(&mut self, rtp: u32, at: Instant) {
        if let Some((then, stamped)) = self.last {
            let apart = at.saturating_duration_since(then).as_secs_f64() * f64::from(RATE);
            let meant = f64::from(rtp.wrapping_sub(stamped) as i32);
            let d = (apart - meant).abs();
            // A spurt that starts after a long silence is not jitter.
            if d < f64::from(RATE) {
                self.jitter += (d - self.jitter) / 16.;
            }
        }
        if self.last.is_none_or(|(_, stamped)| rtp.wrapping_sub(stamped) as i32 >= 0) {
            self.last = Some((at, rtp));
        }
    }

    pub fn ms(&self) -> u32 {
        (self.jitter * 1000. / f64::from(RATE)).round() as u32
    }
}

/// One voice's counts since the meter last asked.
#[derive(Debug, Clone, Copy, Default)]
pub struct Counts {
    pub received: u64,
    pub lost: u64,
    /// Of the lost, those rebuilt from the next packet's copy (in-band FEC).
    pub rebuilt: u64,
    pub jitter_ms: u32,
}

#[derive(Debug)]
pub struct Meter {
    sent: u64,
    received: u64,
    since: Instant,
    /// Over the whole call: lost frames made up, and rebuilt from the next packet.
    concealed: u64,
    recovered: u64,
    /// The devices' troubles, as the call loop last said.
    pub troubles: Troubles,
    pub rtt_ms: u32,
    pub path: CallPath,
    /// A keepalive out, and when, for timing its echo.
    probe: Option<(u32, Instant)>,
}

impl Meter {
    pub fn new(path: CallPath) -> Meter {
        Meter { sent: 0, received: 0, since: Instant::now(), concealed: 0, recovered: 0, troubles: Troubles::default(), rtt_ms: 0, path, probe: None }
    }

    pub fn sent(&mut self, bytes: usize) {
        self.sent += bytes as u64;
    }

    pub fn received(&mut self, bytes: usize) {
        self.received += bytes as u64;
    }

    /// A keepalive numbered `n` went out now.
    pub fn probe(&mut self, n: u32) {
        self.probe = Some((n, Instant::now()));
    }

    /// Its echo came back: that is the round trip.
    pub fn echoed(&mut self, n: u32) {
        if let Some((_, at)) = self.probe.filter(|(sent, _)| *sent == n) {
            self.rtt_ms = at.elapsed().as_millis().min(u128::from(u32::MAX)) as u32;
            self.probe = None;
        }
    }

    pub fn round_trip(&mut self, rtt: Option<Duration>) {
        if let Some(rtt) = rtt {
            self.rtt_ms = rtt.as_millis().min(u128::from(u32::MAX)) as u32;
        }
    }

    /// The last second, from what each voice counted in it; and the counting starts again.
    pub fn second(&mut self, voices: impl Iterator<Item = Counts>) -> CallStats {
        let (mut received, mut lost, mut rebuilt, mut jitter) = (0, 0, 0, 0);
        for voice in voices {
            received += voice.received;
            lost += voice.lost;
            rebuilt += voice.rebuilt;
            jitter = jitter.max(voice.jitter_ms);
        }
        self.recovered += rebuilt;
        self.concealed += lost.saturating_sub(rebuilt);
        let count = |n: u64| n.min(u64::from(u32::MAX)) as u32;
        let troubles = self.troubles;
        let expected = received + lost;
        let elapsed = self.since.elapsed().as_secs_f64().max(0.001);
        let kbps = |bytes: u64| (bytes as f64 * 8. / 1000. / elapsed).round() as u32;
        let stats = CallStats {
            rtt_ms: self.rtt_ms,
            jitter_ms: jitter,
            loss_permille: (lost * 1000).checked_div(expected).map_or(0, |permille| permille.min(1000) as u16),
            send_kbps: kbps(self.sent),
            receive_kbps: kbps(self.received),
            path: self.path,
            codec: "opus".to_owned(),
            device_glitches: count(troubles.mic_glitches + troubles.speaker_glitches),
            capture_overruns: count(troubles.overruns),
            capture_gaps: count(troubles.gaps),
            playout_underruns: count(troubles.underruns),
            concealed_frames: count(self.concealed),
            fec_recovered: count(self.recovered),
        };
        (self.sent, self.received, self.since) = (0, 0, Instant::now());
        stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packets_on_time_have_no_jitter_and_late_ones_some() {
        let start = Instant::now();
        let mut steady = Arrivals::default();
        for n in 0..50u32 {
            steady.arrived(n * 960, start + Duration::from_millis(u64::from(n) * 20));
        }
        assert_eq!(steady.ms(), 0);
        let mut shaky = Arrivals::default();
        for n in 0..200u32 {
            let wobble = if n % 2 == 0 { 0 } else { 10 };
            shaky.arrived(n * 960, start + Duration::from_millis(u64::from(n) * 20 + wobble));
        }
        assert!((8..=11).contains(&shaky.ms()), "{}", shaky.ms());
    }

    #[test]
    fn a_second_reports_its_own_loss_and_rates() {
        let mut meter = Meter::new(CallPath::Udp);
        meter.sent(4000);
        meter.received(8000);
        let first = meter.second([Counts { received: 90, lost: 10, rebuilt: 4, jitter_ms: 4 }, Counts { received: 50, lost: 0, rebuilt: 0, jitter_ms: 9 }].into_iter());
        assert_eq!(first.loss_permille, 66);
        assert_eq!(first.jitter_ms, 9);
        assert_eq!(first.path, CallPath::Udp);
        assert!(first.send_kbps > 0 && first.receive_kbps > first.send_kbps);
        let second = meter.second([Counts { received: 100, jitter_ms: 2, ..Counts::default() }, Counts { jitter_ms: 1, ..Counts::default() }].into_iter());
        assert_eq!(second.loss_permille, 0);
        // Concealment and FEC count over the whole call.
        assert_eq!((first.concealed_frames, first.fec_recovered), (6, 4));
        assert_eq!((second.concealed_frames, second.fec_recovered), (6, 4));
        assert_eq!(second.send_kbps, 0);
    }

    #[test]
    fn the_devices_troubles_go_out_with_each_second() {
        let mut meter = Meter::new(CallPath::Udp);
        meter.troubles = Troubles { mic_glitches: 3, speaker_glitches: 1, overruns: 2, underruns: 5, gaps: 7, skips: 0 };
        let stats = meter.second(std::iter::empty());
        assert_eq!((stats.device_glitches, stats.capture_overruns, stats.capture_gaps, stats.playout_underruns), (4, 2, 7, 5));
    }

    #[test]
    fn a_keepalive_echo_is_the_round_trip() {
        let mut meter = Meter::new(CallPath::Udp);
        meter.probe(7);
        meter.echoed(6);
        assert_eq!(meter.rtt_ms, 0, "not ours");
        std::thread::sleep(Duration::from_millis(15));
        meter.echoed(7);
        assert!(meter.rtt_ms >= 15, "{}", meter.rtt_ms);
    }
}
