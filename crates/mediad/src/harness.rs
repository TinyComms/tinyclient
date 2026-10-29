//! A call's audio path on virtual devices, in virtual time: a microphone and a speaker that run
//! on their own clocks (any rate, any channel count, a period of their own, off by some parts
//! per million), and the 20 ms tick woken the way the system's timer wakes it. Everything in
//! between is the real code: the callbacks, the rings, the resamplers and `Audio`'s capture and
//! play. Deterministic, so a test can hold the counts to exact numbers.
//!
//! The microphone hears a 220 Hz tone and the tick plays one; a dropped, repeated or spliced
//! piece of either shows as a click (a jump the tone cannot make), and every tick that finds no
//! frame from the microphone as a gap.

use crate::audio::Audio;
use crate::{CHANNELS, FRAME, FRAME_SAMPLES, RATE};

/// One run.
#[derive(Debug, Clone, Copy)]
pub struct Spec {
    pub mic: (u32, usize),
    pub speaker: (u32, usize),
    /// How far each device's clock is off ours: +100 runs 100 ppm fast.
    pub mic_ppm: f64,
    pub speaker_ppm: f64,
    /// A device period, as WASAPI's shared mode gives (10 ms).
    pub period_ms: f64,
    /// The timer's grain: the loop asks to wake at the tick and is woken at the next multiple
    /// of this. Windows' default is 15.625 ms; 1 after `timeBeginPeriod(1)`; 0 is exact.
    pub timer_ms: f64,
    /// What a tick costs, in ms of the loop's time (processing, codecs, the network).
    pub tick_cost_ms: f64,
    /// Up to this much more lateness on each wake, at random: a busy machine. The tick on this
    /// machine measured (`how_late_the_tick_comes`) a median of 12 ms late, 28 ms at p90, 52 ms
    /// at worst, while other builds ran.
    pub stall_ms: f64,
    /// The microphone flags a glitch this often, in seconds, as the user's did.
    pub xrun_every: Option<f64>,
    pub seconds: f64,
}

impl Default for Spec {
    fn default() -> Spec {
        Spec {
            mic: (48_000, 2),
            speaker: (48_000, 2),
            mic_ppm: 0.,
            speaker_ppm: 0.,
            period_ms: 10.,
            timer_ms: 0.,
            tick_cost_ms: 1.,
            stall_ms: 0.,
            xrun_every: None,
            seconds: 30.,
        }
    }
}

/// What a run measured, after the first second (start-up is not steady state).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Report {
    /// Ticks that found no microphone frame: a hole in what is sent.
    pub capture_gaps: u64,
    /// Places the captured tone jumps: audio dropped, repeated or spliced.
    pub capture_clicks: u64,
    /// The speaker ran dry.
    pub underruns: u64,
    /// Places the played tone jumps.
    pub playout_clicks: u64,
    /// Device frames of silence played after playing began.
    pub silence_ms: f64,
    /// The microphone said the device broke: the call would reopen both devices.
    pub broken: u64,
    /// The microphone's latency at the end: what was held between the device and the tick, ms.
    pub capture_backlog_ms: f64,
    /// And the speaker's.
    pub playout_backlog_ms: f64,
}

const TONE: f64 = 220.;
const AMPLITUDE: f64 = 0.5;
/// A second difference a 220 Hz tone at half scale can not make (it makes 4e-4 at 48 kHz).
const JUMP: f32 = 0.02;

/// Counts clicks in a mono signal: second differences over [`JUMP`], one per 10 ms at most.
pub fn clicks(signal: &[f32], rate: u32) -> u64 {
    let quiet = rate as usize / 100;
    let mut last = None;
    let mut count = 0;
    for n in 2..signal.len() {
        let d2 = signal[n] - 2. * signal[n - 1] + signal[n - 2];
        if d2.abs() > JUMP && last.is_none_or(|at| n - at > quiet) {
            count += 1;
            last = Some(n);
        }
    }
    count
}

