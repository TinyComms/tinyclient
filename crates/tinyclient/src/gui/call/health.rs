//! How a call is doing, in words and bars: the signal bars beside "Voice connected", the grade
//! of the link, and the plain-English hints of the Call health panel. Everything here is read
//! from `CallStats` (about once a second while a call is up) and never measured here.
//!
//! The audio pipeline's own counters (the microphone dropping audio, the speaker running dry,
//! audio concealed or recovered) are [`Health`] fields that stay `None` until `CallStats`
//! carries them; each hint that needs one is simply not given without it.
//!
//! Also here, because it is the same kind of judgement: whether I seem to be talking while
//! muted ([`MutedTalk`]), and how long a reconnect has left before the call is given up.

use crate::gui::theme;
use gpui::{div, prelude::*, px, Div, Rgba};
use protocol::{CallPath, CallStats};
use std::time::{Duration, Instant};

/// How long a Tiny call may be reconnecting before mediad gives it up as lost (its
/// `COME_BACK`, 07's state chart). A Discord call resumes a few times instead, with no clock.
pub(in crate::gui) const RECONNECT_LIMIT: i64 = 30;

/// The link and the sound, as far as `CallStats` says. `None` is "not measured", never zero.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(in crate::gui) struct Health {
    pub rtt_ms: Option<u32>,
    pub jitter_ms: Option<u32>,
    pub loss_permille: Option<u16>,
    pub path: CallPath,
    /// Microphone audio dropped before it was sent (capture overruns), over the stats window.
    pub overruns: Option<u32>,
    /// Times playback ran dry (output underruns), over the stats window.
    pub underruns: Option<u32>,
    /// Received audio filled in by concealment, per thousand frames.
    pub concealed_permille: Option<u16>,
    /// Lost audio recovered by forward error correction, per thousand frames.
    pub fec_permille: Option<u16>,
}

impl Health {
    /// What `stats` says. The fields `CallStats` does not carry yet stay `None`.
    // CONTRACT: when the audio pipeline adds `overruns`, `underruns`, `concealed_permille` and
    // `fec_permille` to `CallStats` (counts over the same window as `loss_permille`), set them
    // here with `Some(stats.field)`; nothing else in the shell changes.
    pub fn from_stats(stats: &CallStats) -> Health {
        let measured = |value: u32| (value > 0).then_some(value);
        // A link with a round trip has measured its loss and jitter too, even when they are 0.
        let link = stats.rtt_ms > 0 || stats.loss_permille > 0 || stats.jitter_ms > 0;
        Health {
            rtt_ms: measured(stats.rtt_ms),
            jitter_ms: link.then_some(stats.jitter_ms),
            loss_permille: link.then_some(stats.loss_permille),
            path: stats.path,
            ..Health::default()
        }
    }

    pub fn grade(&self) -> Grade {
        let (Some(rtt), Some(loss)) = (self.rtt_ms, self.loss_permille) else {
            return Grade::Unknown;
        };
        let jitter = self.jitter_ms.unwrap_or(0);
        if rtt > 250 || loss > 50 || jitter > 60 {
            Grade::Poor
        } else if rtt > 120 || loss > 15 || jitter > 30 {
            Grade::Fair
        } else if rtt > 60 || loss > 5 || jitter > 15 {
            Grade::Good
        } else {
            Grade::Great
        }
    }

    /// What to tell the user, most serious first. Empty while nothing is measured; a healthy
    /// call says so in one positive line.
    pub fn hints(&self) -> Vec<Hint> {
        let mut out = Vec::new();
        let mut say = |level, words: String| out.push(Hint { level, words });
        if self.overruns.is_some_and(|n| n > 0) {
            say(Level::Bad, "Your microphone is dropping audio. Try another microphone, or close other apps using it.".to_owned());
        }
        if self.underruns.is_some_and(|n| n > 0) {
            say(Level::Bad, "Your speaker is running dry, so voices break up. Try another speaker or headset.".to_owned());
        }
        match self.loss_permille {
            Some(loss) if loss > 50 => say(
                Level::Bad,
                format!("Your connection is losing {} of the audio. Move closer to Wi-Fi, or use a cable.", percent(loss)),
            ),
            Some(loss) if loss > 15 => say(Level::Warn, format!("Some audio is lost on the way ({}). Others may hear you cut out.", percent(loss))),
            _ => {}
        }
        if self.concealed_permille.is_some_and(|n| n > 30) {
            say(Level::Warn, "Parts of what others say are being filled in. Their connection may be weak.".to_owned());
        }
        if self.jitter_ms.is_some_and(|ms| ms > 30) {
            say(Level::Warn, "Audio is arriving unevenly. Downloads or streams on your network can cause this.".to_owned());
        }
        if self.rtt_ms.is_some_and(|ms| ms > 250) {
            say(Level::Warn, "The server is far away, so replies arrive late.".to_owned());
        }
        match self.path {
            CallPath::Tcp => say(Level::Info, "Audio goes over TCP because UDP is blocked. It may lag when your network is busy.".to_owned()),
            CallPath::Relay => say(Level::Info, "Audio goes through a relay because a direct path is blocked by a firewall or VPN.".to_owned()),
            CallPath::Udp | CallPath::Unknown => {}
        }
        if self.fec_permille.is_some_and(|n| n > 0) && out.iter().all(|hint| hint.level != Level::Bad) {
            out.push(Hint { level: Level::Info, words: "Error correction is repairing some lost audio.".to_owned() });
        }
        if out.is_empty() && self.grade() != Grade::Unknown {
            out.push(Hint { level: Level::Good, words: "Your connection is good.".to_owned() });
        }
        out
    }
}

