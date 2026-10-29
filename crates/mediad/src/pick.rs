//! Which device a choice means, given what the system lists right now.
//!
//! A choice ([`DeviceChoice`]) names a device by the system's id for it (a WASAPI endpoint id, a
//! CoreAudio UID, an ALSA name) and by the name it had when it was picked. The id is tried
//! first: two headsets of one model share a name, and Windows renames an endpoint when it moves
//! to another USB port. The name is the fallback, and all a choice stored by an older build has.
//!
//! What is never picked for the user: a virtual device (Krisp, VB-Cable, Voicemeeter, NVIDIA
//! Broadcast, Steam Streaming, ...). Those carry sound only while their app runs and routes it,
//! so a call that lands on one by itself is a call nobody hears. They are used when the user
//! picked one, or picked "System default" while one is the default.
//!
//! Everything here is pure over a [`Host`], so the choice, the fallback and the switch back
//! when a device returns are tested against a fake one.

use protocol::settings::DeviceChoice;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// One device the system lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub id: String,
    pub name: String,
    pub is_virtual: bool,
    /// How good a choice it is when nobody chose: lower is better (a headset, then headphones,
    /// then speakers, then anything else; a monitor's HDMI output last).
    pub rank: u8,
}

/// What lists devices: the system's, or a test's.
pub trait Host {
    fn endpoints(&self, input: bool) -> Vec<Endpoint>;
    /// The id of the system default (Windows' "Default Device", not "Default Communication").
    fn default_id(&self, input: bool) -> Option<String>;
}

/// Why the device in use is not the one chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Instead {
    /// The chosen device is not listed: unplugged, asleep, or switched off.
    Missing(String),
    /// It is listed and would not open; it is tried again later ([`Backoff`]).
    Failed(String),
}

/// The device to open for a choice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pick {
    pub endpoint: Endpoint,
    /// Set when this is a stand-in for the device the user chose.
    pub instead: Option<Instead>,
}

/// Names that mark a device as software rather than hardware. Matched case-insensitively
/// against the whole name, which on Windows includes the driver in brackets ("Krisp
/// Microphone (Krisp Audio)").
const VIRTUAL: &[&str] = &[
    "krisp",
    "vb-audio",
    "vb-cable",
    "cable input",
    "cable output",
    "voicemeeter",
    "nvidia broadcast",
    "nvidia rtx voice",
    "steam streaming",
    "virtual",
    "obs-",
    "obs virtual",
    "wave link",
    "steelseries sonar",
    "sonar -",
    "microsoft teams audio",
    "zoomaudiodevice",
    "zoom audio",
    "discord stream",
    "blackhole",
    "soundflower",
    "loopback audio",
    "manycam",
    "xsplit",
    "nahimic mirroring",
    "voicemod",
    "camo",
    "monitor of ",
];

/// True for software devices: a call never lands on one unless the user picked it.
pub fn is_virtual(name: &str) -> bool {
    let name = name.to_lowercase();
    VIRTUAL.iter().any(|mark| name.contains(mark))
}

/// The device a choice that names nothing would use: the system default when it is hardware,
/// else the best-ranked hardware, else the default whatever it is, else anything listed.
pub fn automatic<'a>(endpoints: &'a [Endpoint], default: Option<&str>, avoid: &[String]) -> Option<&'a Endpoint> {
    let usable = |e: &&Endpoint| !avoid.contains(&e.id);
    let default = endpoints.iter().filter(usable).find(|e| Some(e.id.as_str()) == default);
    if let Some(default) = default.filter(|e| !e.is_virtual) {
        return Some(default);
    }
    let hardware = endpoints.iter().filter(usable).filter(|e| !e.is_virtual).min_by_key(|e| e.rank);
    hardware.or(default).or_else(|| endpoints.iter().find(usable))
}

