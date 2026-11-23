//! What a call stage holds, for any call (direct, group, Discord voice, or Tiny room): a grid of
//! everyone in the call and of every stream (someone's screen, else their camera), each stream a
//! tile of its own beside its owner's. Clicking a stream watches it and fills the stage with it,
//! the people in a strip under it, over it, or hidden; clicking it again, or Escape, is the grid
//! again. Clicking a person draws them large over a row of the rest. Pure, so the choices are
//! tested without a window.

use super::Subject;
use crate::gui::theme;
use crate::gui::tiny::{model, Page};
use protocol::{
    Call, CallState, Content, ConversationKind, Direction, Key, Network, Participant, PictureKind, TinyRoom,
    TinySeat,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

/// One person seated on the stage, whether from a Tiny room or from `Call::participants`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(in crate::gui) struct Seat {
    pub id: String,
    pub name: String,
    pub me: bool,
    pub speaking: bool,
    pub muted: bool,
    pub deafened: bool,
    pub server_muted: bool,
    pub server_deafened: bool,
    pub suppressed: bool,
    pub priority: bool,
    pub camera: bool,
    pub sharing: bool,
    pub hand_raised: bool,
    pub reaction: Option<String>,
}

impl Seat {
    /// Moderation and self mute/deafen labels for a tile's state row.
    pub(in crate::gui) fn states(&self) -> Vec<&'static str> {
        let mut states = Vec::new();
        if self.server_deafened {
            states.push("Server deafened");
        } else if self.deafened {
            states.push("Deafened");
        }
        if self.server_muted {
            states.push("Server muted");
        } else if self.suppressed {
            states.push("Can't speak here");
        } else if self.muted && !self.deafened {
            states.push("Muted");
        }
        if self.priority {
            states.push("Priority speaker");
        }
        if self.hand_raised {
            states.push("Hand raised");
        }
        states
    }

    /// Small corner glyphs for a tile or roster row.
    pub(in crate::gui) fn glyphs(&self) -> Vec<(&'static str, gpui::Rgba, &'static str)> {
        let mut out = Vec::new();
        if self.hand_raised {
            out.push(("icons/hand.svg", theme::warning(), "Hand raised"));
        }
        if self.priority {
            out.push(("icons/megaphone.svg", theme::accent_text(), "Priority speaker"));
        }
        if self.server_deafened {
            out.push(("icons/headphone-off.svg", theme::danger(), "Server deafened"));
        } else if self.deafened {
            out.push(("icons/headphone-off.svg", theme::text_faint(), "Deafened"));
        }
        if self.server_muted {
            out.push(("icons/mic-off.svg", theme::danger(), "Server muted"));
        } else if self.suppressed {
            out.push(("icons/mic-off.svg", theme::warning(), "Can't speak in this channel"));
        } else if self.muted && !self.deafened {
            out.push(("icons/mic-off.svg", theme::text_faint(), "Muted"));
        }
        if self.sharing {
            out.push(("icons/monitor-up.svg", theme::accent(), "Sharing their screen"));
        }
        if self.camera {
            out.push(("icons/video.svg", theme::accent(), "Camera on"));
        }
        out
    }
}

/// A call or voice room's stage model, independent of whether a Tiny server page is loaded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::gui) struct Stage {
    pub key: Key,
    pub title: String,
    pub kind: ConversationKind,
    pub seats: Vec<Seat>,
    pub video: bool,
}

/// Our own encode of `content` is active on `call`.
pub(in crate::gui) fn sending(call: &Call, content: Content) -> bool {
    call.video
        .iter()
        .any(|stream| stream.direction == Direction::Encode && stream.content == content)
}

/// What my call sends, when it is up.
pub(in crate::gui) fn sending_now(call: Option<&Call>) -> Option<Sending> {
    call.filter(|call| call.state == CallState::Joined).map(|call| Sending {
        share: sending(call, Content::Share),
        camera: sending(call, Content::Camera),
    })
}

