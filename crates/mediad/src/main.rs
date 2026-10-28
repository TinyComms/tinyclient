use mediad::session::{self, Controls};
use mediad::{audio, out, permission, tiny};
use protocol::{
    media_process_needed, read_frame, Content, Direction, MediaRequest, MediaResponse, VideoStream,
    DEVICES_LIMIT,
};
use std::env;
use std::io::{self, ErrorKind};
use std::process::ExitCode;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread::{sleep, JoinHandle};
use std::time::{Duration, Instant};

/// This mediad carries voice: gateway, UDP, Opus, DAVE and the sound devices are linked in.
const ENGINE_LINKED: bool = true;

/// How often the system's devices are looked at for one plugged in or removed.
const DEVICES_EVERY: Duration = Duration::from_secs(2);

const NO_VIDEO: &str = "Screen sharing and video are not built yet. This call carries voice only.";
const ENGINE: &str = "The call engine could not start. Try again.";
const NOT_YET: &str = "The call isn't connected yet, so there's nothing to measure.";

/// macOS reads the reason for the microphone prompt from the Info.plist of the process it holds
/// responsible. Inside `tinyclient.app` that is the app, whose Info.plist says it. When mediad is
/// responsible for itself (its parents were started by hand and are gone), this copy, in the
/// section a single-file tool keeps its Info.plist in, is what the system reads; without one
/// the system kills the process the moment it opens the microphone.
#[used]
#[link_section = "__TEXT,__info_plist"]
static INFO_PLIST: [u8; include_bytes!("../Info.plist").len()] = *include_bytes!("../Info.plist");

/// The video codec sessions of one call. Each field is one session, so a second share replaces
/// the first instead of adding a tile. Bookkeeping only: video is not carried yet.
#[derive(Default)]
struct Video {
    share_encode: bool,
    share_decode: Option<String>,
    speaker_decode: Option<String>,
}

impl Video {
    fn streams(&self) -> Vec<VideoStream> {
        let decode = |content, source: &Option<String>| {
            source.clone().map(|source| VideoStream {
                direction: Direction::Decode,
                content,
                source,
            })
        };
        let encode = self.share_encode.then(|| VideoStream {
            direction: Direction::Encode,
            content: Content::Share,
            source: String::new(),
        });
        encode
            .into_iter()
            .chain(decode(Content::Share, &self.share_decode))
            .chain(decode(Content::Speaker, &self.speaker_decode))
            .collect()
    }
}

/// The call this process exists for.
#[derive(Default)]
struct Call {
    video: Video,
    controls: Arc<Controls>,
    voice: Option<JoinHandle<()>>,
    /// A Tiny server call's offer, out with the server and not yet answered.
    offer: Option<tiny::Offer>,
    /// Our user id on that server, and whether the call running is one.
    tiny: Option<String>,
}

