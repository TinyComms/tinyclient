//! The process's body, as a desktop `main` and an in-process mediad (`inproc`) both run it: the
//! call served over stdin and stdout (`serve`), and the other modes `main` dispatches on.

use crate::session::{self, Controls};
use crate::{audio, out, permission, picture, signal, telegram, tiny};
use protocol::{
    media_process_needed, read_frame, CallFailure, Content, Direction, MediaRequest, MediaResponse, SecurityMode, VideoStream,
    DEVICES_LIMIT,
};
use std::io::{self, ErrorKind};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread::{sleep, JoinHandle};
use std::time::{Duration, Instant};

/// This mediad carries voice: gateway, UDP, Opus, DAVE and the sound devices are linked in.
const ENGINE_LINKED: bool = true;

/// How often the system's devices are looked at for one plugged in or removed.
const DEVICES_EVERY: Duration = Duration::from_secs(2);

const ENGINE: &str = "The call engine could not start. Try again.";
const ENGINE_STOPPED: &str = "The call engine stopped. Try joining again.";
const NOT_YET: &str = "The call isn't connected yet, so there's nothing to measure.";
/// Once, when the server turns a call's end-to-end encryption off and the call is blocked.
const BLOCKED_NOTICE: &str = "The server turned this call's encryption off. Nothing is sent or played. Open the call's security panel to leave or decide.";

/// The video sessions of one call.
#[derive(Default)]
struct Video {
    share_encode: bool,
    camera_encode: bool,
    share_decodes: Vec<String>,
    speaker_decode: Option<String>,
}

impl Video {
    fn streams(&self) -> Vec<VideoStream> {
        let decode = |content: Content, source: &Option<String>| {
            source.clone().map(|source| VideoStream {
                direction: Direction::Decode,
                content,
                source,
            })
        };
        let encode = |on: bool, content: Content| {
            on.then(|| VideoStream { direction: Direction::Encode, content, source: String::new() })
        };
        encode(self.share_encode, Content::Share)
            .into_iter()
            .chain(encode(self.camera_encode, Content::Camera))
            .chain(self.share_decodes.iter().cloned().map(|source| VideoStream {
                direction: Direction::Decode,
                content: Content::Share,
                source,
            }))
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
    /// Go Live: one connection that sends the screen, and one per watched share keyed by stream owner.
    share_leg: Option<JoinHandle<()>>,
    watch_legs: std::collections::HashMap<String, JoinHandle<()>>,
    /// A Tiny server call's offer, out with the server and not yet answered.
    offer: Option<tiny::Offer>,
    /// Our user id on that server, and whether the call running is one.
    tiny: Option<String>,
    /// A Telegram group voice chat's local offer, waiting for `updateGroupCallConnection`.
    tg_group_offer: Option<telegram::GroupOffer>,
    /// A Telegram group screen-share presentation offer, waiting for `updateGroupCallConnection(presentation=true)` (#467).
    tg_presentation_offer: Option<telegram::PresentationOffer>,
    /// Active Telegram presentation transport thread (#467).
    tg_presentation_leg: Option<JoinHandle<()>>,
    /// Our user id in a Telegram group voice chat.
    tg_user_id: Option<String>,
    /// True while carrying a Telegram group call (used to allow headless test camera/share without hardware).
    tg_group_active: bool,
    /// A Signal call's pending offer, out with the remote peer and not yet answered.
    signal_offer: Option<signal::Offer>,
    /// Our user id and peer id for a Signal call.
    signal_peer: Option<(String, String)>,
}

impl Call {
    /// The Go Live leg this request replaces has to be gone before the next one starts, or the
    /// stop flag meant for it would end the new one.
    fn replace_leg(&mut self, watch: bool, stream_owner: Option<&str>) {
        self.controls.stop_leg(watch, stream_owner);
        if watch {
            if let Some(owner) = stream_owner {
                if let Some(running) = self.watch_legs.remove(owner) {
                    let _ = running.join();
                }
            } else {
                for (_, running) in self.watch_legs.drain() {
                    let _ = running.join();
                }
            }
        } else if let Some(running) = self.share_leg.take() {
            let _ = running.join();
        }
        self.controls.clear_leg(watch, stream_owner);
    }