/// What the person whose seat this is can be heard as, in my call: my own seat with what my
/// call says right now (it knows before the server echoes it), anyone else's with mediad's
/// speaking edge when it is quicker than the server's.
pub(in crate::gui) fn as_heard(seat: &TinySeat, me: bool, call: Option<&Call>) -> TinySeat {
    let mut seat = seat.clone();
    let Some(call) = call.filter(|call| call.state == CallState::Joined) else {
        return seat;
    };
    if me {
        seat.muted = call.muted || call.deafened;
        seat.deafened = call.deafened;
        seat.speaking = call.speaking;
    } else if call.participants.iter().any(|who| who.id == seat.user_id && who.speaking) {
        seat.speaking = true;
    }
    seat
}

/// The room with my seat added, when my call is up in it and the room does not list me yet (the
/// call joins before the server says who is seated). `None` when it lists me already.
pub(in crate::gui) fn with_me(room: &TinyRoom, me: &str) -> Option<TinyRoom> {
    if room.participants.iter().any(|seat| seat.user_id == me) {
        return None;
    }
    let mut room = room.clone();
    room.participants.push(TinySeat { user_id: me.to_owned(), ..TinySeat::default() });
    Some(room)
}

impl Stage {
    /// Builds a stage from any live, ringing, connecting, or failed `Call`: everyone else first,
    /// then me last (matching `call_phone_tiles`). While ringing out with no joined peers, only
    /// my seat is present.
    pub(in crate::gui) fn of_call(
        call: &Call,
        subject: &Subject,
        me_id: &str,
        me_name: &str,
    ) -> Stage {
        let my_id = if me_id.is_empty() { "me" } else { me_id };
        let my_name = if me_name.is_empty() { "You" } else { me_name };
        let live = matches!(call.state, CallState::Joined | CallState::Reconnecting);
        let mut seats: Vec<Seat> = if live {
            call.participants
                .iter()
                .filter(|who| !who.ringing && who.id != my_id)
                .map(|who| Seat {
                    id: who.id.clone(),
                    name: who.name.clone(),
                    me: false,
                    speaking: who.speaking,
                    muted: who.muted,
                    deafened: who.deafened,
                    server_muted: who.server_muted,
                    server_deafened: who.server_deafened,
                    suppressed: who.suppressed,
                    priority: who.priority,
                    camera: who.camera,
                    sharing: who.sharing,
                    hand_raised: who.hand_raised,
                    reaction: who.reaction.clone(),
                })
                .collect()
        } else {
            Vec::new()
        };
        let my_camera = sending(call, Content::Camera);
        let my_sharing = sending(call, Content::Share);
        seats.push(Seat {
            id: my_id.to_owned(),
            name: my_name.to_owned(),
            me: true,
            speaking: call.state == CallState::Joined && call.speaking,
            muted: call.muted || call.deafened,
            deafened: call.deafened,
            server_muted: call.server_muted,
            server_deafened: call.server_deafened,
            suppressed: call.suppressed,
            priority: false,
            camera: my_camera,
            sharing: my_sharing,
            hand_raised: call.hand_raised,
            reaction: call.reaction.clone(),
        });
        let video = call.video_allowed
            && protocol::call_pictures(call.conversation.network);
        Stage {
            key: call.conversation.clone(),
            title: subject.title.clone(),
            kind: subject.kind,
            seats,
            video,
        }
    }

    /// Builds a stage from a Tiny room and its server `Page` (whether or not I am in that room).
    pub(in crate::gui) fn of_room(page: &Page, room: &TinyRoom, call: Option<&Call>) -> Stage {
        let seated_here = call.is_some_and(|call| {
            matches!(call.state, CallState::Joined | CallState::Reconnecting)
        });
        let padded = seated_here.then(|| with_me(room, page.me)).flatten();
        let shown = padded.as_ref().unwrap_or(room);
        let seats = shown
            .participants
            .iter()
            .map(|raw| {
                let me = raw.user_id == page.me;
                let heard = as_heard(raw, me, call);
                Seat {
                    id: heard.user_id.clone(),
                    name: model::name_by_id(page.community, &heard.user_id),
                    me,
                    speaking: heard.speaking,
                    muted: heard.muted,
                    deafened: heard.deafened,
                    server_muted: heard.server_muted,
                    server_deafened: heard.server_deafened,
                    suppressed: heard.suppressed,
                    priority: heard.priority,
                    camera: heard.camera,
                    sharing: heard.sharing,
                    hand_raised: false,
                    reaction: None,
                }
            })
            .collect();
        Stage {
            key: Key {
                network: Network::Tiny,
                account: page.account.account.clone(),
                id: room.id.clone(),
            },
            title: room.name.clone(),
            kind: ConversationKind::Voice,
            seats,
            video: room.video_enabled,
        }
    }