/// The device `choice` means now. `avoid` are devices that just failed to open. `None` when
/// nothing usable is listed at all.
pub fn resolve(choice: &DeviceChoice, endpoints: &[Endpoint], default: Option<&str>, avoid: &[String]) -> Option<Pick> {
    let pick = |endpoint: &Endpoint, instead| Some(Pick { endpoint: endpoint.clone(), instead });
    match choice {
        DeviceChoice::Auto => pick(automatic(endpoints, default, avoid)?, None),
        DeviceChoice::System => {
            let listed = endpoints.iter().find(|e| Some(e.id.as_str()) == default);
            match listed {
                Some(endpoint) if !avoid.contains(&endpoint.id) => pick(endpoint, None),
                // The default would not open: as for a device that failed.
                Some(endpoint) => pick(automatic(endpoints, None, avoid)?, Some(Instead::Failed(endpoint.name.clone()))),
                None => pick(automatic(endpoints, default, avoid)?, None),
            }
        }
        DeviceChoice::Device { name, id } => {
            let by_id = id.as_ref().and_then(|id| endpoints.iter().find(|e| &e.id == id));
            // A choice with an id falls back to the name only when no device has that id: the
            // same model on another port, or the endpoint Windows made anew after a driver update.
            let found = by_id.or_else(|| endpoints.iter().find(|e| &e.name == name));
            match found {
                Some(endpoint) if !avoid.contains(&endpoint.id) => pick(endpoint, None),
                Some(endpoint) => pick(automatic(endpoints, default, avoid)?, Some(Instead::Failed(endpoint.name.clone()))),
                None => pick(automatic(endpoints, default, avoid)?, Some(Instead::Missing(name.clone()))),
            }
        }
    }
}

/// What resolving `choice` on `host` gives now.
pub fn resolve_on(host: &dyn Host, choice: &DeviceChoice, input: bool, avoid: &[String]) -> Option<Pick> {
    resolve(choice, &host.endpoints(input), host.default_id(input).as_deref(), avoid)
}

/// The words for a device in use that is not the one chosen. `None` when it is.
pub fn notice(pick: &Pick) -> Option<String> {
    let using = &pick.endpoint.name;
    Some(match pick.instead.as_ref()? {
        Instead::Missing(name) => format!("{name} isn't connected. Using {using} until it's back."),
        Instead::Failed(name) => format!("{name} didn't start. Using {using} for now and trying it again."),
    })
}

/// The first wait before a device that would not open is tried again; it doubles each time.
const FIRST_RETRY: Duration = Duration::from_secs(3);
const LONGEST_RETRY: Duration = Duration::from_secs(30);

/// Devices that would not open, and when each is tried again.
#[derive(Debug, Default, Clone)]
pub struct Backoff {
    failed: HashMap<String, (Instant, u32)>,
}

impl Backoff {
    /// The devices not to try now.
    pub fn avoid(&self, now: Instant) -> Vec<String> {
        self.failed.iter().filter(|(_, (until, _))| *until > now).map(|(id, _)| id.clone()).collect()
    }

    pub fn failed(&mut self, id: &str, now: Instant) {
        let tries = self.failed.get(id).map_or(0, |(_, tries)| *tries);
        let wait = FIRST_RETRY.saturating_mul(1 << tries.min(4)).min(LONGEST_RETRY);
        self.failed.insert(id.to_owned(), (now + wait, tries + 1));
    }

    pub fn opened(&mut self, id: &str) {
        self.failed.remove(id);
    }

    /// The soonest a device now avoided may be tried again.
    pub fn next_retry(&self, now: Instant) -> Option<Instant> {
        self.failed.values().map(|(until, _)| *until).filter(|until| *until > now).min()
    }
}

/// Whether a device stream is doing its job, checked a moment after it opens: a speaker whose
/// callback never runs, or a microphone that delivers nothing, is silent without any error.
#[derive(Debug, Clone)]
pub struct Watch {
    since: Instant,
    /// The stream's count (callbacks, or samples delivered) when watching began.
    base: u64,
    /// It was rebuilt once for not running.
    rebuilt: bool,
    /// Seen running: nothing more to check until it is opened again.
    proven: bool,
}

