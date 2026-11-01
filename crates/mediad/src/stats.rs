//! How the connection is doing (`MediaRequest::Stats`), over the last second: round trip,
//! jitter, loss, what we send and receive, and the path. Each call loop counts its own bytes
//! and keeps one [`Meter`]; once a second it publishes a [`CallStats`] to `Controls`, which the
//! request reads without touching the loop.
//!
//! A Tiny call grades our own path, in both directions: someone else's bad uplink is not ours, and
//! must not read as our connection. Loss is str0m's TWCC count of what the server missed of ours
//! and we missed of the server's, the worse of the two. Jitter is what the server reports of our
//! microphone, and with two voices or more the steadier one, since what they have in common is
//! the link to us. Neither says anything while nothing flows: `CallStats::link_measured` is then
//! false. A Discord call knows only its voices: RFC 3550's interarrival jitter, kept per voice by
//! `media::Stream`, the worst reported; and the loss the jitter buffers found and nothing rebuilt.
//! Either way, what the voices lost goes out for the hints about them.
//! Round trip: a Discord call times the voice server's echo of our keepalive; a Tiny call takes
//! the ICE checks on the chosen pair. str0m's own `rtt` is a TWCC feedback's arrival less a
//! packet's send time: the 50 ms between feedbacks is in it, and it stands still while we send
//! nothing. It only stands in until the first check is answered.

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
    /// Packets that came after their turn to play had gone.
    pub late: u64,
    /// Times the voice stalled in mid-speech and went on when its packet came.
    pub gaps: u64,
    pub jitter_ms: u32,
}

#[derive(Debug)]
pub struct Meter {
    sent: u64,
    received: u64,
    since: Instant,
    /// Over the whole call: lost frames made up, and rebuilt from the next packet; packets too
    /// late to play, and stalls in the middle of someone speaking.
    concealed: u64,
    recovered: u64,
    late: u64,
    gaps: u64,
    /// Loss and jitter are of our own path, from the reports below; not of the voices.
    own_path: bool,
    /// The worse direction's loss since the last second.
    path_loss: Option<f32>,
    /// How unevenly the server hears our microphone, in ms, since the last second.
    uplink_jitter: Option<u32>,
    /// Microphone packets sent, as last reported.
    spoke: u64,
    /// The devices' troubles, as the call loop last said.
    pub troubles: Troubles,
    pub rtt_ms: u32,
    pub path: CallPath,
    /// A keepalive out, and when, for timing its echo.
    probe: Option<(u32, Instant)>,
}

impl Meter {
    pub fn new(path: CallPath) -> Meter {
        Meter {
            sent: 0,
            received: 0,
            since: Instant::now(),
            concealed: 0,
            recovered: 0,
            late: 0,
            gaps: 0,
            own_path: false,
            path_loss: None,
            uplink_jitter: None,
            spoke: 0,
            troubles: Troubles::default(),
            rtt_ms: 0,
            path,
            probe: None,
        }
    }

    /// For a call whose link reports on itself ([`Meter::path_loss`]).
    pub fn own_path(path: CallPath) -> Meter {
        Meter { own_path: true, ..Meter::new(path) }
    }

    /// The fraction of ours the server missed, and of the server's we missed, over the last
    /// second. `None` for a direction nothing went in.
    pub fn path_loss(&mut self, egress: Option<f32>, ingress: Option<f32>) {
        self.path_loss = [self.path_loss, egress, ingress].into_iter().flatten().reduce(f32::max);
    }

    /// The microphone has sent `packets` so far: true if that is more than at the last report.
    /// A report from the far end says nothing about a silence, and they keep coming.
    pub fn spoke(&mut self, packets: u64) -> bool {
        let more = packets > self.spoke;
        self.spoke = packets;
        more
    }