    /// The id of my seat in this stage, if I am seated in it.
    pub(in crate::gui) fn me_id(&self) -> Option<&str> {
        self.seats.iter().find(|seat| seat.me).map(|seat| seat.id.as_str())
    }
}

/// Resolves display metadata for participants on a stage, with or without a loaded Tiny `Page`.
pub(in crate::gui) trait People {
    fn name(&self, id: &str) -> String;
    fn key(&self, id: &str) -> Key;
    fn picture(&self, id: &str) -> Option<PathBuf>;
    fn color(&self, id: &str) -> Option<u32>;
}

impl People for Page<'_> {
    fn name(&self, id: &str) -> String {
        model::name_by_id(self.community, id)
    }

    fn key(&self, id: &str) -> Key {
        Page::key(self, id)
    }

    fn picture(&self, id: &str) -> Option<PathBuf> {
        Page::picture(self, id)
    }

    fn color(&self, id: &str) -> Option<u32> {
        self.community
            .member(id)
            .and_then(|member| model::color_of(self.community, member))
    }
}

/// Fallback `People` built from a `Call`'s participants and resolved avatar paths.
pub(in crate::gui) struct CallPeople {
    pub conversation: Key,
    pub names: HashMap<String, String>,
    pub pictures: HashMap<String, PathBuf>,
}

impl People for CallPeople {
    fn name(&self, id: &str) -> String {
        self.names.get(id).cloned().unwrap_or_else(|| id.to_owned())
    }

    fn key(&self, id: &str) -> Key {
        Key { id: id.to_owned(), ..self.conversation.clone() }
    }

    fn picture(&self, id: &str) -> Option<PathBuf> {
        self.pictures.get(id).cloned()
    }

    fn color(&self, _id: &str) -> Option<u32> {
        None
    }
}

/// What fills the stage instead of the grid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::gui) enum Focus {
    /// One person drawn large over a row of the rest.
    Person(String),
    /// One person's stream filling the stage.
    Stream(String),
}

/// Where the people go while a stream is focused.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::gui) struct Strip {
    /// The stream has the whole stage.
    pub hidden: bool,
    /// The strip is laid over the stream's foot instead of under it.
    pub over: bool,
}

/// One stream on the stage. A person has one: their screen when they share it, else their
/// camera, which is what watching them shows (until multi-stream adds separate camera/share slots).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::gui) struct Stream {
    pub user_id: String,
    pub share: bool,
    pub mine: bool,
    /// False for a peer's camera while a screen share is live in the room on a single-slot server.
    pub watchable: bool,
}

/// Why a watched stream tile has not shown a frame yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::gui) enum Trouble {
    /// The server's single video slot is carrying someone else's screen or camera.
    OtherSource { name: String, kind: PictureKind },
    /// The local user is deafened (the server plans an empty video slot).
    Deafened,
    /// Waiting more than 3 s with no picture yet.
    Slow,
    /// Waiting more than 10 s with no picture yet.
    Failed,
    /// Excluded by mediad's concurrent decode budget.
    BudgetFull,
}

/// "{n} watching: names" for our own share. Empty when Discord has named nobody.
pub(in crate::gui) fn viewer_line(viewers: &[Participant]) -> Option<String> {
    if viewers.is_empty() {
        return None;
    }
    let names = viewers.iter().map(|who| if who.name.is_empty() { who.id.as_str() } else { who.name.as_str() }).collect::<Vec<_>>().join(", ");
    Some(format!("{} watching: {names}", viewers.len()))
}

/// The words on a stream tile before its picture, or while Discord says it is paused.
pub(in crate::gui) fn watch_words(asked: bool, paused: bool) -> &'static str {
    if paused {
        "Stream paused"
    } else if asked {
        "Connecting…"
    } else {
        "Watch stream"
    }
}

