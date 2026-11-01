//! `--shot` only: a made-up call for design review, so every face of the dock can be drawn with
//! no call, no mediad and no microphone. `TINYCLIENT_SHOT_CALL` names the state:
//! `ringing_out`, `incoming`, `connecting`, `joined`, `joined_speaking`, `joined_muted`,
//! `joined_deafened`, `joined_alone`, `joined_channel`, `reconnecting`, `ended`, `confirm`,
//! `incoming_strip`, `confirm_answer`, `join_call` (the header pill of a conversation whose
//! call is running), `devices_menu`, `people_menu`, and the failures `failed_noanswer`,
//! `failed_declined`, `failed_refused`, `failed_lost`, `failed_unreachable`, `failed_udp`,
//! `failed_mic`, `failed_nomic`, `failed_speaker`, `failed_engine`, `failed_dave`.
//!
//! On a Tiny server (`--mode tiny`), `tiny_` before a state puts the call in the room I sit in
//! on the demo's server instead: `tiny_joined`, `tiny_connecting`, `tiny_reconnecting`,
//! `tiny_joined_muted`, `tiny_muted_talking` (muted, and the meter hears speech),
//! `tiny_joined_speaking`, `tiny_poor` (a bad link), `tiny_failed_kicked`, `tiny_failed_lost`,
//! `tiny_failed_banned`, `tiny_ptt` (push to talk, key up) and `tiny_ptt_talking` (key held).
//!
//! A Tiny call is end-to-end encrypted, its group the room's seats. `tiny_e2ee_pending`,
//! `tiny_transport` and `tiny_check` (someone's verified key changed) show the indicator's other
//! faces; `tiny_security` opens the security panel from the channel's top bar,
//! `tiny_security_bar` from the column's compact bar. `tiny_poor`'s numbers carry audio trouble
//! too, for the health hints.

use super::security::Place;
use super::{now, Confirm, Ended, Menu, Subject};
use crate::gui::state::Snapshot;
use protocol::{
    Call, CallFailure, CallSecurity, CallState, ConversationKind, Key, Network, Occupant, Occupants, Participant, SecureMember,
    SecurityMode, Trust,
};

fn asked() -> Option<String> {
    std::env::var("TINYCLIENT_SHOT_CALL").ok().filter(|name| !name.is_empty())
}

/// The state asked for, without the `tiny_` that says where.
fn named() -> Option<String> {
    asked().map(|name| name.strip_prefix("tiny_").map_or_else(|| name.clone(), str::to_owned))
}

/// The made-up call is in a Tiny room: the shell puts it in the one I sit in.
pub(super) fn tiny() -> bool {
    asked().is_some_and(|name| name.starts_with("tiny_"))
}

/// Push to talk is on, and whether its key is held: `ptt` and `ptt_talking`.
pub(super) fn ptt() -> Option<bool> {
    match named()?.as_str() {
        "ptt" => Some(false),
        "ptt_talking" => Some(true),
        _ => None,
    }
}

/// I am talking into a muted microphone.
pub(super) fn muted_talking() -> bool {
    named().is_some_and(|name| name == "muted_talking")
}

/// The connection's numbers for a made-up Tiny call: a good link, or with `tiny_poor` a bad
/// one over TCP.
pub(super) fn stats() -> Option<protocol::CallStats> {
    let name = asked().filter(|name| name.starts_with("tiny_"))?;
    let poor = name == "tiny_poor";
    Some(protocol::CallStats {
        rtt_ms: if poor { 184 } else { 38 },
        jitter_ms: if poor { 41 } else { 4 },
        loss_permille: if poor { 62 } else { 2 },
        send_kbps: 66,
        receive_kbps: 182,
        path: if poor { protocol::CallPath::Tcp } else { protocol::CallPath::Udp },
        codec: "opus".to_owned(),
        // Counted over the call: drawn as the first poll, so as the last second's.
        playout_underruns: if poor { 2 } else { 0 },
        concealed_frames: if poor { 4 } else { 0 },
        fec_recovered: if poor { 2 } else { 0 },
        ..protocol::CallStats::default()
    })
}