impl Call {
    /// Every request gets exactly one reply.
    fn apply(&mut self, request: MediaRequest) -> MediaResponse {
        match request {
            MediaRequest::Streams => {}
            // CONTRACT: nothing tells the shell that a call is voice only, so asking for video
            // on a joined voice call is refused in words. Turning one off is never refused.
            // Before a join there is no engine in the call and the bookkeeping below stands,
            // which is what chatd's own lifecycle tests drive.
            MediaRequest::ShareSend { on: true }
            | MediaRequest::ShareWatch { source: Some(_) }
            | MediaRequest::Speaker { source: Some(_) }
                if self.voice.is_some() =>
            {
                return MediaResponse::Refused { detail: NO_VIDEO.to_owned() };
            }
            MediaRequest::ShareSend { on } => self.video.share_encode = on,
            MediaRequest::ShareWatch { source } => self.video.share_decode = source,
            MediaRequest::Speaker { source } => self.video.speaker_decode = source,
            MediaRequest::Join(join) => {
                if self.voice.is_some() {
                    return MediaResponse::Refused { detail: "This call is already connecting.".to_owned() };
                }
                self.controls.muted.store(join.muted, Ordering::Relaxed);
                self.controls.deafened.store(join.deafened, Ordering::Relaxed);
                // An audio-only engine took the call: it holds no video session to report.
                self.video = Video::default();
                let controls = self.controls.clone();
                let spawned = std::thread::Builder::new().name("voice".to_owned()).spawn(move || {
                    session::run(join, controls);
                    // The other side ended the call, or chatd hung up: either way we are done.
                    eprintln!("mediad: call over, exiting");
                    std::process::exit(0);
                });
                match spawned {
                    Ok(voice) => self.voice = Some(voice),
                    Err(err) => {
                        eprintln!("mediad: the voice thread did not start: {err}");
                        return MediaResponse::Refused { detail: ENGINE.to_owned() };
                    }
                }
                return MediaResponse::Done;
            }
            // The call moved. The voice thread drops the old connection and takes this one;
            // devices, mute and the call itself stay. Before any join it is simply the join.
            MediaRequest::Rejoin(join) => {
                if self.voice.is_none() {
                    return self.apply(MediaRequest::Join(join));
                }
                self.controls.rejoin(join);
                return MediaResponse::Done;
            }
            MediaRequest::Meter { on } => {
                self.controls.meter(on);
                return MediaResponse::Done;
            }
            MediaRequest::Audio { muted, deafened } => {
                self.controls.set_audio(muted, deafened);
                return MediaResponse::Done;
            }
            MediaRequest::Tune(tuning) => {
                self.controls.tune(tuning);
                return MediaResponse::Done;
            }
            MediaRequest::Cue { cue } => {
                self.controls.cue(cue);
                return MediaResponse::Done;
            }
            // A fresh offer for a Tiny call already running is its rejoin: the call keeps its
            // devices and carries on with the new connection once the answer comes.
            MediaRequest::TinyOffer(spec) => {
                if self.voice.is_some() && self.tiny.is_none() {
                    return MediaResponse::Refused { detail: "This call is already connecting.".to_owned() };
                }
                if self.voice.is_none() {
                    self.controls.muted.store(spec.muted, Ordering::Relaxed);
                    self.controls.deafened.store(spec.deafened, Ordering::Relaxed);
                }
                return match tiny::Offer::new(&spec) {
                    Ok(offer) => {
                        let sdp = offer.sdp().to_owned();
                        self.offer = Some(offer);
                        self.tiny = Some(spec.user_id);
                        MediaResponse::Offer { sdp }
                    }
                    Err(err) => {
                        eprintln!("mediad: the offer could not be made: {err}");
                        MediaResponse::Refused { detail: ENGINE.to_owned() }
                    }
                };
            }
            MediaRequest::TinyAnswer { sdp } => {
                let Some(offer) = self.offer.take() else {
                    return MediaResponse::Refused { detail: "No offer is waiting for an answer.".to_owned() };
                };
                let link = match offer.accept(&sdp) {
                    Ok(link) => link,
                    Err(err) => {
                        eprintln!("mediad: the server's answer was refused: {err}");
                        return MediaResponse::Refused { detail: "The server's audio setup didn't work with tinyclient. Try again.".to_owned() };
                    }
                };
                if self.voice.is_some() {
                    self.controls.tiny_rejoin(link);
                    return MediaResponse::Done;
                }
                self.video = Video::default();
                let (controls, user_id) = (self.controls.clone(), self.tiny.clone().unwrap_or_default());
                let spawned = std::thread::Builder::new().name("tiny".to_owned()).spawn(move || {
                    tiny::run(link, user_id, controls);
                    eprintln!("mediad: call over, exiting");
                    std::process::exit(0);
                });
                match spawned {
                    Ok(voice) => self.voice = Some(voice),
                    Err(err) => {
                        eprintln!("mediad: the call thread did not start: {err}");
                        return MediaResponse::Refused { detail: ENGINE.to_owned() };
                    }
                }
                return MediaResponse::Done;
            }
            MediaRequest::TinySlots { audio } => {
                self.controls.tiny_slots(audio);
                return MediaResponse::Done;
            }
            MediaRequest::Devices { input, output } => {
                self.controls.choose(input, output);
                return MediaResponse::Done;
            }
            // The sound of the call, as the shell and the room decide it. Each holds until sent
            // again, through a reconnect or a move, and is read by whichever loop carries the
            // call: Gain and Priority in its mix (`hear`; a Tiny call names voices through the
            // slot map), Input and Talk at its gate (`talk`), Processing on the microphone
            // (`process`), Bitrate at its encoder.
            MediaRequest::Gain { user_id, percent, muted } => {
                self.controls.gain(user_id, percent, muted);
                return MediaResponse::Done;
            }
            MediaRequest::Input(mode) => {
                self.controls.set_input(mode);
                return MediaResponse::Done;
            }
            MediaRequest::Talk { held, priority } => {
                self.controls.talk(held, priority);
                return MediaResponse::Done;
            }
            MediaRequest::Priority { user_ids } => {
                self.controls.priority(user_ids);
                return MediaResponse::Done;
            }
            MediaRequest::Processing(processing) => {
                self.controls.set_processing(processing);
                return MediaResponse::Done;
            }
            MediaRequest::Bitrate { kbps } => {
                self.controls.set_bitrate(kbps);
                return MediaResponse::Done;
            }
            // Measured once a second by the call's loop (`stats`).
            MediaRequest::Stats => {
                return match self.controls.stats() {
                    Some(stats) if self.voice.is_some() => MediaResponse::Stats(stats),
                    _ => MediaResponse::Refused { detail: NOT_YET.to_owned() },
                };
            }
            MediaRequest::Loopback { .. } => return MediaResponse::Refused { detail: "A call can't play your microphone back to you.".to_owned() },
        }
        MediaResponse::Streams { video: self.video.streams() }
    }
}