/// Diagnoses why watching `user_id` has not yielded a frame after `since`.
/// Returns `None` during the initial 3 s window when no immediate blocker is known.
pub(in crate::gui) fn watch_trouble(
    call: &Call,
    user_id: &str,
    owner_name: impl Fn(&str) -> String,
    since: Duration,
) -> Option<Trouble> {
    if call.deafened {
        return Some(Trouble::Deafened);
    }
    if call.watch_refused.iter().any(|(uid, _)| uid == user_id) {
        return Some(Trouble::BudgetFull);
    }
    if call.pictures.is_empty() {
        if let Some((other_id, kind)) = &call.picture_source {
            if other_id != user_id {
                return Some(Trouble::OtherSource { name: owner_name(other_id), kind: *kind });
            }
        }
    }
    if since >= Duration::from_secs(10) {
        return Some(Trouble::Failed);
    }
    if since >= Duration::from_secs(3) {
        return Some(Trouble::Slow);
    }
    None
}

/// What my own call here is sending, which it knows before the server's seat says so. `None`
/// when my call is not up here.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::gui) struct Sending {
    pub share: bool,
    pub camera: bool,
}

/// Every stream on the stage, in seat order. A stage with video turned off has none.
pub(in crate::gui) fn streams(stage: &Stage, me: &str, sending: Option<Sending>) -> Vec<Stream> {
    if !stage.video {
        return Vec::new();
    }
    let of = |seat: &Seat| {
        let mine = seat.me || (!me.is_empty() && seat.id == me);
        let (share, camera) = match sending.filter(|_| mine) {
            Some(sending) => (sending.share || seat.sharing, sending.camera || seat.camera),
            None => (seat.sharing, seat.camera),
        };
        (share || camera).then(|| Stream {
            user_id: seat.id.clone(),
            share,
            mine,
            watchable: true,
        })
    };
    let mut found: Vec<Stream> = stage.seats.iter().filter_map(of).collect();
    // My share is drawn from my call even before my seat is in the stage's list.
    if let Some(sending) = sending.filter(|sending| {
        (sending.share || sending.camera)
            && !stage.seats.iter().any(|seat| seat.me || (!me.is_empty() && seat.id == me))
    }) {
        found.push(Stream {
            user_id: me.to_owned(),
            share: sending.share,
            mine: true,
            watchable: true,
        });
    }
    let any_share = found.iter().any(|stream| stream.share);
    if any_share {
        for stream in &mut found {
            if !stream.share && !stream.mine {
                stream.watchable = false;
            }
        }
    }
    found
}

/// Above this many participants, the stage grid keeps active speakers, streams, and `me` up to
/// this cap so tiles never shrink without bound, and moves silent listeners into a scrollable
/// roster list underneath.
pub(in crate::gui) const STAGE_GRID_CAP: usize = 12;

/// Partition `seats` into `(grid, roster)` for a call of any size.
/// Small calls (`<= cap`) place everyone in `grid` with an empty `roster`.
/// Large calls place speaking seats, anyone with an active stream, and `me` in `grid` (filling
/// remaining slots up to `cap`), and place the remaining silent participants in `roster`.
pub(in crate::gui) fn partition_stage<'a>(
    seats: &'a [Seat],
    streams: &[Stream],
    me: &str,
    cap: usize,
) -> (Vec<&'a Seat>, Vec<&'a Seat>) {
    if seats.len() <= cap {
        return (seats.iter().collect(), Vec::new());
    }
    let has_stream = |uid: &str| streams.iter().any(|s| s.user_id == uid);
    let mut grid = Vec::new();
    let mut rest = Vec::new();
    for seat in seats {
        if seat.speaking
            || seat.sharing
            || seat.camera
            || seat.me
            || (!me.is_empty() && seat.id == me)
            || has_stream(&seat.id)
        {
            grid.push(seat);
        } else {
            rest.push(seat);
        }
    }
    if grid.len() < cap {
        let take = (cap - grid.len()).min(rest.len());
        grid.extend(rest.drain(..take));
    }
    (grid, rest)
}

/// A click on a tile: that tile focused, or the grid again when it already was.
pub(in crate::gui) fn toggle(focus: Option<&Focus>, clicked: Focus) -> Option<Focus> {
    (focus != Some(&clicked)).then_some(clicked)
}

/// The focus as it can still be drawn: a person who left, or a stream that ended, is the grid.
pub(in crate::gui) fn still<'a>(
    focus: Option<&'a Focus>,
    stage: &Stage,
    streams: &[Stream],
) -> Option<&'a Focus> {
    focus.filter(|focus| match focus {
        Focus::Person(user_id) => stage.seats.iter().any(|seat| seat.id == *user_id),
        Focus::Stream(user_id) => streams.iter().any(|stream| stream.user_id == *user_id),
    })
}

