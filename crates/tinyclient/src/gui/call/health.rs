//! How a call is doing, in words and bars: the signal bars beside "Voice connected", the grade
//! of the link, and the plain-English hints of the Call health panel. Everything here is read
//! from `CallStats` (about once a second while a call is up) and never measured here.
//!
//! The audio pipeline's own counters (the microphone dropping audio, the speaker running dry,
//! audio concealed or recovered) come in `CallStats` as counts over the whole call; [`Health`]
//! takes what each grew by since the poll before, so a hint speaks of the last second and goes
//! when the trouble does. A field is `None` only when nothing was polled.
//!
//! One second says little: a single lost packet is 2 % of it. [`Steady`] is the same numbers over
//! several seconds, and a grade that takes its time to change, so the bars move when the call
//! does.
//!
//! Also here, because it is the same kind of judgement: whether I seem to be talking while
//! muted ([`MutedTalk`]).

use crate::gui::theme;
use gpui::{div, prelude::*, px, Div, Rgba};
use protocol::{CallPath, CallStats};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// The link and the sound, as far as `CallStats` says. `None` is "not measured", never zero.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(in crate::gui) struct Health {
    pub rtt_ms: Option<u32>,
    pub jitter_ms: Option<u32>,
    pub loss_permille: Option<u16>,
    pub path: CallPath,
    /// Microphone audio dropped or missing before it was sent (capture overruns and gaps), over
    /// the stats window.
    pub overruns: Option<u32>,
    /// Times playback ran dry (output underruns), over the stats window.
    pub underruns: Option<u32>,
    /// Received audio filled in by concealment, per thousand frames.
    pub concealed_permille: Option<u16>,
    /// Lost audio recovered by forward error correction, per thousand frames.
    pub fec_permille: Option<u16>,
    /// The grade as [`Steady`] holds it, over what one reading says.
    pub steady: Option<Grade>,
}

impl Health {
    /// What one poll says, its audio counters taken as grown from nothing: the first poll of a
    /// call, or where only the link is wanted.
    #[cfg(test)]
    pub fn from_stats(stats: &CallStats) -> Health {
        Health::from_polls(None, stats)
    }

    /// What `now` says, the audio counters as grown since `before` (the poll a second ago). A
    /// counter that went down started again (mediad reconnected): it counts from nothing.
    pub fn from_polls(before: Option<&CallStats>, now: &CallStats) -> Health {
        let measured = |value: u32| (value > 0).then_some(value);
        // A link with a round trip has measured its loss and jitter too, even when they are 0.
        let link = now.rtt_ms > 0 || now.loss_permille > 0 || now.jitter_ms > 0;
        let grew = |field: fn(&CallStats) -> u32| {
            let (then, now) = (before.map_or(0, field), field(now));
            if now >= then { now - then } else { now }
        };
        let overruns = grew(|s| s.capture_overruns).saturating_add(grew(|s| s.capture_gaps));
        let underruns = grew(|s| s.playout_underruns);
        // The window's lost frames were each either rebuilt (FEC) or made up: the voices' loss
        // is shared out between the two as they grew.
        let (concealed, recovered) = (grew(|s| s.concealed_frames), grew(|s| s.fec_recovered));
        let share = |part: u32| {
            let lost = u64::from(concealed) + u64::from(recovered);
            (u64::from(now.voice_loss_permille) * u64::from(part)).checked_div(lost).map_or(0, |n| n.min(1000) as u16)
        };
        Health {
            rtt_ms: measured(now.rtt_ms),
            jitter_ms: link.then_some(now.jitter_ms),
            loss_permille: link.then_some(now.loss_permille),
            path: now.path,
            overruns: Some(overruns),
            underruns: Some(underruns),
            concealed_permille: Some(share(concealed)),
            fec_permille: Some(share(recovered)),
            steady: None,
        }
    }

    pub fn grade(&self) -> Grade {
        self.steady.unwrap_or_else(|| self.reading())
    }