/// What a [`Watch`] says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Running, or too early to tell.
    Fine,
    /// Silent past the grace time: rebuild it, once.
    Rebuild,
    /// Silent again after the rebuild: tell the user.
    Stuck,
}

/// How long a stream may take to start before it counts as not running.
pub const GRACE: Duration = Duration::from_millis(1500);

impl Watch {
    pub fn new(now: Instant, count: u64) -> Watch {
        Watch { since: now, base: count, rebuilt: false, proven: false }
    }

    /// The stream was opened again: watch it afresh, remembering whether this is the rebuild.
    pub fn reopened(&mut self, now: Instant, count: u64, rebuild: bool) {
        *self = Watch { since: now, base: count, rebuilt: rebuild, proven: false };
    }

    /// Not expected to run just now (a paused microphone): start the clock again.
    pub fn idle(&mut self, now: Instant, count: u64) {
        if !self.proven {
            self.since = now;
            self.base = count;
        }
    }

    pub fn check(&mut self, now: Instant, count: u64) -> Verdict {
        if self.proven {
            return Verdict::Fine;
        }
        if count > self.base {
            self.proven = true;
            return Verdict::Fine;
        }
        if now.duration_since(self.since) < GRACE {
            return Verdict::Fine;
        }
        if self.rebuilt {
            Verdict::Stuck
        } else {
            Verdict::Rebuild
        }
    }