/// The three-step Escape walk for the call view: full screen -> stage focus -> leave view.
pub(in crate::gui) fn escape_step(
    full: &mut Option<bool>,
    focus: &mut Option<Focus>,
    view: &mut Option<Key>,
) -> bool {
    if full.take().is_some() {
        return true;
    }
    if focus.take().is_some() {
        return true;
    }
    view.take().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::tiny::model;
    use protocol::{AccountKey, Participant, TinyCommunity, TinyMember};

    fn stage_of(seats: Vec<Seat>) -> Stage {
        Stage {
            key: Key { network: Network::Tiny, account: "srv".into(), id: "lounge".into() },
            title: "Lounge".into(),
            kind: ConversationKind::Voice,
            seats,
            video: true,
        }
    }

    fn seat(id: &str, sharing: bool, camera: bool) -> Seat {
        Seat { id: id.into(), name: id.into(), sharing, camera, ..Seat::default() }
    }

    fn sample_call(state: CallState, participants: Vec<Participant>) -> Call {
        let mut call: Call = serde_json::from_value(serde_json::json!({
            "conversation": { "network": "tiny", "account": "srv", "id": "dm-1" },
            "state": "joined",
            "participants": [],
            "muted": false,
            "deafened": false,
            "joined": 1,
            "engine_linked": true,
            "video": []
        }))
        .unwrap();
        call.state = state;
        call.participants = participants;
        call
    }

    #[test]
    fn everyone_sharing_or_on_camera_has_a_stream_and_a_share_wins() {
        let stage = stage_of(vec![
            seat("ada", false, true),
            seat("bev", false, false),
            seat("cal", true, true),
        ]);
        let found = streams(&stage, "me", None);
        assert_eq!(
            found,
            vec![
                Stream { user_id: "ada".into(), share: false, mine: false, watchable: false },
                Stream { user_id: "cal".into(), share: true, mine: false, watchable: true },
            ]
        );
        let quiet = Stage { video: false, ..stage };
        assert!(streams(&quiet, "me", None).is_empty(), "a stage with video off has no streams");
    }

    #[test]
    fn my_stream_is_drawn_from_my_call_before_the_server_says_so() {
        let stage = stage_of(vec![
            Seat { me: true, ..seat("me", false, false) },
            seat("ada", false, false),
        ]);
        let sharing = Some(Sending { share: true, camera: false });
        assert_eq!(
            streams(&stage, "me", sharing),
            vec![Stream { user_id: "me".into(), share: true, mine: true, watchable: true }]
        );
        let seated = stage_of(vec![Seat { me: true, ..seat("me", true, false) }]);
        assert_eq!(
            streams(&seated, "me", Some(Sending::default())),
            vec![Stream { user_id: "me".into(), share: true, mine: true, watchable: true }]
        );
        assert_eq!(
            streams(&seated, "me", None),
            vec![Stream { user_id: "me".into(), share: true, mine: true, watchable: true }]
        );
        assert_eq!(streams(&stage_of(Vec::new()), "me", sharing).len(), 1);
    }

    #[test]
    fn watch_trouble_diagnoses_deafened_other_source_slow_and_failed() {
        let mut call = sample_call(CallState::Joined, Vec::new());
        let name = |id: &str| if id == "dev" { "Dev".into() } else { "Mara".into() };
        assert_eq!(watch_trouble(&call, "mara", name, Duration::from_secs(1)), None);
        assert_eq!(watch_trouble(&call, "mara", name, Duration::from_secs(3)), Some(Trouble::Slow));
        assert_eq!(watch_trouble(&call, "mara", name, Duration::from_secs(10)), Some(Trouble::Failed));

        call.picture_source = Some(("dev".into(), PictureKind::Share));
        assert_eq!(
            watch_trouble(&call, "mara", name, Duration::from_secs(1)),
            Some(Trouble::OtherSource { name: "Dev".into(), kind: PictureKind::Share })
        );

        call.deafened = true;
        assert_eq!(watch_trouble(&call, "mara", name, Duration::from_secs(0)), Some(Trouble::Deafened));
    }

    #[test]
    fn a_paused_stream_is_named_and_viewers_are_counted() {
        assert_eq!(watch_words(false, false), "Watch stream");
        assert_eq!(watch_words(true, false), "Connecting…");
        assert_eq!(watch_words(true, true), "Stream paused");
        assert_eq!(viewer_line(&[]), None);
        let dev = Participant { id: "2002".into(), name: "Dev Patel".into(), ..Participant::default() };
        let sam = Participant { id: "2006".into(), name: String::new(), ..Participant::default() };
        assert_eq!(viewer_line(&[dev, sam]).as_deref(), Some("2 watching: Dev Patel, 2006"));
    }

    #[test]
    fn a_click_focuses_and_a_second_click_is_the_grid_again() {
        let ada = Focus::Stream("ada".into());
        assert_eq!(toggle(None, ada.clone()), Some(ada.clone()));
        assert_eq!(toggle(Some(&ada), ada.clone()), None);
        let person = Focus::Person("ada".into());
        assert_eq!(
            toggle(Some(&ada), person.clone()),
            Some(person),
            "a person and their stream are two tiles"
        );
    }

    #[test]
    fn a_stream_that_ends_or_a_person_who_leaves_unfocuses() {
        let stage = stage_of(vec![seat("ada", true, false), seat("bev", false, false)]);
        let found = streams(&stage, "me", None);
        let ada = Focus::Stream("ada".into());
        assert_eq!(still(Some(&ada), &stage, &found), Some(&ada));
        assert_eq!(still(Some(&Focus::Stream("bev".into())), &stage, &found), None, "bev shares nothing");
        assert_eq!(still(Some(&Focus::Person("bev".into())), &stage, &found), Some(&Focus::Person("bev".into())));
        assert_eq!(still(Some(&Focus::Person("cal".into())), &stage, &found), None, "cal is not in the call");
    }

    #[test]
    fn large_call_partitions_speakers_and_streams_into_grid_and_rest_into_roster() {
        let small: Vec<Seat> = (0..10).map(|i| seat(&format!("u{i}"), false, false)).collect();
        let (grid, roster) = partition_stage(&small, &[], "u0", STAGE_GRID_CAP);
        assert_eq!(grid.len(), 10);
        assert!(roster.is_empty());

        let mut large: Vec<Seat> = (0..40).map(|i| seat(&format!("u{i}"), false, false)).collect();
        large[25].speaking = true;
        large[33].camera = true;
        large[39].me = true;
        let stage = stage_of(large.clone());
        let found = streams(&stage, "u39", None);
        let (grid, roster) = partition_stage(&large, &found, "u39", STAGE_GRID_CAP);
        assert_eq!(grid.len(), STAGE_GRID_CAP);
        assert_eq!(roster.len(), 40 - STAGE_GRID_CAP);
        assert!(grid.iter().any(|s| s.id == "u25"), "speaker stays in grid");
        assert!(grid.iter().any(|s| s.id == "u33"), "camera holder stays in grid");
        assert!(grid.iter().any(|s| s.id == "u39"), "me stays in grid");
    }

    #[test]
    fn stage_of_call_direct_has_two_seats_with_me_last_and_ringing_has_one() {
        let subject = Subject {
            title: "Mara Okafor".into(),
            kind: ConversationKind::Direct,
            avatar: None,
            space: None,
            occupied: 0,
        };
        let joined = sample_call(
            CallState::Joined,
            vec![Participant {
                id: "mara".into(),
                name: "Mara Okafor".into(),
                speaking: true,
                camera: true,
                ..Participant::default()
            }],
        );
        let stage = Stage::of_call(&joined, &subject, "robin", "Robin");
        assert_eq!(stage.seats.len(), 2);
        assert_eq!(stage.seats[0].id, "mara");
        assert!(!stage.seats[0].me);
        assert!(stage.seats[0].speaking && stage.seats[0].camera);
        assert_eq!(stage.seats[1].id, "robin");
        assert!(stage.seats[1].me);
        assert!(stage.video);

        let ringing = sample_call(
            CallState::RingingOut,
            vec![Participant {
                id: "mara".into(),
                name: "Mara Okafor".into(),
                ringing: true,
                ..Participant::default()
            }],
        );
        let ringing_stage = Stage::of_call(&ringing, &subject, "robin", "Robin");
        assert_eq!(ringing_stage.seats.len(), 1);
        assert!(ringing_stage.seats[0].me);
    }

    #[test]
    fn stage_of_room_draws_a_room_i_am_not_in() {
        let account = AccountKey { network: Network::Tiny, account: "srv".into() };
        // TinyCommunity has no Default: read an empty one the way the wire gives it.
        let mut community: TinyCommunity =
            serde_json::from_value(serde_json::json!({"role": "", "members": [], "invites": [], "audit": []})).unwrap();
        community.me = "me".into();
        community.members = vec![
            TinyMember { user_id: "ada".into(), display_name: "Ada".into(), ..TinyMember::default() },
            TinyMember { user_id: "bev".into(), display_name: "Bev".into(), ..TinyMember::default() },
        ];
        let mut room = TinyRoom::new("lounge", "Lounge");
        room.participants = vec![
            TinySeat { user_id: "ada".into(), speaking: true, ..TinySeat::default() },
            TinySeat { user_id: "bev".into(), muted: true, ..TinySeat::default() },
        ];
        let page = Page {
            community: &community,
            account: &account,
            may: model::may(&community),
            live: true,
            me: "me",
            mine: None,
            audio: true,
            name: "Night Shift".into(),
        };
        let stage = Stage::of_room(&page, &room, None);
        assert_eq!(stage.seats.len(), 2);
        assert_eq!(stage.seats[0].id, "ada");
        assert_eq!(stage.seats[0].name, "Ada");
        assert!(!stage.seats[0].me);
        assert!(stage.seats[0].speaking);
        assert_eq!(stage.seats[1].id, "bev");
        assert!(stage.seats[1].muted);
        assert!(stage.me_id().is_none(), "not in the room: my seat is not added");
    }

    #[test]
    fn escape_walks_fullscreen_then_focus_then_view() {
        let mut full = Some(true);
        let mut focus = Some(Focus::Stream("ada".into()));
        let mut view = Some(Key { network: Network::Tiny, account: "srv".into(), id: "dm".into() });

        assert!(escape_step(&mut full, &mut focus, &mut view));
        assert!(full.is_none() && focus.is_some() && view.is_some());

        assert!(escape_step(&mut full, &mut focus, &mut view));
        assert!(full.is_none() && focus.is_none() && view.is_some());

        assert!(escape_step(&mut full, &mut focus, &mut view));
        assert!(full.is_none() && focus.is_none() && view.is_none());

        assert!(!escape_step(&mut full, &mut focus, &mut view));
    }

    #[test]
    fn signal_group_call_stage_carries_video_raise_hand_and_reactions() {
        let mut group_call = sample_call(
            CallState::Joined,
            vec![
                Participant {
                    id: "grace".into(),
                    name: "Grace Ombra".into(),
                    hand_raised: true,
                    reaction: Some("👏".into()),
                    camera: true,
                    ..Participant::default()
                },
                Participant {
                    id: "alan".into(),
                    name: "Alan Ferro".into(),
                    sharing: true,
                    ..Participant::default()
                },
            ],
        );
        group_call.conversation = Key { network: Network::Signal, account: "me".into(), id: "g.club".into() };
        group_call.hand_raised = true;
        group_call.reaction = Some("👍".into());
        let subject = Subject {
            title: "Friday club".into(),
            kind: ConversationKind::Group,
            avatar: None,
            space: None,
            occupied: 3,
        };
        let stage = Stage::of_call(&group_call, &subject, "me", "Ada");
        assert!(stage.video, "Signal group calls allow camera and screen share");
        assert_eq!(stage.seats.len(), 3);
        let grace = stage.seats.iter().find(|s| s.id == "grace").unwrap();
        assert!(grace.hand_raised && grace.reaction.as_deref() == Some("👏") && grace.camera);
        assert!(grace.states().contains(&"Hand raised"));
        assert!(grace.glyphs().iter().any(|(icon, _, _)| *icon == "icons/hand.svg"));
        let alan = stage.seats.iter().find(|s| s.id == "alan").unwrap();
        assert!(alan.sharing);
        let me = stage.seats.iter().find(|s| s.me).unwrap();
        assert!(me.hand_raised && me.reaction.as_deref() == Some("👍"));
    }
}