/// Serves chatd until it closes our stdin, which is how a call ends.
fn serve() -> io::Result<()> {
    let mut stdin = io::stdin().lock();
    out::send(&MediaResponse::Ready { engine_linked: ENGINE_LINKED });
    let mut call = Call::default();
    let controls = call.controls.clone();
    // Ends with the process. A mediad that cannot start it has no device menu, and still calls.
    let _ = std::thread::Builder::new().name("devices".to_owned()).spawn(move || watch_devices(&controls));
    loop {
        let request: MediaRequest = match read_frame(&mut stdin) {
            Ok(request) => request,
            Err(err) if err.kind() == ErrorKind::UnexpectedEof => break,
            Err(err) => return Err(err),
        };
        out::send(&call.apply(request));
    }
    // Hang-up. The voice thread says goodbye to the network and exits the process itself; if
    // the network is slow about it, we leave anyway, well inside chatd's two seconds.
    call.controls.hangup.store(true, Ordering::Relaxed);
    if call.voice.is_some() {
        let deadline = Instant::now() + Duration::from_millis(1200);
        while Instant::now() < deadline {
            sleep(Duration::from_millis(10));
        }
    }
    Ok(())
}

/// Tells chatd every device the system lists: now, whenever the list changes, and whenever the
/// device menu opens. Nothing is opened to find out.
fn watch_devices(controls: &Controls) {
    let mut told = None;
    let mut asked = true;
    loop {
        let mut listing = audio::listing();
        listing.inputs.truncate(DEVICES_LIMIT);
        listing.outputs.truncate(DEVICES_LIMIT);
        let now = (listing.inputs, listing.outputs);
        if asked || told.as_ref() != Some(&now) {
            out::send(&MediaResponse::DeviceList { inputs: now.0.clone(), outputs: now.1.clone() });
            told = Some(now);
        }
        asked = controls.listing_due(DEVICES_EVERY);
    }
}

/// `--devices`: every device, the defaults, whether macOS lets this process hear the
/// microphone, and which process macOS holds responsible for it, as one JSON object. Nothing is
/// opened and nobody is asked for permission.
fn list_devices() {
    let listing = audio::listing();
    let json = serde_json::json!({
        "inputs": listing.inputs,
        "outputs": listing.outputs,
        "default_input": listing.default_input,
        "default_output": listing.default_output,
        // The answer is about whoever `responsible` names, not about mediad as such.
        "microphone": match permission::microphone() {
            Some(permission::Microphone::Allowed) => "allowed",
            Some(permission::Microphone::Denied) => "denied",
            Some(permission::Microphone::Unasked) => "unasked",
            None => "unknown",
        },
        "responsible": permission::responsible(),
    });
    println!("{json}");
}

fn main() -> ExitCode {
    protocol::crash::install_panic_hook("mediad");
    let first = env::args().nth(1);
    match first.as_deref() {
        Some("--version") => {
            println!("mediad {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Some("--devices") => {
            list_devices();
            return ExitCode::SUCCESS;
        }
        // Who macOS would name in the microphone prompt. Touches no audio and no permission.
        Some("--responsible") => {
            println!("{}", permission::responsible().unwrap_or_default());
            return ExitCode::SUCCESS;
        }
        // One voice message or audio file, for chatd, until it ends or chatd closes stdin.
        Some("--play") => {
            return match mediad::play::run() {
                Ok(()) => ExitCode::SUCCESS,
                Err(err) => {
                    eprintln!("mediad: {err}");
                    ExitCode::from(1)
                }
            };
        }
        // Settings' microphone test and test sound, with no call (`probe`).
        Some("test") => {
            mediad::probe::run();
            eprintln!("mediad: test over, exiting");
            return ExitCode::SUCCESS;
        }
        _ => {}
    }
    let calls = first.and_then(|s| s.parse().ok()).unwrap_or(0);
    if !media_process_needed(calls) {
        eprintln!("mediad: no active call, exiting");
        return ExitCode::SUCCESS;
    }
    match serve() {
        Ok(()) => {
            eprintln!("mediad: call ended, exiting");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("mediad: {err}");
            ExitCode::from(1)
        }
    }
}