    /// The server's jitter for our microphone, in RTP time.
    pub fn uplink_jitter(&mut self, rtp: u32) {
        self.uplink_jitter = Some((u64::from(rtp) * 1000 / u64::from(RATE)) as u32);
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

    /// What a Tiny call's link says: the last ICE check's round trip, and str0m's `rtt`.
    pub fn round_trip(&mut self, check: Option<Duration>, feedback: Option<Duration>) {
        if let Some(rtt) = check.or(feedback) {
            self.rtt_ms = rtt.as_millis().min(u128::from(u32::MAX)) as u32;
        }
    }

    /// The last second, from what each voice counted in it; and the counting starts again.
    pub fn second(&mut self, voices: impl Iterator<Item = Counts>) -> CallStats {
        let (mut received, mut lost, mut rebuilt, mut jitter) = (0, 0, 0, 0);
        let (mut steadiest, mut heard) = (u32::MAX, 0);
        for voice in voices {
            if voice.received > 0 {
                steadiest = steadiest.min(voice.jitter_ms);
                heard += 1;
            }
            received += voice.received;
            lost += voice.lost;
            rebuilt += voice.rebuilt;
            self.late += voice.late;
            self.gaps += voice.gaps;
            jitter = jitter.max(voice.jitter_ms);
        }
        self.recovered += rebuilt;
        self.concealed += lost.saturating_sub(rebuilt);
        let count = |n: u64| n.min(u64::from(u32::MAX)) as u32;
        let troubles = self.troubles;
        let expected = received + lost;
        let per_mille = |lost: u64| (lost * 1000).checked_div(expected).map_or(0, |permille| permille.min(1000) as u16);
        let (loss, jitter, measured) = if self.own_path {
            let loss = self.path_loss.take();
            let shared = (heard >= 2).then_some(steadiest);
            let jitter = self.uplink_jitter.take().into_iter().chain(shared).max();
            let permille = loss.map_or(0, |loss| (loss * 1000.).round().clamp(0., 1000.) as u16);
            (permille, jitter.unwrap_or(0), loss.is_some() || jitter.is_some())
        } else {
            (per_mille(lost.saturating_sub(rebuilt)), jitter, expected > 0)
        };
        let elapsed = self.since.elapsed().as_secs_f64().max(0.001);
        let kbps = |bytes: u64| (bytes as f64 * 8. / 1000. / elapsed).round() as u32;
        let stats = CallStats {
            rtt_ms: self.rtt_ms,
            jitter_ms: jitter,
            loss_permille: loss,
            link_measured: measured,
            voice_loss_permille: per_mille(lost),
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
            late_packets: count(self.late),
            playout_gaps: count(self.gaps),
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
        let first = meter.second([Counts { received: 90, lost: 10, rebuilt: 4, late: 2, gaps: 1, jitter_ms: 4 }, Counts { received: 50, jitter_ms: 9, ..Counts::default() }].into_iter());
        assert_eq!(first.loss_permille, 40, "frames FEC rebuilt were heard");
        assert_eq!(first.voice_loss_permille, 66);
        assert_eq!(first.jitter_ms, 9);
        assert_eq!(first.path, CallPath::Udp);
        assert!(first.send_kbps > 0 && first.receive_kbps > first.send_kbps);
        let second = meter.second([Counts { received: 100, jitter_ms: 2, ..Counts::default() }, Counts { jitter_ms: 1, ..Counts::default() }].into_iter());
        assert_eq!(second.loss_permille, 0);
        // Concealment and FEC count over the whole call.
        assert_eq!((first.concealed_frames, first.fec_recovered), (6, 4));
        assert_eq!((second.concealed_frames, second.fec_recovered), (6, 4));
        assert_eq!((second.late_packets, second.playout_gaps), (2, 1));
        assert_eq!(second.send_kbps, 0);
    }

    fn awful(jitter_ms: u32) -> Counts {
        Counts { received: 40, lost: 10, jitter_ms, ..Counts::default() }
    }

    /// Someone on bad Wi-Fi lost a fifth of what they sent, and their packets came in lumps. The
    /// path from here to the server is fine, and that is what is reported.
    #[test]
    fn someone_elses_uplink_is_not_my_connection() {
        let mut meter = Meter::own_path(CallPath::Udp);
        meter.path_loss(Some(0.), Some(0.));
        let stats = meter.second([awful(90)].into_iter());
        assert_eq!((stats.loss_permille, stats.jitter_ms, stats.link_measured), (0, 0, true));
        assert_eq!(stats.voice_loss_permille, 200, "their trouble still reaches the hints about them");
    }

    #[test]
    fn my_path_is_graded_by_its_worse_direction() {
        let mut meter = Meter::own_path(CallPath::Udp);
        meter.path_loss(Some(0.01), Some(0.05));
        assert_eq!(meter.second(std::iter::empty()).loss_permille, 50);
        meter.path_loss(Some(0.02), None);
        assert_eq!(meter.second(std::iter::empty()).loss_permille, 20);
    }

    #[test]
    fn jitter_every_voice_shares_is_mine_and_the_servers_report_is_too() {
        let mut meter = Meter::own_path(CallPath::Udp);
        assert_eq!(meter.second([awful(70), awful(30)].into_iter()).jitter_ms, 30);
        assert_eq!(meter.second([awful(70)].into_iter()).jitter_ms, 0, "one voice cannot tell whose it is");
        meter.uplink_jitter(960);
        assert_eq!(meter.second([awful(70), awful(10)].into_iter()).jitter_ms, 20);
    }

    #[test]
    fn a_silent_second_measures_nothing() {
        let mut meter = Meter::own_path(CallPath::Udp);
        let quiet = meter.second(std::iter::empty());
        assert_eq!((quiet.loss_permille, quiet.link_measured), (0, false));
        assert!(!Meter::new(CallPath::Udp).second(std::iter::empty()).link_measured);
    }

    #[test]
    fn reports_count_only_while_the_microphone_sends() {
        let mut meter = Meter::own_path(CallPath::Udp);
        assert!(meter.spoke(50));
        assert!(!meter.spoke(50), "gated or muted");
        assert!(meter.spoke(90));
    }

    #[test]
    fn the_devices_troubles_go_out_with_each_second() {
        let mut meter = Meter::new(CallPath::Udp);
        meter.troubles = Troubles { mic_glitches: 3, speaker_glitches: 1, overruns: 2, underruns: 5, gaps: 7, skips: 0 };
        let stats = meter.second(std::iter::empty());
        assert_eq!((stats.device_glitches, stats.capture_overruns, stats.capture_gaps, stats.playout_underruns), (4, 2, 7, 5));
    }

    #[test]
    fn the_round_trip_is_the_ice_check_and_not_the_feedback_timing() {
        let mut meter = Meter::new(CallPath::Udp);
        let ms = |ms| Some(Duration::from_millis(ms));
        meter.round_trip(None, ms(90));
        assert_eq!(meter.rtt_ms, 90, "better than nothing until a check is answered");
        meter.round_trip(ms(31), ms(90));
        assert_eq!(meter.rtt_ms, 31);
        meter.round_trip(None, None);
        assert_eq!(meter.rtt_ms, 31, "the last one stands");
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