    /// What these numbers mean to someone listening. Opus with in-band FEC hides a percent of
    /// loss completely and three with little to notice; past six, words go. The round trip is to
    /// the server: speech takes about half of it each way, and talking over each other starts
    /// when that nears 300 ms end to end. The jitter buffer holds about three times the jitter,
    /// so 60 ms of it is the buffer's whole 200 ms.
    fn reading(&self) -> Grade {
        let (Some(rtt), Some(loss)) = (self.rtt_ms, self.loss_permille) else {
            return Grade::Unknown;
        };
        let jitter = self.jitter_ms.unwrap_or(0);
        if rtt > 300 || loss > 60 || jitter > 60 {
            Grade::Poor
        } else if rtt > 200 || loss > 30 || jitter > 30 {
            Grade::Fair
        } else if rtt > 100 || loss > 10 || jitter > 15 {
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
        // One late tick dries a short cushion. A headset is the cause only when it keeps happening.
        if self.underruns.is_some_and(|n| n >= 3) {
            say(Level::Bad, "Your speaker is running dry, so voices break up. Try another speaker or headset.".to_owned());
        }
        match self.loss_permille {
            Some(loss) if loss > 60 => say(
                Level::Bad,
                format!("Your connection is losing {} of the audio. Move closer to Wi-Fi, or use a cable.", percent(loss)),
            ),
            Some(loss) if loss > 30 => say(Level::Warn, format!("Some audio is lost on the way ({}). Others may hear you cut out.", percent(loss))),
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

/// Seconds of measured loss the grade looks at. Silent seconds are not among them.
const LOSS_WINDOW: usize = 8;
/// How much of each new round trip and jitter reading is taken in: about five seconds' memory.
const SMOOTHING: f32 = 0.2;
/// How long the numbers must say worse before the bars do.
const WORSE_AFTER: Duration = Duration::from_secs(3);
/// And better: a call that went bad has to prove itself for longer.
const BETTER_AFTER: Duration = Duration::from_secs(10);

/// The link over the last several seconds, fed each poll, and the grade it has earned. While
/// nothing flows nothing is known about loss or jitter, so they and the grade stay as they were.
#[derive(Debug, Default)]
pub(in crate::gui) struct Steady {
    rtt: Option<f32>,
    jitter: Option<f32>,
    /// Per mille, one reading for each measured second, newest last.
    loss: VecDeque<u16>,
    grade: Option<Grade>,
    /// What the numbers have been saying instead of `grade`, and since when.
    leaning: Option<(Grade, Instant)>,
}

impl Steady {
    /// One poll, read `now`.
    pub fn feed(&mut self, stats: &CallStats, now: Instant) {
        let smooth = |was: Option<f32>, reading: u32| Some(was.map_or(reading as f32, |was| was + (reading as f32 - was) * SMOOTHING));
        if stats.rtt_ms > 0 {
            self.rtt = smooth(self.rtt, stats.rtt_ms);
        }
        if stats.link_measured {
            self.jitter = smooth(self.jitter, stats.jitter_ms);
            self.loss.push_back(stats.loss_permille);
            if self.loss.len() > LOSS_WINDOW {
                self.loss.pop_front();
            }
        }
        let wanted = self.applied(Health { rtt_ms: self.rtt.map(|_| 0), loss_permille: Some(0), ..Health::default() }).reading();
        self.settle(wanted, now);
    }

    /// Moves the grade toward `wanted`, once it has been wanted for long enough.
    fn settle(&mut self, wanted: Grade, now: Instant) {
        let Some(held) = self.grade else {
            self.grade = (wanted != Grade::Unknown).then_some(wanted);
            return;
        };
        if wanted == held || wanted == Grade::Unknown {
            self.leaning = None;
            return;
        }
        let worse = |grade: Grade| grade.bars() < held.bars();
        let since = self.leaning.filter(|(was, _)| worse(*was) == worse(wanted)).map_or(now, |(_, since)| since);
        if now.saturating_duration_since(since) >= if worse(wanted) { WORSE_AFTER } else { BETTER_AFTER } {
            self.grade = Some(wanted);
            self.leaning = None;
        } else {
            self.leaning = Some((wanted, since));
        }
    }

    /// `health` with the steady numbers and grade in place of one reading's.
    pub fn applied(&self, health: Health) -> Health {
        let round = |value: f32| value.round() as u32;
        let mean = (!self.loss.is_empty()).then(|| (self.loss.iter().map(|&n| u32::from(n)).sum::<u32>() / self.loss.len() as u32) as u16);
        Health {
            rtt_ms: health.rtt_ms.map(|ms| self.rtt.map_or(ms, round)),
            jitter_ms: self.jitter.map(round).or(health.jitter_ms),
            loss_permille: mean.or(health.loss_permille),
            steady: self.grade,
            ..health
        }
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
        assert_eq!(link(80, 0, 4).grade(), Grade::Great);
        assert_eq!(link(150, 0, 4).grade(), Grade::Good);
        assert_eq!(link(38, 20, 4).grade(), Grade::Good, "one packet in fifty is not a Fair call");
        assert_eq!(link(38, 40, 4).grade(), Grade::Fair);
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
        let blip = Health { underruns: Some(1), overruns: Some(0), ..link(40, 0, 2) };
        assert!(blip.hints().iter().all(|hint| !hint.words.starts_with("Your speaker")));
        let dry = Health { underruns: Some(3), overruns: Some(0), ..link(40, 0, 2) };
        assert!(dry.hints()[0].words.starts_with("Your speaker"));
        let repaired = Health { fec_permille: Some(12), ..link(40, 20, 2) };
        assert!(repaired.hints().iter().any(|hint| hint.words.starts_with("Error correction")));
        let filled = Health { concealed_permille: Some(60), ..link(40, 0, 2) };
        assert!(filled.hints().iter().any(|hint| hint.words.contains("filled in")));
    }

    #[test]
    fn the_audio_counters_speak_of_the_last_second_not_the_whole_call() {
        let poll = |overruns, gaps, underruns, concealed, recovered, voice_loss_permille| CallStats {
            rtt_ms: 40,
            voice_loss_permille,
            capture_overruns: overruns,
            capture_gaps: gaps,
            playout_underruns: underruns,
            concealed_frames: concealed,
            fec_recovered: recovered,
            ..CallStats::default()
        };
        let first = poll(2, 1, 0, 3, 1, 40);
        let health = Health::from_polls(None, &first);
        assert_eq!((health.overruns, health.underruns), (Some(3), Some(0)), "the first poll counts from nothing");
        assert_eq!((health.concealed_permille, health.fec_permille), (Some(30), Some(10)), "loss shared as it grew");
        // A second later nothing more went wrong: the hints go, though the call's totals stay.
        let quiet = Health::from_polls(Some(&first), &poll(2, 1, 0, 3, 1, 0));
        assert_eq!((quiet.overruns, quiet.underruns, quiet.concealed_permille, quiet.fec_permille), (Some(0), Some(0), Some(0), Some(0)));
        assert_eq!(quiet.hints(), [Hint { level: Level::Good, words: "Your connection is good.".into() }]);
        // Two dry moments in a second are a blip. Three is the speaker actually running out.
        let blip = Health::from_polls(Some(&first), &poll(2, 1, 2, 3, 1, 0));
        assert_eq!(blip.underruns, Some(2));
        assert!(blip.hints().iter().all(|hint| !hint.words.starts_with("Your speaker")));
        let dry = Health::from_polls(Some(&first), &poll(2, 1, 3, 3, 1, 0));
        assert_eq!(dry.underruns, Some(3));
        assert!(dry.hints()[0].words.starts_with("Your speaker"));
        // mediad reconnected and counts from nothing again.
        let again = Health::from_polls(Some(&first), &poll(1, 0, 0, 0, 0, 0));
        assert_eq!(again.overruns, Some(1));
    }

    /// Feeds `readings` a second apart from `start`, and gives the grade after each.
    fn graded(steady: &mut Steady, start: Instant, readings: &[CallStats]) -> Vec<Grade> {
        readings
            .iter()
            .enumerate()
            .map(|(n, stats)| {
                steady.feed(stats, start + Duration::from_secs(n as u64));
                steady.applied(Health::from_stats(stats)).grade()
            })
            .collect()
    }

    fn second(rtt_ms: u32, loss_permille: u16, jitter_ms: u32) -> CallStats {
        CallStats { rtt_ms, loss_permille, jitter_ms, link_measured: true, ..CallStats::default() }
    }

    #[test]
    fn a_packet_lost_now_and_then_does_not_move_the_bars() {
        // One packet of fifty is 20 per mille, and three in one bad second is 60: the old grade
        // read those as Fair and Poor, then back, second by second.
        let lost = |n: usize| match n % 16 {
            3 => 20,
            11 => 60,
            _ => 0,
        };
        let readings: Vec<CallStats> = (0..64).map(|n| second(40, lost(n), 4)).collect();
        let grades = graded(&mut Steady::default(), Instant::now(), &readings);
        assert!(grades.iter().all(|grade| *grade == Grade::Great), "{grades:?}");
    }

    #[test]
    fn a_bad_link_shows_in_seconds_and_takes_ten_to_be_forgiven() {
        let start = Instant::now();
        let mut steady = Steady::default();
        graded(&mut steady, start, &vec![second(40, 0, 4); 10]);
        let later = start + Duration::from_secs(10);
        let grades = graded(&mut steady, later, &vec![second(40, 200, 4); 6]);
        let first = grades.iter().position(|grade| *grade != Grade::Great).unwrap();
        assert!((2..=5).contains(&first), "{grades:?}");
        assert_eq!(grades[5], Grade::Poor, "{grades:?}");
        // Clean again. The window has to clear, and then the grade waits ten seconds.
        let later = later + Duration::from_secs(6);
        let grades = graded(&mut steady, later, &vec![second(40, 0, 4); 30]);
        assert!(grades[..14].iter().all(|grade| grade.bars() <= Grade::Fair.bars()), "{grades:?}");
        assert_eq!(grades[29], Grade::Great, "{grades:?}");
        let changes = grades.windows(2).filter(|pair| pair[0] != pair[1]).count();
        assert!(changes <= 3, "{grades:?}");
    }

    #[test]
    fn numbers_that_wander_over_a_line_do_not_flap_the_grade() {
        let readings: Vec<CallStats> = (0..120).map(|n| second(if n % 6 < 3 { 90 } else { 115 }, 0, 4)).collect();
        let grades = graded(&mut Steady::default(), Instant::now(), &readings);
        let changes = grades.windows(2).filter(|pair| pair[0] != pair[1]).count();
        assert!(changes <= 1, "{grades:?}");
    }

    #[test]
    fn silence_keeps_the_last_grade_and_its_numbers() {
        let start = Instant::now();
        let mut steady = Steady::default();
        graded(&mut steady, start, &vec![second(40, 100, 4); 8]);
        assert_eq!(steady.applied(Health::default()).steady, Some(Grade::Poor));
        // Nobody talks: mediad says 0, and says it measured nothing.
        let quiet = CallStats { rtt_ms: 40, ..CallStats::default() };
        let later = start + Duration::from_secs(8);
        let grades = graded(&mut steady, later, &vec![quiet.clone(); 60]);
        assert!(grades.iter().all(|grade| *grade == Grade::Poor), "{grades:?}");
        assert_eq!(steady.applied(Health::from_stats(&quiet)).loss_permille, Some(100));
    }

    #[test]
    fn a_round_trip_spike_is_smoothed_out_of_the_number_shown() {
        let start = Instant::now();
        let mut steady = Steady::default();
        graded(&mut steady, start, &vec![second(40, 0, 4); 10]);
        let spike = second(400, 0, 4);
        graded(&mut steady, start + Duration::from_secs(10), &[spike.clone()]);
        let shown = steady.applied(Health::from_stats(&spike)).rtt_ms.unwrap();
        assert!((60..=130).contains(&shown), "{shown}");
        assert_eq!(steady.applied(Health::default()).rtt_ms, None, "no reading, no number");
    }

    #[test]
    fn a_call_with_nobody_talking_yet_is_graded_by_its_round_trip() {
        let mut steady = Steady::default();
        steady.feed(&CallStats { rtt_ms: 40, ..CallStats::default() }, Instant::now());
        assert_eq!(steady.applied(Health::default()).steady, Some(Grade::Great));
        let mut steady = Steady::default();
        steady.feed(&CallStats::default(), Instant::now());
        assert_eq!(steady.applied(Health::default()).grade(), Grade::Unknown);
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