/// How the made-up Tiny call is protected: end to end, with everyone else in the room's call in
/// its group (`members` are their ids, from the room the shell puts the call in).
pub(super) fn security(members: &[String]) -> Option<CallSecurity> {
    if !tiny() {
        return None;
    }
    let name = named()?;
    let mode = match name.as_str() {
        "e2ee_pending" => SecurityMode::Pending,
        "transport" => SecurityMode::Transport,
        "blocked" => SecurityMode::Blocked,
        _ => SecurityMode::EndToEnd,
    };
    let end_to_end = mode == SecurityMode::EndToEnd;
    let digits = |seed: u64, count: usize| -> String {
        let mut x = seed;
        (0..count)
            .map(|_| {
                x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
                char::from(b'0' + (x >> 59) as u8 % 10)
            })
            .collect()
    };
    let members = if end_to_end {
        members
            .iter()
            .enumerate()
            .map(|(ix, user_id)| SecureMember {
                user_id: user_id.clone(),
                key: format!("{:064x}", ix + 1),
                safety: digits(ix as u64 + 11, 45),
                trust: match (ix, name.as_str()) {
                    (0, "check") => Trust::Changed,
                    (0, _) => Trust::Verified,
                    _ => Trust::Unverified,
                },
            })
            .collect()
    } else {
        Vec::new()
    };
    Some(CallSecurity {
        mode,
        code: if end_to_end { digits(7, 30) } else { String::new() },
        epoch: if end_to_end { 4 } else { 0 },
        members,
        unsupported: Vec::new(),
        server_lacks: mode == SecurityMode::Transport,
        required: false,
    })
}

/// Which indicator the security panel is open from, when a shot opens it.
pub(super) fn security_at() -> Option<Place> {
    match named()?.as_str() {
        "security" => Some(Place::TopBar),
        "security_bar" => Some(Place::TinyBar),
        _ => None,
    }
}

fn channel(name: &str) -> bool {
    matches!(name, "joined_channel" | "devices_menu" | "people_menu" | "confirm" | "incoming_strip" | "confirm_answer")
}

fn person(name: &str, speaking: bool, muted: bool, deafened: bool) -> Participant {
    Participant { id: name.to_lowercase().replace(' ', "-"), name: name.to_owned(), speaking, muted, deafened, ..Participant::default() }
}

fn key(id: &str) -> Key {
    Key { network: Network::Discord, account: "shot".to_owned(), id: id.to_owned() }
}

pub(super) fn call(snapshot: &Snapshot) -> Option<Call> {
    let name = named()?;
    if matches!(name.as_str(), "ended" | "join_call") {
        return None;
    }
    let failed = |reason, detail: &str| CallState::Failed { reason, detail: detail.to_owned() };
    // The sentences are the ones chatd, the adapter and mediad send.
    let state = match name.as_str() {
        "ringing_out" => CallState::RingingOut,
        "incoming" => CallState::RingingIn { from: snapshot.open.as_ref().map_or_else(|| "Mara Okafor".to_owned(), |open| open.title.clone()) },
        "connecting" => CallState::Connecting,
        "reconnecting" => CallState::Reconnecting,
        "failed_noanswer" => failed(CallFailure::NoAnswer, "Nobody picked up."),
        "failed_declined" => failed(CallFailure::NoAnswer, "They declined the call."),
        "failed_refused" => failed(CallFailure::Refused, "Discord refused the voice connection. Try again, or take this call in Discord."),
        "failed_lost" => failed(CallFailure::Lost, "The connection to Discord dropped and didn't come back. You've left the call."),
        "failed_unreachable" => failed(
            CallFailure::Lost,
            "Couldn't reach Discord's voice server. Check your internet connection, then try again.",
        ),
        "failed_udp" => failed(
            CallFailure::Lost,
            "Discord's voice server answered, but call audio couldn't get through. A VPN or firewall may be blocking it.",
        ),
        "failed_mic" => failed(
            CallFailure::Audio,
            "Allow it in System Settings › Privacy & Security › Microphone, then call again.",
        ),
        "failed_nomic" => failed(CallFailure::Audio, "Plug in a microphone, or choose another in Microphone and speaker."),
        "failed_speaker" => failed(
            CallFailure::Audio,
            "The speaker didn't start. Reconnect it or choose another in Microphone and speaker, then try again.",
        ),
        "failed_engine" => failed(CallFailure::Engine, "The call engine stopped."),
        "failed_kicked" => failed(CallFailure::Refused, "You were removed from the call."),
        "failed_banned" => failed(CallFailure::Refused, "You were banned from this server."),
        "failed_dave" => failed(
            CallFailure::Encryption,
            "Discord raised its encryption requirement. Update tinyclient, or take this call in Discord.",
        ),
        _ => CallState::Joined,
    };
    // The open conversation's, so the header pill reads Leave or Cancel as it would.
    let conversation = snapshot.open.as_ref().map_or_else(|| key("shot"), |open| open.key.clone());
    let muted = matches!(name.as_str(), "joined_muted" | "muted_talking");
    let participants = if tiny() {
        // A Tiny stage draws the room's seats, not the call's roster.
        Vec::new()
    } else if channel(&name) {
        vec![
            person("Dev Patel", false, false, false),
            person("Iris Navarro", false, true, true),
            person("Mara Okafor", true, false, false),
            person("Tomas Lindqvist", false, true, false),
            person("Jun Aoki", false, false, false),
        ]
    } else if name == "joined_alone" {
        Vec::new()
    } else {
        // With a conversation open the call is with it, so its own picture and name are drawn.
        let peer = snapshot.open.as_ref().map_or("Mara Okafor", |open| open.title.as_str());
        vec![person(peer, name == "joined", muted, false)]
    };
    let joined = match name.as_str() {
        "ringing_out" => now() - 12,
        "reconnecting" => now() - 8,
        "failed_lost" | "failed_kicked" | "failed_banned" => now() - 900,
        "incoming" | "connecting" => 0,
        name if name.starts_with("failed") => 0,
        name if channel(name) => now() - 271,
        _ => now() - 724,
    };
    let menu = name == "devices_menu";
    let speaking = matches!(name.as_str(), "joined_speaking" | "devices_menu" | "people_menu" | "ptt_talking");
    // The first is the system default and what the automatic choice uses; a "Krisp" one is virtual.
    let listed = |names: &[&str]| -> Vec<protocol::AudioDevice> {
        let device = |(ix, name): (usize, &&str)| protocol::AudioDevice {
            id: format!("shot-{name}"),
            name: (*name).to_owned(),
            is_virtual: name.starts_with("Krisp"),
            default: ix == 0,
            auto: ix == 0,
        };
        if menu { names.iter().enumerate().map(device).collect() } else { Vec::new() }
    };
    Some(Call {
        speaking,
        level: (menu || name == "muted_talking").then_some(62),
        output_level: menu.then_some(48),
        inputs: listed(&["MacBook Pro Microphone", "Studio headset", "Desk camera microphone", "Krisp Microphone (Krisp Audio)"]),
        outputs: listed(&["MacBook Pro Speakers", "Studio headset"]),
        input_id: "shot-MacBook Pro Microphone".to_owned(),
        output_id: "shot-MacBook Pro Speakers".to_owned(),
        device_notice: None,
        device_stuck: false,
        conversation,
        state,
        participants,
        muted: muted || name == "joined_deafened",
        deafened: name == "joined_deafened",
        joined,
        input: "MacBook Pro Microphone".to_owned(),
        output: "MacBook Pro Speakers".to_owned(),
        engine_linked: true,
        video: Vec::new(),
        security: None,
        picture: None,
    })
}