    pub fn proven(&self) -> bool {
        self.proven
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// This machine's endpoints, as Windows lists them, with Krisp as the default.
    struct Fake {
        inputs: RefCell<Vec<Endpoint>>,
        outputs: RefCell<Vec<Endpoint>>,
        default_input: RefCell<Option<String>>,
        default_output: RefCell<Option<String>>,
    }

    fn endpoint(id: &str, name: &str, rank: u8) -> Endpoint {
        Endpoint { id: id.into(), name: name.into(), is_virtual: is_virtual(name), rank }
    }

    const HEADSET_MIC: &str = "Headset Microphone (CORSAIR HS55 Wireless Core Receiver)";
    const HEADSET_EAR: &str = "Headset Earphone (CORSAIR HS55 Wireless Core Receiver)";

    fn fake() -> Fake {
        Fake {
            inputs: RefCell::new(vec![
                endpoint("in-krisp", "Krisp Microphone (Krisp Audio)", 4),
                endpoint("in-usb", "Microphone (2- USB Audio Device)", 3),
                endpoint("in-hs55", HEADSET_MIC, 0),
            ]),
            outputs: RefCell::new(vec![
                endpoint("out-krisp", "Krisp Speaker (Krisp Audio)", 3),
                endpoint("out-dell", "Dell AW2518H (NVIDIA High Definition Audio)", 8),
                endpoint("out-hs55", HEADSET_EAR, 0),
                endpoint("out-realtek", "Realtek Digital Output (Realtek(R) Audio)", 8),
                endpoint("out-lg", "LG ULTRAGEAR (NVIDIA High Definition Audio)", 8),
                endpoint("out-usb", "Speakers (2- USB Audio Device)", 3),
            ]),
            default_input: RefCell::new(Some("in-krisp".into())),
            default_output: RefCell::new(Some("out-krisp".into())),
        }
    }

    impl Host for Fake {
        fn endpoints(&self, input: bool) -> Vec<Endpoint> {
            if input { self.inputs.borrow().clone() } else { self.outputs.borrow().clone() }
        }
        fn default_id(&self, input: bool) -> Option<String> {
            if input { self.default_input.borrow().clone() } else { self.default_output.borrow().clone() }
        }
    }

    fn id(host: &Fake, choice: &DeviceChoice, input: bool) -> String {
        resolve_on(host, choice, input, &[]).unwrap().endpoint.id
    }

    #[test]
    fn the_krisp_and_cable_kinds_are_virtual_and_hardware_is_not() {
        for name in ["Krisp Speaker (Krisp Audio)", "CABLE Input (VB-Audio Virtual Cable)", "VoiceMeeter Input (VB-Audio VoiceMeeter VAIO)", "Microphone (NVIDIA Broadcast)", "Speakers (Steam Streaming Speakers)"] {
            assert!(is_virtual(name), "{name}");
        }
        for name in [HEADSET_MIC, HEADSET_EAR, "Speakers (2- USB Audio Device)", "LG ULTRAGEAR (NVIDIA High Definition Audio)", "Realtek Digital Output (Realtek(R) Audio)"] {
            assert!(!is_virtual(name), "{name}");
        }
    }

    #[test]
    fn nobody_chose_so_a_virtual_default_is_passed_over_for_the_headset() {
        let host = fake();
        assert_eq!(id(&host, &DeviceChoice::Auto, true), "in-hs55");
        assert_eq!(id(&host, &DeviceChoice::Auto, false), "out-hs55", "not Krisp, and not a monitor");
        // A hardware default is simply the default.
        *host.default_output.borrow_mut() = Some("out-usb".into());
        assert_eq!(id(&host, &DeviceChoice::Auto, false), "out-usb");
    }

    #[test]
    fn system_default_is_a_choice_and_follows_the_default_even_onto_krisp() {
        let host = fake();
        assert_eq!(id(&host, &DeviceChoice::System, false), "out-krisp");
        *host.default_output.borrow_mut() = Some("out-usb".into());
        assert_eq!(id(&host, &DeviceChoice::System, false), "out-usb", "it moves with the system");
    }

    #[test]
    fn a_picked_virtual_device_is_honoured() {
        let host = fake();
        let krisp = DeviceChoice::Device { name: "Krisp Microphone (Krisp Audio)".into(), id: Some("in-krisp".into()) };
        assert_eq!(id(&host, &krisp, true), "in-krisp");
    }

    #[test]
    fn a_choice_is_found_by_id_first_then_by_name() {
        let host = fake();
        // Windows renamed the endpoint ("2- USB" became "3- USB"): the id still finds it.
        host.inputs.borrow_mut()[1].name = "Microphone (3- USB Audio Device)".into();
        let usb = DeviceChoice::Device { name: "Microphone (2- USB Audio Device)".into(), id: Some("in-usb".into()) };
        assert_eq!(id(&host, &usb, true), "in-usb");
        // A choice saved by an older build has only the name.
        let legacy = DeviceChoice::Device { name: HEADSET_MIC.into(), id: None };
        assert_eq!(id(&host, &legacy, true), "in-hs55");
        // The id is gone but a device of that name is there: the same model on another port.
        let moved = DeviceChoice::Device { name: HEADSET_MIC.into(), id: Some("in-old-port".into()) };
        let pick = resolve_on(&host, &moved, true, &[]).unwrap();
        assert_eq!((pick.endpoint.id.as_str(), pick.instead), ("in-hs55", None));
    }

    #[test]
    fn a_missing_device_falls_back_says_so_and_comes_back_when_it_returns() {
        let host = fake();
        let headset = DeviceChoice::Device { name: HEADSET_EAR.into(), id: Some("out-hs55".into()) };
        assert_eq!(id(&host, &headset, false), "out-hs55");
        // The headset is switched off: its endpoint goes from the list.
        let gone = host.outputs.borrow_mut().remove(2);
        let pick = resolve_on(&host, &headset, false, &[]).unwrap();
        assert_eq!(pick.endpoint.id, "out-usb", "hardware, never Krisp, never the monitor");
        assert_eq!(pick.instead, Some(Instead::Missing(HEADSET_EAR.into())));
        let words = notice(&pick).unwrap();
        assert!(words.starts_with(HEADSET_EAR) && words.contains("Speakers (2- USB Audio Device)"), "{words}");
        // It is switched on again: the same choice now means the headset.
        host.outputs.borrow_mut().push(gone);
        let pick = resolve_on(&host, &headset, false, &[]).unwrap();
        assert_eq!((pick.endpoint.id.as_str(), notice(&pick)), ("out-hs55", None));
    }

    #[test]
    fn a_device_that_would_not_open_is_stood_in_for_and_retried_later() {
        let host = fake();
        let headset = DeviceChoice::Device { name: HEADSET_MIC.into(), id: Some("in-hs55".into()) };
        let start = Instant::now();
        let mut backoff = Backoff::default();
        backoff.failed("in-hs55", start);
        let pick = resolve_on(&host, &headset, true, &backoff.avoid(start)).unwrap();
        assert_eq!(pick.endpoint.id, "in-usb");
        assert_eq!(pick.instead, Some(Instead::Failed(HEADSET_MIC.into())));
        assert_eq!(backoff.next_retry(start), Some(start + FIRST_RETRY));
        // Once the wait is over the headset is tried again.
        let later = start + FIRST_RETRY + Duration::from_millis(1);
        assert_eq!(resolve_on(&host, &headset, true, &backoff.avoid(later)).unwrap().endpoint.id, "in-hs55");
        // Failing again waits twice as long; opening forgets it.
        backoff.failed("in-hs55", later);
        assert_eq!(backoff.next_retry(later), Some(later + 2 * FIRST_RETRY));
        backoff.opened("in-hs55");
        assert!(backoff.avoid(later).is_empty());
    }

    #[test]
    fn with_only_virtual_devices_left_a_call_still_has_sound() {
        let host = fake();
        host.inputs.borrow_mut().retain(|e| e.is_virtual);
        assert_eq!(id(&host, &DeviceChoice::Auto, true), "in-krisp");
        host.inputs.borrow_mut().clear();
        assert_eq!(resolve_on(&host, &DeviceChoice::Auto, true, &[]), None);
    }

    #[test]
    fn a_stream_that_never_runs_is_rebuilt_once_and_then_reported() {
        let start = Instant::now();
        let mut watch = Watch::new(start, 0);
        assert_eq!(watch.check(start + Duration::from_millis(500), 0), Verdict::Fine, "still starting");
        assert_eq!(watch.check(start + GRACE, 0), Verdict::Rebuild);
        let rebuilt = start + GRACE;
        watch.reopened(rebuilt, 0, true);
        assert_eq!(watch.check(rebuilt + Duration::from_millis(100), 0), Verdict::Fine);
        assert_eq!(watch.check(rebuilt + GRACE, 0), Verdict::Stuck);
        // It starts after all: fine from then on.
        assert_eq!(watch.check(rebuilt + GRACE * 2, 5), Verdict::Fine);
        assert!(watch.proven());
        assert_eq!(watch.check(rebuilt + GRACE * 9, 5), Verdict::Fine);
    }

    #[test]
    fn a_running_stream_passes_and_a_paused_microphone_is_not_blamed() {
        let start = Instant::now();
        let mut watch = Watch::new(start, 10);
        // Paused for a long while (muted): the clock restarts each time.
        watch.idle(start + GRACE * 3, 10);
        assert_eq!(watch.check(start + GRACE * 3 + Duration::from_millis(10), 10), Verdict::Fine);
        assert_eq!(watch.check(start + GRACE * 3 + Duration::from_millis(20), 11), Verdict::Fine);
        assert!(watch.proven());
        // A device picked anew is watched anew, not as a rebuild.
        watch.reopened(start + GRACE * 4, 11, false);
        assert!(!watch.proven());
        assert_eq!(watch.check(start + GRACE * 6, 11), Verdict::Rebuild);
    }
}