pub fn run(spec: Spec) -> Report {
    let (mut audio, mut mic, mut speaker) = Audio::virtual_devices(spec.mic, spec.speaker);
    let ms = |frames: f64, rate: u32| frames * 1000. / f64::from(rate);
    let mic_period = (f64::from(spec.mic.0) * spec.period_ms / 1000.).round() as usize;
    let speaker_period = (f64::from(spec.speaker.0) * spec.period_ms / 1000.).round() as usize;
    // Each device's period, in our milliseconds.
    let mic_every = ms(mic_period as f64, spec.mic.0) / (1. + spec.mic_ppm * 1e-6);
    let speaker_every = ms(speaker_period as f64, spec.speaker.0) / (1. + spec.speaker_ppm * 1e-6);
    let steady = 1000.;
    let end = spec.seconds * 1000.;

    let (mut next_mic, mut next_speaker, mut next_xrun) = (mic_every, speaker_every, spec.xrun_every.map(|s| s * 1000.));
    // The loop: it ticks when it wakes at or past `due`, then asks to wake at the next.
    let (mut due, mut wake) = (20., 20.);
    let mut mic_phase = 0f64;
    let mut buffer = vec![0f32; mic_period * spec.mic.1];
    let mut played = vec![0f32; speaker_period * spec.speaker.1];
    let mut out_phase = 0f64;
    let mut captured: Vec<f32> = Vec::new();
    let mut heard: Vec<f32> = Vec::new();
    let mut report = Report::default();
    let mut started = false;
    let mut seed = 1u64;
    let mut frame = [0i16; FRAME_SAMPLES];
    loop {
        let now = next_mic.min(next_speaker).min(wake).min(next_xrun.unwrap_or(f64::MAX));
        if now >= end {
            break;
        }
        if Some(now) == next_xrun {
            mic.xrun();
            next_xrun = spec.xrun_every.map(|s| now + s * 1000.);
        } else if now == next_mic {
            for frame in buffer.chunks_exact_mut(spec.mic.1) {
                frame.fill((mic_phase.sin() * AMPLITUDE) as f32);
                mic_phase = (mic_phase + TONE * std::f64::consts::TAU / f64::from(spec.mic.0)) % std::f64::consts::TAU;
            }
            mic.deliver(&buffer);
            next_mic += mic_every;
        } else if now == next_speaker {
            speaker.render(&mut played);
            if now >= steady {
                heard.extend(played.chunks_exact(spec.speaker.1).map(|frame| frame[0]));
            }
            next_speaker += speaker_every;
        } else {
            // Woken: tick as many times as are due, each costing its time.
            let mut at = now;
            while at >= due {
                let got = audio.capture(&mut frame);
                if got {
                    started = true;
                    if at >= steady {
                        captured.extend(frame.iter().step_by(CHANNELS).map(|&s| f32::from(s) / 32768.));
                    }
                } else if started && at >= steady {
                    report.capture_gaps += 1;
                }
                for pair in frame.as_chunks_mut::<CHANNELS>().0 {
                    pair.fill((out_phase.sin() * AMPLITUDE * 32767.) as i16);
                    out_phase = (out_phase + TONE * std::f64::consts::TAU / f64::from(RATE)) % std::f64::consts::TAU;
                }
                audio.play(&frame);
                if audio.take_broken() && at >= steady {
                    report.broken += 1;
                }
                due += 20.;
                if at > due + 100. {
                    due = at + 20.;
                }
                at += spec.tick_cost_ms;
            }
            wake = if spec.timer_ms > 0. { (due / spec.timer_ms).ceil() * spec.timer_ms } else { due };
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            wake = wake.max(at) + spec.stall_ms * ((seed >> 40) as f64 / (1u64 << 24) as f64).powi(2);
        }
        if now < steady {
            report.underruns = speaker.underruns();
        }
    }
    report.underruns = speaker.underruns() - report.underruns;
    report.capture_clicks = clicks(&captured, RATE);
    report.playout_clicks = clicks(&heard, spec.speaker.0);
    let first = heard.iter().position(|&s| s != 0.).unwrap_or(heard.len());
    let mut run = 0;
    let mut silence = 0;
    for &s in &heard[first..] {
        if s == 0. {
            run += 1;
        } else {
            if run >= 4 {
                silence += run;
            }
            run = 0;
        }
    }
    report.silence_ms = ms(f64::from(silence), spec.speaker.0);
    let (capture, playout) = audio.backlog();
    report.capture_backlog_ms = ms(capture as f64, RATE);
    report.playout_backlog_ms = ms(playout as f64, spec.speaker.0);
    let _ = FRAME;
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The numbers the harness gives for each case, printed for the record (`--nocapture`).
    fn measure(name: &str, spec: Spec) -> Report {
        let report = run(spec);
        println!("{name}: {report:?}");
        report
    }

    #[test]
    fn the_harness_hears_a_click_where_one_is() {
        let mut tone: Vec<f32> = (0..4800).map(|n| (n as f32 * 220. * std::f32::consts::TAU / 48_000.).sin() * 0.5).collect();
        assert_eq!(clicks(&tone, 48_000), 0);
        tone.drain(1000..1480);
        assert_eq!(clicks(&tone, 48_000), 1);
    }

    #[test]
    fn measurements() {
        let windows = Spec { timer_ms: 15.625, ..Spec::default() };
        measure("48k, exact timer", Spec::default());
        measure("48k, Windows timer", windows);
        measure("44.1k mono mic, Windows timer", Spec { mic: (44_100, 1), speaker: (44_100, 2), ..windows });
        measure("mic -150 ppm, speaker +150 ppm, 1 ms timer", Spec { mic_ppm: -150., speaker_ppm: 150., timer_ms: 1., ..Spec::default() });
        measure("mic +150 ppm, speaker -150 ppm, 1 ms timer", Spec { mic_ppm: 150., speaker_ppm: -150., timer_ms: 1., ..Spec::default() });
        measure("xrun every 1.3 s", Spec { xrun_every: Some(1.3), ..windows });
        let busy = Spec { stall_ms: 50., seconds: 600., ..windows };
        measure("busy, 10 min", busy);
        measure("busy, 10 min, mic -100 ppm, speaker +100 ppm", Spec { mic_ppm: -100., speaker_ppm: 100., ..busy });
        measure("busy, 10 min, mic +100 ppm, speaker -100 ppm", Spec { mic_ppm: 100., speaker_ppm: -100., ..busy });
        measure("busy, 10 min, 44.1k, mic -100 ppm, speaker +100 ppm", Spec { mic: (44_100, 2), speaker: (44_100, 2), mic_ppm: -100., speaker_ppm: 100., ..busy });
    }
}