    /// Every request gets exactly one reply.
    fn apply(&mut self, request: MediaRequest) -> MediaResponse {
        match request {
            MediaRequest::Streams => {}
            MediaRequest::ShareSend { on, screen, quality, show_cursor, sound, resolution, fps, .. } => {
                // What the share captures, chosen before the probe so the probe tries that one,
                // and what it spends. Before a call is up the request is only noted: the voice
                // tests ask then, and a headless run must not open a screen to answer them.
                // Sound is stored before the share flag, so the stream thread that sees the
                // share also sees whether its sound goes with it. Nothing here opens a loopback.
                if on {
                    picture::choose_screen(screen);
                    picture::choose_share_config(quality, resolution, fps);
                    picture::choose_cursor(show_cursor);
                }
                self.controls.set_share_sound(on && sound == Some(true));
                let headless_tg = self.tg_group_active && out::var_os("TINYCLIENT_MEDIAD_AUDIO").is_some();
                if on && (self.voice.is_some() || self.tiny.is_some()) && !headless_tg {
                    if let Err(words) = picture::probe_screen() {
                        return MediaResponse::Refused { detail: words };
                    }
                }
                self.video.share_encode = on;
                if self.voice.is_some() || self.tiny.is_some() {
                    self.controls.set_share(on);
                }
            }
            MediaRequest::Camera { on } => {
                if on {
                    self.controls.set_camera_preview(false);
                }
                let headless_tg = self.tg_group_active && out::var_os("TINYCLIENT_MEDIAD_AUDIO").is_some();
                if on && (self.voice.is_some() || self.tiny.is_some()) && !headless_tg {
                    if let Err(words) = picture::probe_camera() {
                        return MediaResponse::Refused { detail: words };
                    }
                }
                self.video.camera_encode = on;
                if self.voice.is_some() || self.tiny.is_some() {
                    self.controls.set_camera(on);
                }
            }
            MediaRequest::CameraPreview { on } => {
                if on && (self.voice.is_some() || self.tiny.is_some()) {
                    if let Err(words) = picture::probe_camera() {
                        return MediaResponse::Refused { detail: words };
                    }
                }
                self.controls.set_camera_preview(on);
                return MediaResponse::Done;
            }
            MediaRequest::CameraSettings(settings) => {
                picture::choose_camera_settings(settings);
                return MediaResponse::Done;
            }
            MediaRequest::ShareWatch { source } => {
                match &source {
                    Some(user_id) => tracing::info!(user_id = %user_id, "watch on"),
                    None => tracing::info!("watch off"),
                }
                self.video.share_decodes = source.clone().into_iter().collect();
                if self.voice.is_some() || self.tiny.is_some() {
                    self.controls.set_watch(source.is_some());
                    self.controls.set_watch_sources(
                        source
                            .map(|user_id| protocol::WatchSource {
                                user_id,
                                kind: protocol::PictureKind::Share,
                                size: protocol::WatchSize::Large,
                            })
                            .into_iter()
                            .collect(),
                    );
                }
            }
            MediaRequest::Watch { sources } => {
                let (max_large, max_small) = crate::video::default_budget();
                let (accepted, refused) = crate::video::apply_watch_budget(&sources, max_large, max_small);
                tracing::info!(accepted = accepted.len(), refused = refused.len(), "watch list");
                self.video.share_decodes = accepted
                    .iter()
                    .filter(|s| s.kind == protocol::PictureKind::Share)
                    .map(|s| s.user_id.clone())
                    .collect();
                if self.voice.is_some() || self.tiny.is_some() {
                    self.controls.set_watch(!accepted.is_empty());
                    self.controls.set_watch_sources(accepted);
                    out::send(&MediaResponse::WatchRefused { refused });
                }
            }
            // A Tiny room's SFU already follows the active speaker. The bookkeeping stays for a
            // call that has no engine yet, which is what chatd's lifecycle test drives. A
            // Discord call paints the person named here.
            MediaRequest::Speaker { source } => {
                if self.tiny.is_none() || self.voice.is_none() {
                    self.video.speaker_decode = source.clone();
                }
                if self.voice.is_some() && self.tiny.is_none() {
                    self.controls.set_speaker(source);
                }
            }
            MediaRequest::Join(join) => {
                if self.voice.is_some() {
                    return MediaResponse::Refused { detail: "This call is already connecting.".to_owned() };
                }
                self.controls.muted.store(join.muted, Ordering::Relaxed);
                self.controls.deafened.store(join.deafened, Ordering::Relaxed);
                // An audio-only engine took the call: it holds no video session to report.
                self.video = Video::default();
                let controls = self.controls.clone();
                let spawned = out::spawn(std::thread::Builder::new().name("voice".to_owned()), move || {
                    session::run(join, controls);
                    // The other side ended the call, or chatd hung up: either way we are done.
                    tracing::warn!("mediad: call over, exiting");
                    out::exit(0);
                });
                match spawned {
                    Ok(voice) => self.voice = Some(voice),
                    Err(err) => {
                        tracing::warn!("mediad: the voice thread did not start: {err}");
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
            // Beside the voice call. The thread returns; it does not exit the process.
            MediaRequest::Stream(join) => {
                let watch = join.watch;
                let owner_key = join.stream_owner.clone().unwrap_or_default();
                self.replace_leg(watch, watch.then_some(owner_key.as_str()));
                let controls = self.controls.clone();
                let name = if watch { "watch" } else { "share" };
                let spawned = out::spawn(std::thread::Builder::new().name(name.to_owned()), move || session::run(join, controls));
                match spawned {
                    Ok(leg) => {
                        if watch {
                            self.watch_legs.insert(owner_key, leg);
                        } else {
                            self.share_leg = Some(leg);
                        }
                    }
                    Err(err) => {
                        tracing::warn!("mediad: the stream thread did not start: {err}");
                        return MediaResponse::Refused { detail: ENGINE.to_owned() };
                    }
                }
                return MediaResponse::Done;
            }
            MediaRequest::StreamStop { watch, stream_owner } => {
                self.controls.stop_leg(watch, stream_owner.as_deref());
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
            MediaRequest::PlayFile { path, gain } => {
                if gain == 0 {
                    return MediaResponse::Done;
                }
                return match crate::decode::load_stereo(std::path::Path::new(&path)) {
                    Ok(samples) => {
                        self.controls.play_file(samples, gain);
                        MediaResponse::Done
                    }
                    Err(detail) => MediaResponse::Refused { detail },
                };
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
                // A fresh seat starts its encryption afresh; the server says after its answer
                // whether this call is end to end.
                self.controls.e2ee(|e2ee| e2ee.offer(&spec.user_id, spec.e2ee.as_ref()));
                self.controls.e2ee_report();
                return match tiny::Offer::new(&spec) {
                    Ok(offer) => {
                        let sdp = offer.sdp().to_owned();
                        self.offer = Some(offer);
                        self.tiny = Some(spec.user_id);
                        MediaResponse::Offer { sdp }
                    }
                    Err(err) => {
                        tracing::warn!("mediad: the offer could not be made: {err}");
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
                        tracing::warn!("mediad: the server's answer was refused: {err}");
                        return MediaResponse::Refused { detail: "The server's audio setup didn't work with tinyclient. Try again.".to_owned() };
                    }
                };
                if self.voice.is_some() {
                    self.controls.tiny_rejoin(link);
                    return MediaResponse::Done;
                }
                self.video = Video::default();
                let (controls, user_id) = (self.controls.clone(), self.tiny.clone().unwrap_or_default());
                let spawned = out::spawn(std::thread::Builder::new().name("tiny".to_owned()), move || {
                    out::or_fail(CallFailure::Engine, ENGINE_STOPPED, || tiny::run(link, user_id, controls));
                    tracing::warn!("mediad: call over, exiting");
                    out::exit(0);
                });
                match spawned {
                    Ok(voice) => self.voice = Some(voice),
                    Err(err) => {
                        tracing::warn!("mediad: the call thread did not start: {err}");
                        return MediaResponse::Refused { detail: ENGINE.to_owned() };
                    }
                }
                return MediaResponse::Done;
            }
            MediaRequest::TinySlots { audio } => {
                self.controls.tiny_slots(audio);
                return MediaResponse::Done;
            }
            MediaRequest::TinyRenegotiation { sdp } => {
                self.controls.picture_answer(sdp);
                return MediaResponse::Done;
            }
            MediaRequest::TinyPicture { picture } => {
                self.controls.set_picture(picture);
                return MediaResponse::Done;
            }
            MediaRequest::TinyPictures { pictures } => {
                self.controls.set_pictures(pictures);
                return MediaResponse::Done;
            }
            MediaRequest::TinyE2ee { frame } => {
                let blocked = self.controls.e2ee(|e2ee| {
                    let before = e2ee.security().mode;
                    e2ee.frame(&frame);
                    before != SecurityMode::Blocked && e2ee.security().mode == SecurityMode::Blocked
                });
                self.controls.e2ee_report();
                if blocked {
                    // Said once, so a call gone quiet is not taken for a network problem.
                    out::send(&MediaResponse::Notice { detail: BLOCKED_NOTICE.to_owned() });
                }
                return MediaResponse::Done;
            }
            MediaRequest::TinyTransport => {
                let chosen = self.controls.e2ee(|e2ee| e2ee.carry_on_unencrypted());
                self.controls.e2ee_report();
                return match chosen {
                    Ok(()) => MediaResponse::Done,
                    Err(why) => MediaResponse::Refused { detail: why.to_owned() },
                };
            }
            MediaRequest::TinyRoster { user_ids } => {
                self.controls.e2ee(|e2ee| e2ee.roster(user_ids));
                self.controls.e2ee_report();
                return MediaResponse::Done;
            }
            MediaRequest::TelegramCall(join) => {
                if self.voice.is_some() {
                    return MediaResponse::Refused { detail: "This call is already connecting.".to_owned() };
                }
                self.controls.muted.store(join.muted, Ordering::Relaxed);
                self.controls.deafened.store(join.deafened, Ordering::Relaxed);
                self.video = Video::default();
                let controls = self.controls.clone();
                let spawned = out::spawn(std::thread::Builder::new().name("tg-voice".to_owned()), move || {
                    out::or_fail(CallFailure::Engine, ENGINE_STOPPED, || telegram::run_direct(join, controls));
                    tracing::warn!("mediad: telegram call over, exiting");
                    out::exit(0);
                });
                match spawned {
                    Ok(voice) => self.voice = Some(voice),
                    Err(err) => {
                        tracing::warn!("mediad: the telegram call thread did not start: {err}");
                        return MediaResponse::Refused { detail: ENGINE.to_owned() };
                    }
                }
                return MediaResponse::Done;
            }
            MediaRequest::TelegramSignal { data } => {
                use base64::Engine as _;
                if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(&data) {
                    self.controls.push_tg_signal(bytes);
                }
                return MediaResponse::Done;
            }
            MediaRequest::TelegramGroupOffer(spec) => {
                if self.voice.is_some() {
                    return MediaResponse::Refused { detail: "This call is already connecting.".to_owned() };
                }
                self.controls.muted.store(spec.muted, Ordering::Relaxed);
                self.controls.deafened.store(spec.deafened, Ordering::Relaxed);
                let public_key = if spec.conference {
                    use base64::Engine as _;
                    let uid = spec.user_id.strip_prefix('u').and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
                    let pk = crate::telegram_e2ee::generate_public_key();
                    self.controls.init_tg_conference(uid, pk);
                    out::send(&MediaResponse::TinySecurity(protocol::CallSecurity {
                        mode: protocol::SecurityMode::Pending,
                        required: true,
                        ..protocol::CallSecurity::default()
                    }));
                    Some(base64::engine::general_purpose::STANDARD.encode(pk))
                } else {
                    self.controls.clear_tg_conference();
                    None
                };
                return match telegram::GroupOffer::new(&spec) {
                    Ok(offer) => {
                        let params = offer.params_json().to_owned();
                        let ssrc = offer.ssrc();
                        self.tg_group_offer = Some(offer);
                        self.tg_user_id = Some(spec.user_id);
                        MediaResponse::TelegramGroupJoin { params, ssrc, public_key }
                    }
                    Err(err) => {
                        tracing::warn!("mediad: telegram group offer failed: {err}");
                        self.controls.clear_tg_conference();
                        MediaResponse::Refused { detail: ENGINE.to_owned() }
                    }
                };
            }
            MediaRequest::TelegramGroupAnswer { params } => {
                let Some(offer) = self.tg_group_offer.take() else {
                    return MediaResponse::Refused { detail: "No Telegram group call offer is waiting for an answer.".to_owned() };
                };
                let link = match offer.accept(&params) {
                    Ok(link) => link,
                    Err(err) => {
                        tracing::warn!("mediad: telegram group transport answer refused: {err}");
                        return MediaResponse::Refused {
                            detail: "Telegram sent a group voice setup this tinyclient couldn't use. Try again.".to_owned(),
                        };
                    }
                };
                self.video = Video::default();
                let controls = self.controls.clone();
                let user_id = self.tg_user_id.clone().unwrap_or_default();
                let spawned = out::spawn(std::thread::Builder::new().name("tg-group".to_owned()), move || {
                    out::or_fail(CallFailure::Engine, ENGINE_STOPPED, || telegram::run_group(link, user_id, controls));
                    tracing::warn!("mediad: telegram group call over, exiting");
                    out::exit(0);
                });
                match spawned {
                    Ok(voice) => {
                        self.voice = Some(voice);
                        self.tg_group_active = true;
                    }
                    Err(err) => {
                        tracing::warn!("mediad: the telegram group call thread did not start: {err}");
                        return MediaResponse::Refused { detail: ENGINE.to_owned() };
                    }
                }
                return MediaResponse::Done;
            }
            MediaRequest::TelegramGroupSources { sources } => {
                self.controls.set_tg_sources(sources);
                return MediaResponse::Done;
            }
            MediaRequest::TelegramGroupPresentationOffer => {
                return match telegram::PresentationOffer::new() {
                    Ok(offer) => {
                        let params = offer.params_json().to_owned();
                        let ssrc = offer.ssrc();
                        self.tg_presentation_offer = Some(offer);
                        MediaResponse::TelegramGroupPresentationJoin { params, ssrc }
                    }
                    Err(err) => {
                        tracing::warn!("mediad: telegram presentation offer failed: {err}");
                        MediaResponse::Refused { detail: ENGINE.to_owned() }
                    }
                };
            }
            MediaRequest::TelegramGroupPresentationAnswer { params } => {
                let Some(offer) = self.tg_presentation_offer.take() else {
                    return MediaResponse::Refused {
                        detail: "No Telegram presentation offer is waiting for an answer.".to_owned(),
                    };
                };
                self.controls.set_tg_presentation_stop(true);
                if let Some(prev) = self.tg_presentation_leg.take() {
                    let _ = prev.join();
                }
                self.controls.set_tg_presentation_stop(false);
                let link = match offer.accept(&params) {
                    Ok(link) => link,
                    Err(err) => {
                        tracing::warn!("mediad: telegram presentation answer refused: {err}");
                        return MediaResponse::Refused {
                            detail: "Telegram sent a screen-share transport this tinyclient couldn't use.".to_owned(),
                        };
                    }
                };
                let controls = self.controls.clone();
                let spawned = out::spawn(std::thread::Builder::new().name("tg-present".to_owned()), move || {
                    telegram::run_presentation(link, controls);
                });
                match spawned {
                    Ok(handle) => self.tg_presentation_leg = Some(handle),
                    Err(err) => {
                        tracing::warn!("mediad: telegram presentation thread did not start: {err}");
                        return MediaResponse::Refused { detail: ENGINE.to_owned() };
                    }
                }
                return MediaResponse::Done;
            }
            MediaRequest::TelegramGroupPresentationLeave => {
                self.tg_presentation_offer = None;
                self.controls.set_tg_presentation_stop(true);
                self.controls.set_share(false);
                self.video.share_encode = false;
                if let Some(leg) = self.tg_presentation_leg.take() {
                    let _ = leg.join();
                }
                return MediaResponse::Done;
            }
            MediaRequest::TelegramGroupVideoSources { sources } => {
                self.controls.set_tg_video_sources(sources);
                return MediaResponse::Done;
            }
            MediaRequest::TelegramRtmpStart { channel_id } => {
                if self.voice.is_some() {
                    return MediaResponse::Refused { detail: "This call is already connecting.".to_owned() };
                }
                self.controls.muted.store(true, Ordering::Relaxed);
                self.controls.set_tg_rtmp_state(protocol::RtmpStreamState::Waiting);
                self.video = Video::default();
                let controls = self.controls.clone();
                let spawned = out::spawn(std::thread::Builder::new().name("tg-rtmp".to_owned()), move || {
                    out::or_fail(CallFailure::Engine, ENGINE_STOPPED, || {
                        telegram::run_rtmp_viewer(channel_id, controls)
                    });
                    tracing::warn!("mediad: telegram rtmp viewer over, exiting");
                    out::exit(0);
                });
                match spawned {
                    Ok(voice) => {
                        self.voice = Some(voice);
                        self.tg_group_active = true;
                    }
                    Err(err) => {
                        tracing::warn!("mediad: the telegram rtmp viewer thread did not start: {err}");
                        return MediaResponse::Refused { detail: ENGINE.to_owned() };
                    }
                }
                return MediaResponse::Done;
            }
            MediaRequest::TelegramRtmpSegment { time_ms, scale, video_channel, video_quality, data } => {
                use base64::Engine as _;
                if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(&data) {
                    self.controls.push_tg_rtmp_segment(telegram::RtmpSegmentInput {
                        time_ms,
                        scale,
                        video_channel,
                        video_quality,
                        bytes,
                    });
                }
                return MediaResponse::Done;
            }
            MediaRequest::TelegramRtmpState { state } => {
                self.controls.set_tg_rtmp_state(state);
                return MediaResponse::Done;
            }
            MediaRequest::TelegramConferenceBlocks { sub_chain_id, blocks } => {
                use base64::Engine as _;
                let mut latest_sec = None;
                for b64 in blocks {
                    let Ok(raw) = base64::engine::general_purpose::STANDARD.decode(&b64) else {
                        let blocked = protocol::CallSecurity::telegram_conference_blocked();
                        out::send(&MediaResponse::TinySecurity(blocked));
                        return MediaResponse::Refused {
                            detail: "Invalid base64 in conference E2EE chain block.".to_owned(),
                        };
                    };
                    match self.controls.apply_tg_conference_block(sub_chain_id, &raw) {
                        Ok(sec) => latest_sec = Some(sec),
                        Err((err, blocked)) => {
                            out::send(&MediaResponse::TinySecurity(blocked));
                            return MediaResponse::Refused { detail: err.to_string() };
                        }
                    }
                }
                if let Some(sec) = latest_sec {
                    out::send(&MediaResponse::TinySecurity(sec));
                }
                return MediaResponse::Done;
            }
            MediaRequest::SignalOffer(spec) => {
                if self.voice.is_some() {
                    return MediaResponse::Refused { detail: "This call is already connecting.".to_owned() };
                }
                self.controls.muted.store(spec.muted, Ordering::Relaxed);
                self.controls.deafened.store(spec.deafened, Ordering::Relaxed);
                return match signal::Offer::new(&spec) {
                    Ok(offer) => {
                        let sdp = offer.sdp().to_owned();
                        for candidate in offer.local_candidates() {
                            out::send(&MediaResponse::SignalIce { candidate: candidate.clone() });
                        }
                        self.signal_offer = Some(offer);
                        self.signal_peer = Some((spec.user_id, spec.peer_id));
                        MediaResponse::Offer { sdp }
                    }
                    Err(err) => {
                        tracing::warn!("mediad: signal offer failed: {err}");
                        MediaResponse::Refused { detail: ENGINE.to_owned() }
                    }
                };
            }
            MediaRequest::SignalAccept(spec) => {
                if self.voice.is_some() {
                    return MediaResponse::Refused { detail: "This call is already connecting.".to_owned() };
                }
                self.controls.muted.store(spec.muted, Ordering::Relaxed);
                self.controls.deafened.store(spec.deafened, Ordering::Relaxed);
                let answer = match signal::Answer::from_offer(&spec) {
                    Ok(answer) => answer,
                    Err(err) => {
                        tracing::warn!("mediad: signal offer could not be accepted: {err}");
                        return MediaResponse::Refused { detail: "The Signal call offer didn't work with tinyclient. Try again.".to_owned() };
                    }
                };
                let sdp = answer.sdp().to_owned();
                for candidate in answer.local_candidates() {
                    out::send(&MediaResponse::SignalIce { candidate: candidate.clone() });
                }
                let link = answer.into_link();
                self.video = Video::default();
                let (controls, user_id, peer_id) = (self.controls.clone(), spec.user_id, spec.peer_id);
                let spawned = out::spawn(std::thread::Builder::new().name("signal".to_owned()), move || {
                    out::or_fail(CallFailure::Engine, ENGINE_STOPPED, || signal::run(link, user_id, peer_id, controls));
                    tracing::warn!("mediad: signal call over, exiting");
                    out::exit(0);
                });
                match spawned {
                    Ok(voice) => {
                        self.voice = Some(voice);
                        return MediaResponse::Answer { sdp };
                    }
                    Err(err) => {
                        tracing::warn!("mediad: signal call thread did not start: {err}");
                        return MediaResponse::Refused { detail: ENGINE.to_owned() };
                    }
                }
            }
            MediaRequest::SignalAnswer { sdp } => {
                let Some(offer) = self.signal_offer.take() else {
                    return MediaResponse::Refused { detail: "No Signal offer is waiting for an answer.".to_owned() };
                };
                let link = match offer.accept(&sdp) {
                    Ok(link) => link,
                    Err(err) => {
                        tracing::warn!("mediad: signal answer refused: {err}");
                        return MediaResponse::Refused { detail: "The Signal call answer didn't work with tinyclient. Try again.".to_owned() };
                    }
                };
                self.video = Video::default();
                let (user_id, peer_id) = self.signal_peer.clone().unwrap_or_default();
                let controls = self.controls.clone();
                let spawned = out::spawn(std::thread::Builder::new().name("signal".to_owned()), move || {
                    out::or_fail(CallFailure::Engine, ENGINE_STOPPED, || signal::run(link, user_id, peer_id, controls));
                    tracing::warn!("mediad: signal call over, exiting");
                    out::exit(0);
                });
                match spawned {
                    Ok(voice) => {
                        self.voice = Some(voice);
                        return MediaResponse::Done;
                    }
                    Err(err) => {
                        tracing::warn!("mediad: signal call thread did not start: {err}");
                        return MediaResponse::Refused { detail: ENGINE.to_owned() };
                    }
                }
            }
            MediaRequest::SignalIce { candidate } => {
                if let Some(offer) = self.signal_offer.as_mut() {
                    let _ = offer.add_remote_candidate(&candidate);
                } else {
                    self.controls.push_signal_ice(candidate);
                }
                return MediaResponse::Done;
            }
            MediaRequest::Devices { input, output, camera } => {
                self.controls.choose(input, output);
                picture::choose_camera(camera);
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
            MediaRequest::Quality(quality) => {
                self.controls.set_quality(quality);
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
            MediaRequest::SaveLogs => {
                let path = protocol::log_policy::current()
                    .and_then(|guard| guard.export_requested().ok().flatten())
                    .map(|path| path.display().to_string())
                    .unwrap_or_default();
                return MediaResponse::LogsSaved { path };
            }
            MediaRequest::LogLevel { level } => {
                crate::relevel(&level);
                return MediaResponse::Done;
            }
        }
        MediaResponse::Streams { video: self.video.streams() }
    }
}

/// Serves chatd until it closes our stdin, which is how a call ends.
fn serve() -> io::Result<()> {
    let mut stdin = out::stdin();
    out::send(&MediaResponse::Ready { engine_linked: ENGINE_LINKED });
    let mut call = Call::default();
    let controls = call.controls.clone();
    // A call needs a certificate; it is made while this process waits for one.
    let _ = out::spawn(std::thread::Builder::new().name("warm-up".to_owned()), tiny::warm_up);
    // Ends with the process. A mediad that cannot start it has no device menu, and still calls.
    let _ = out::spawn(std::thread::Builder::new().name("devices".to_owned()), move || watch_devices(&controls));
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
    // In process nothing ends this thread with the call but the call itself.
    while !out::over() {
        let mut listing = audio::listing();
        listing.inputs.truncate(DEVICES_LIMIT);
        listing.outputs.truncate(DEVICES_LIMIT);
        // Only query camera names while the device menu or preview is open (never thumbnails,
        // and never under the audio stand-in used by automated voice tests).
        let cameras = if controls.metering() && std::env::var_os("TINYCLIENT_MEDIAD_AUDIO").is_none() {
            let mut cams = picture::cameras(None);
            cams.truncate(DEVICES_LIMIT);
            cams
        } else {
            Vec::new()
        };
        let now = (listing.inputs, listing.outputs, cameras);
        if asked || told.as_ref() != Some(&now) {
            out::send(&MediaResponse::DeviceList { inputs: now.0.clone(), outputs: now.1.clone(), cameras: now.2.clone() });
            told = Some(now);
        }
        asked = controls.listing_due(DEVICES_EVERY);
    }
}

/// `--devices`: every device, the defaults, whether macOS lets this process hear the
/// microphone, and which process macOS holds responsible for it, as one JSON object. Nothing is
/// opened and nobody is asked for permission. `inputs` and `outputs` are the names alone, as
/// older shells read them; `input_devices` and `output_devices` are `AudioDevice`s, with ids.
fn list_devices() {
    let listing = audio::listing();
    let names = |devices: &[protocol::AudioDevice]| devices.iter().map(|d| d.name.clone()).collect::<Vec<_>>();
    let json = serde_json::json!({
        "inputs": names(&listing.inputs),
        "outputs": names(&listing.outputs),
        "input_devices": listing.inputs,
        "output_devices": listing.outputs,
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
    out::print_line(&json.to_string());
}

/// Everything `main` does after logging is set up, given the arguments after the program name.
/// Answers the exit code. As a process `main` exits with it; in process `inproc` reports it.
pub fn run(args: &[String]) -> u8 {
    let first = args.first().cloned();
    match first.as_deref() {
        Some("--devices") => {
            list_devices();
            return 0;
        }
        // Who macOS would name in the microphone prompt. Touches no audio and no permission.
        // The share picker's list: every screen and window, with a small picture of each in the
        // folder named.
        Some("--screens") => {
            let folder = args.get(1).map(std::path::PathBuf::from);
            let screens = picture::screens(folder.as_deref());
            out::print_line(&serde_json::to_string(&screens).unwrap_or_else(|_| "[]".to_owned()));
            return 0;
        }
        // Settings' camera list: every camera, with a small picture from each in the folder
        // named. Each is opened for one frame, so this is not run while a call has one.
        Some("--cameras") => {
            let folder = args.get(1).map(std::path::PathBuf::from);
            let cameras = picture::cameras(folder.as_deref());
            out::print_line(&serde_json::to_string(&cameras).unwrap_or_else(|_| "[]".to_owned()));
            return 0;
        }
        Some("--responsible") => {
            out::print_line(&permission::responsible().unwrap_or_default());
            return 0;
        }
        // One voice message or audio file, for chatd, until it ends or chatd closes stdin.
        Some("--play") => {
            return match crate::play::run() {
                Ok(()) => 0,
                Err(err) => {
                    tracing::warn!("mediad: {err}");
                    1
                }
            };
        }
        // One notification sound, for chatd: the word, the volume, the speaker (`alert`).
        Some("--sound") => {
            return match crate::alert::run(&args[1..]) {
                Ok(()) => 0,
                Err(err) => {
                    tracing::warn!("mediad: {err}");
                    1
                }
            };
        }
        // One voice note recorded off the microphone, for chatd: into a file it named, until
        // it closes our stdin (`rec`).
        Some("--rec") => {
            let Some(path) = args.get(1).map(std::path::PathBuf::from) else {
                tracing::warn!("mediad: --rec needs a file to record to");
                return 2;
            };
            let input = args
                .get(2)
                .filter(|flag| flag.as_str() == "--input")
                .and_then(|_| args.get(3))
                .cloned();
            return match crate::rec::run(path, input) {
                Ok(()) => 0,
                Err(err) => {
                    tracing::warn!("mediad: {err}");
                    1
                }
            };
        }
        // One round video message: camera, square H.264, video-only MP4, at most a minute.
        Some("--rec-video") => {
            let Some(path) = args.get(1).map(std::path::PathBuf::from) else {
                tracing::warn!("mediad: --rec-video needs a file to record to");
                return 2;
            };
            let input = args.get(2).filter(|flag| flag.as_str() == "--input").and_then(|_| args.get(3)).cloned();
            return match crate::rec_video::run(path, input) {
                Ok(()) => 0,
                Err(err) => {
                    tracing::warn!("mediad: {err}");
                    1
                }
            };
        }
        // S13: checks a sound file the user chose and saves it beside the database.
        Some("--import-sound") => {
            let (Some(source), Some(id)) = (args.get(1), args.get(2)) else {
                tracing::warn!("mediad: --import-sound needs a file and an id");
                return 2;
            };
            let dir = protocol::support_dir();
            return match crate::alert::import(source, id, &dir) {
                Ok(()) => 0,
                Err(err) => {
                    // The shell shows this line as it is.
                    out::print_line(&err.to_string());
                    tracing::warn!("mediad: import: {err}");
                    1
                }
            };
        }
        // Settings' microphone test and test sound, with no call (`probe`).
        Some("test") => {
            crate::probe::run();
            tracing::warn!("mediad: test over, exiting");
            return 0;
        }
        _ => {}
    }
    let calls = first.and_then(|s| s.parse().ok()).unwrap_or(0);
    if !media_process_needed(calls) {
        tracing::warn!("mediad: no active call, exiting");
        return 0;
    }
    match serve() {
        Ok(()) => {
            tracing::warn!("mediad: call ended, exiting");
            0
        }
        Err(err) => {
            tracing::warn!("mediad: {err}");
            1
        }
    }
}