fn mara() -> Subject {
    Subject { title: "Mara Okafor".to_owned(), kind: ConversationKind::Direct, avatar: None, space: None }
}

/// Who the made-up call is with, or the voice channel of the channel shots. `None` with a
/// conversation open: the call is in it, and its row says who.
pub(super) fn subject(snapshot: &Snapshot) -> Option<Subject> {
    if tiny() {
        return None;
    }
    if channel(&named()?) {
        let space = Some("tinyclient-dev".to_owned());
        return Some(Subject { title: "Lounge".to_owned(), kind: ConversationKind::Voice, avatar: None, space });
    }
    snapshot.open.is_none().then(mara)
}

/// Whoever was left alone in `joined_alone`.
pub(super) fn left() -> Option<String> {
    (named()? == "joined_alone").then(|| "Mara Okafor".to_owned())
}

pub(super) fn menu() -> Option<Menu> {
    match named()?.as_str() {
        "devices_menu" => Some(Menu::Devices),
        "people_menu" => Some(Menu::People),
        "security" | "security_bar" => Some(Menu::Security),
        _ => None,
    }
}

/// The other side hung up after twelve minutes and four seconds.
pub(super) fn ended() -> Option<Ended> {
    (named()? == "ended").then(|| Ended { conversation: key("shot"), subject: mara(), seconds: 724 })
}

/// The question the card asks before leaving Lounge.
pub(super) fn confirm() -> Option<Confirm> {
    match named()?.as_str() {
        "confirm" => Some(Confirm { conversation: key("mara"), title: "Mara Okafor".to_owned(), answer: false }),
        "confirm_answer" => Some(Confirm { conversation: key("crew"), title: "Iris Navarro".to_owned(), answer: true }),
        _ => None,
    }
}

/// A ring that arrives during the call: the conversation it is in, and who is calling.
pub(super) fn strip() -> Option<(Confirm, String)> {
    let ring = Confirm { conversation: key("crew"), title: "Weekend crew".to_owned(), answer: true };
    (named()? == "incoming_strip").then(|| (ring, "Iris Navarro".to_owned()))
}

/// The open conversation's call is running and I am not in it.
pub(super) fn running() -> Option<Occupants> {
    let there = |name: &str| Occupant {
        person: key(&name.to_lowercase()),
        name: name.to_owned(),
        muted: false,
        deafened: false,
        streaming: false,
        speaking: false,
    };
    (named()? == "join_call").then(|| Occupants { people: vec![there("Iris Navarro"), there("Jun Aoki")], total: 3 })
}