/// The tick as the system really wakes it: `mio`'s poll with a timeout, as the Tiny call's loop
/// waits. Gives how late each of `ticks` 20 ms ticks came, in ms, sorted.
pub fn real_ticks(ticks: usize) -> Vec<f64> {
    use std::time::{Duration, Instant};
    let mut poll = mio::Poll::new().unwrap();
    let mut events = mio::Events::with_capacity(4);
    let start = Instant::now();
    let mut next = start + Duration::from_millis(20);
    let mut late = Vec::with_capacity(ticks);
    while late.len() < ticks {
        let now = Instant::now();
        if now >= next {
            late.push((now - next).as_secs_f64() * 1000.);
            next += Duration::from_millis(20);
            continue;
        }
        let wait = next.saturating_duration_since(now).max(Duration::from_millis(1));
        let _ = poll.poll(&mut events, Some(wait));
    }
    late.sort_by(f64::total_cmp);
    late
}

#[cfg(test)]
mod timing {
    /// How late the real tick comes on this machine (`--nocapture --ignored`).
    #[test]
    #[ignore = "a measurement of this machine, not a check"]
    fn how_late_the_tick_comes() {
        let late = super::real_ticks(100);
        let at = |q: f64| late[((late.len() - 1) as f64 * q) as usize];
        println!("tick lateness ms: median {:.2}, p90 {:.2}, p99 {:.2}, max {:.2}", at(0.5), at(0.9), at(0.99), at(1.));
    }
}