/// `1.5 %`, from per mille.
fn percent(permille: u16) -> String {
    format!("{:.1} %", f32::from(permille) / 10.)
}

/// How the link is, as bars.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::gui) enum Grade {
    Unknown,
    Poor,
    Fair,
    Good,
    Great,
}

impl Grade {
    /// Bars lit of four.
    pub fn bars(self) -> u8 {
        match self {
            Grade::Unknown => 0,
            Grade::Poor => 1,
            Grade::Fair => 2,
            Grade::Good => 3,
            Grade::Great => 4,
        }
    }

    pub fn color(self) -> Rgba {
        match self {
            Grade::Unknown => theme::text_faint(),
            Grade::Poor => theme::danger(),
            Grade::Fair => theme::warning(),
            Grade::Good | Grade::Great => theme::positive(),
        }
    }

    pub fn words(self) -> &'static str {
        match self {
            Grade::Unknown => "Measuring the connection",
            Grade::Poor => "Poor connection",
            Grade::Fair => "Fair connection",
            Grade::Good => "Good connection",
            Grade::Great => "Excellent connection",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::gui) enum Level {
    Bad,
    Warn,
    Info,
    Good,
}

impl Level {
    pub fn color(self) -> Rgba {
        match self {
            Level::Bad => theme::danger(),
            Level::Warn => theme::warning(),
            Level::Info => theme::text_soft(),
            Level::Good => theme::positive(),
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Level::Bad | Level::Warn => "icons/triangle-alert.svg",
            Level::Info => "icons/info.svg",
            Level::Good => "icons/check.svg",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::gui) struct Hint {
    pub level: Level,
    pub words: String,
}

/// Four signal bars, `size` high, lit by `grade`. Unlit bars stay drawn, faint, so the shape is
/// always there and nothing moves when the grade changes.
pub(in crate::gui) fn bars(grade: Grade, size: f32) -> Div {
    let lit = grade.bars();
    let color = grade.color();
    let width = (size / 5.).max(2.);
    div().flex_none().h(px(size)).flex().items_end().gap(px((width / 2.).max(1.))).children((1..=4u8).map(move |bar| {
        div()
            .w(px(width))
            .h(px(size * f32::from(bar) / 4.))
            .rounded(px(1.))
            .bg(if bar <= lit { color } else { theme::selected() })
    }))
}

/// "Reconnecting… gives up in 0:22": the seconds left of [`RECONNECT_LIMIT`].
pub(in crate::gui) fn reconnect_left(elapsed: i64) -> i64 {
    (RECONNECT_LIMIT - elapsed.max(0)).max(0)
}

/// How long speech must last while muted before "You're muted" shows, and how long it stays.
const MUTED_TALK_AFTER: Duration = Duration::from_millis(300);
pub(in crate::gui) const MUTED_TALK_HOLD: Duration = Duration::from_secs(3);

/// Notices me talking while muted, from the microphone's level (which mediad measures while
/// muted only while the meter is on). A cough does not count; a sentence does.
#[derive(Default, Debug)]
pub(in crate::gui) struct MutedTalk {
    /// Since when the level has been over the gate.
    since: Option<Instant>,
    /// Shown until then.
    until: Option<Instant>,
}

impl MutedTalk {
    /// Feeds one reading. True while "You're muted" should show.
    pub fn feed(&mut self, muted: bool, level: Option<u8>, gate: u8, now: Instant) -> bool {
        if !muted {
            *self = MutedTalk::default();
            return false;
        }
        // At least a little over the gate: a gate of 0 would otherwise fire on silence.
        let loud = level.is_some_and(|level| level >= gate.max(8));
        if loud {
            let since = *self.since.get_or_insert(now);
            if now.duration_since(since) >= MUTED_TALK_AFTER {
                self.until = Some(now + MUTED_TALK_HOLD);
            }
        } else {
            self.since = None;
        }
        self.showing(now)
    }

    pub fn showing(&self, now: Instant) -> bool {
        self.until.is_some_and(|until| now < until)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(rtt_ms: u32, loss_permille: u16, jitter_ms: u32) -> Health {
        Health::from_stats(&CallStats { rtt_ms, loss_permille, jitter_ms, ..CallStats::default() })
    }

    #[test]
    fn the_grade_follows_the_worst_of_round_trip_loss_and_jitter() {
        assert_eq!(Health::default().grade(), Grade::Unknown);
        assert_eq!(link(0, 0, 0).grade(), Grade::Unknown, "nothing measured is not a perfect link");
        assert_eq!(link(38, 0, 4).grade(), Grade::Great);
        assert_eq!(link(80, 0, 4).grade(), Grade::Good);
        assert_eq!(link(38, 20, 4).grade(), Grade::Fair);
        assert_eq!(link(38, 0, 45).grade(), Grade::Fair);
        assert_eq!(link(400, 0, 4).grade(), Grade::Poor);
        assert_eq!(link(38, 80, 4).grade(), Grade::Poor);
        assert_eq!(Grade::Great.bars(), 4);
        assert_eq!(Grade::Poor.color(), theme::danger());
    }

    #[test]
    fn a_healthy_call_says_so_and_nothing_measured_says_nothing() {
        assert!(Health::default().hints().is_empty());
        assert_eq!(link(38, 0, 4).hints(), [Hint { level: Level::Good, words: "Your connection is good.".into() }]);
    }

    #[test]
    fn hints_name_the_trouble_in_words_most_serious_first() {
        let hints = link(300, 70, 40).hints();
        let levels: Vec<Level> = hints.iter().map(|hint| hint.level).collect();
        assert_eq!(levels, [Level::Bad, Level::Warn, Level::Warn]);
        assert!(hints[0].words.contains("7.0 %"), "{}", hints[0].words);
        let relayed = Health { path: CallPath::Relay, ..link(40, 0, 2) };
        assert_eq!(relayed.hints().len(), 1);
        assert!(relayed.hints()[0].words.contains("relay"));
    }

    #[test]
    fn the_audio_pipelines_counters_speak_only_when_present() {
        let without = link(40, 0, 2);
        assert!(without.hints().iter().all(|hint| !hint.words.contains("microphone")));
        let dropping = Health { overruns: Some(3), ..link(40, 0, 2) };
        assert_eq!(dropping.hints()[0].level, Level::Bad);
        assert!(dropping.hints()[0].words.starts_with("Your microphone is dropping audio"));
        let dry = Health { underruns: Some(1), overruns: Some(0), ..link(40, 0, 2) };
        assert!(dry.hints()[0].words.starts_with("Your speaker"));
        let repaired = Health { fec_permille: Some(12), ..link(40, 20, 2) };
        assert!(repaired.hints().iter().any(|hint| hint.words.starts_with("Error correction")));
        let filled = Health { concealed_permille: Some(60), ..link(40, 0, 2) };
        assert!(filled.hints().iter().any(|hint| hint.words.contains("filled in")));
    }

    #[test]
    fn a_reconnect_counts_down_to_the_limit_and_stops_at_zero() {
        assert_eq!(reconnect_left(0), 30);
        assert_eq!(reconnect_left(8), 22);
        assert_eq!(reconnect_left(45), 0);
        assert_eq!(reconnect_left(-3), 30);
    }

    #[test]
    fn talking_while_muted_needs_a_sentence_not_a_cough() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let mut talk = MutedTalk::default();
        assert!(!talk.feed(true, Some(60), 25, at(0)));
        assert!(!talk.feed(true, Some(10), 25, at(100)), "a cough");
        assert!(!talk.feed(true, Some(60), 25, at(200)));
        assert!(talk.feed(true, Some(60), 25, at(550)), "a sentence");
        assert!(talk.feed(true, Some(5), 25, at(1_000)), "it stays a moment after");
        assert!(!talk.showing(at(4_000)));
        assert!(!talk.feed(false, Some(90), 25, at(4_100)), "unmuted: never");
        assert!(!talk.feed(true, Some(3), 0, at(4_200)), "a gate of 0 still needs some sound");
        assert!(!talk.feed(true, None, 25, at(4_300)), "no meter, no word");
    }
}
