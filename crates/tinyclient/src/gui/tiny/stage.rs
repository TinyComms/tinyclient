//! What the call stage holds, as Discord lays it out: a grid of everyone in the call and of every
//! stream (someone's screen, else their camera), each stream a tile of its own beside its owner's.
//! Clicking a stream watches it and fills the stage with it, the people in a strip under it, over
//! it, or hidden; clicking it again, or Escape, is the grid again. Clicking a person draws them
//! large over a row of the rest. Pure, so the choices are tested without a window.

use protocol::{Call, PictureKind, TinyRoom, TinySeat};
use std::time::Duration;

/// What fills the stage instead of the grid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Focus {
    /// One person drawn large over a row of the rest.
    Person(String),
    /// One person's stream filling the stage.
    Stream(String),
}

/// Where the people go while a stream is focused.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Strip {
    /// The stream has the whole stage.
    pub hidden: bool,
    /// The strip is laid over the stream's foot instead of under it.
    pub over: bool,
}

/// One stream on the stage. A person has one: their screen when they share it, else their
/// camera, which is what watching them shows (the server's one video slot carries one picture).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Stream {
    pub user_id: String,
    pub share: bool,
    pub mine: bool,
    /// False for a peer's camera while a screen share is live in the room (the single video slot
    /// carries the share, so Watch on a camera tile would stay Connecting forever).
    pub watchable: bool,
}

/// Why a watched stream tile has not shown a frame yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Trouble {
    /// The server's single video slot is carrying someone else's screen or camera.
    OtherSource { name: String, kind: PictureKind },
    /// The local user is deafened (the server plans an empty video slot).
    Deafened,
    /// Waiting more than 3 s with no picture yet.
    Slow,
    /// Waiting more than 10 s with no picture yet.
    Failed,
}

/// Diagnoses why watching `user_id` has not yielded a frame after `since`.
/// Returns `None` during the initial 3 s window when no immediate blocker is known.
pub(super) fn watch_trouble(
    call: &Call,
    user_id: &str,
    owner_name: impl Fn(&str) -> String,
    since: Duration,
) -> Option<Trouble> {
    if call.deafened {
        return Some(Trouble::Deafened);
    }
    if let Some((other_id, kind)) = &call.picture_source {
        if other_id != user_id {
            return Some(Trouble::OtherSource { name: owner_name(other_id), kind: *kind });
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
pub(super) struct Sending {
    pub share: bool,
    pub camera: bool,
}

/// Every stream in the room, in seat order. A room with video turned off has none.
pub(super) fn streams(room: &TinyRoom, me: &str, sending: Option<Sending>) -> Vec<Stream> {
    if !room.video_enabled {
        return Vec::new();
    }
    let of = |seat: &TinySeat| {
        let mine = seat.user_id == me;
        // Mine is either: my call knows first, and the seat holds when my call's own list lags.
        let (share, camera) = match sending.filter(|_| mine) {
            Some(sending) => (sending.share || seat.sharing, sending.camera || seat.camera),
            None => (seat.sharing, seat.camera),
        };
        (share || camera).then(|| Stream { user_id: seat.user_id.clone(), share, mine, watchable: true })
    };
    let mut found: Vec<Stream> = room.participants.iter().filter_map(of).collect();
    // My share is drawn from my call even before my seat is in the room's list.
    if let Some(sending) = sending.filter(|sending| (sending.share || sending.camera) && !room.participants.iter().any(|seat| seat.user_id == me)) {
        found.push(Stream { user_id: me.to_owned(), share: sending.share, mine: true, watchable: true });
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

/// A click on a tile: that tile focused, or the grid again when it already was.
pub(super) fn toggle(focus: Option<&Focus>, clicked: Focus) -> Option<Focus> {
    (focus != Some(&clicked)).then_some(clicked)
}

/// The focus as it can still be drawn: a person who left, or a stream that ended, is the grid.
pub(super) fn still<'a>(focus: Option<&'a Focus>, room: &TinyRoom, streams: &[Stream]) -> Option<&'a Focus> {
    focus.filter(|focus| match focus {
        Focus::Person(user_id) => room.participants.iter().any(|seat| seat.user_id == *user_id),
        Focus::Stream(user_id) => streams.iter().any(|stream| stream.user_id == *user_id),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room(seats: Vec<TinySeat>) -> TinyRoom {
        TinyRoom { participants: seats, ..TinyRoom::new("lounge", "Lounge") }
    }

    fn seat(user_id: &str, sharing: bool, camera: bool) -> TinySeat {
        TinySeat { user_id: user_id.into(), sharing, camera, ..TinySeat::default() }
    }

    #[test]
    fn everyone_sharing_or_on_camera_has_a_stream_and_a_share_wins() {
        let room = room(vec![seat("ada", false, true), seat("bev", false, false), seat("cal", true, true)]);
        let found = streams(&room, "me", None);
        assert_eq!(found, vec![
            Stream { user_id: "ada".into(), share: false, mine: false, watchable: false },
            Stream { user_id: "cal".into(), share: true, mine: false, watchable: true },
        ]);
        let quiet = TinyRoom { video_enabled: false, ..room };
        assert!(streams(&quiet, "me", None).is_empty(), "a room with video off has no streams");
    }

    #[test]
    fn my_stream_is_drawn_from_my_call_before_the_server_says_so() {
        // The server has not echoed my share yet: my call already sends it.
        let room = room(vec![seat("me", false, false), seat("ada", false, false)]);
        let sharing = Some(Sending { share: true, camera: false });
        assert_eq!(streams(&room, "me", sharing), vec![Stream { user_id: "me".into(), share: true, mine: true, watchable: true }]);
        // My seat says I share while my call's list has not caught up: still drawn.
        let seated = room_with_my_share();
        assert_eq!(streams(&seated, "me", Some(Sending::default())), vec![Stream { user_id: "me".into(), share: true, mine: true, watchable: true }]);
        // No call of mine here: the seat is all there is to go on.
        assert_eq!(streams(&seated, "me", None), vec![Stream { user_id: "me".into(), share: true, mine: true, watchable: true }]);
        // My seat not in the list yet.
        assert_eq!(streams(&room_empty(), "me", sharing).len(), 1);
    }

    #[test]
    fn watch_trouble_diagnoses_deafened_other_source_slow_and_failed() {
        let mut call: Call = serde_json::from_value(serde_json::json!({
            "conversation": { "network": "tiny", "account": "srv", "id": "lounge" },
            "state": "joined", "participants": [], "muted": false, "deafened": false,
            "joined": 1, "engine_linked": true, "video": []
        })).unwrap();
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

    fn room_with_my_share() -> TinyRoom {
        room(vec![seat("me", true, false)])
    }

    fn room_empty() -> TinyRoom {
        room(Vec::new())
    }

    #[test]
    fn a_click_focuses_and_a_second_click_is_the_grid_again() {
        let ada = Focus::Stream("ada".into());
        assert_eq!(toggle(None, ada.clone()), Some(ada.clone()));
        assert_eq!(toggle(Some(&ada), ada.clone()), None);
        let person = Focus::Person("ada".into());
        assert_eq!(toggle(Some(&ada), person.clone()), Some(person), "a person and their stream are two tiles");
    }

    #[test]
    fn a_stream_that_ends_or_a_person_who_leaves_unfocuses() {
        let room = room(vec![seat("ada", true, false), seat("bev", false, false)]);
        let found = streams(&room, "me", None);
        let ada = Focus::Stream("ada".into());
        assert_eq!(still(Some(&ada), &room, &found), Some(&ada));
        assert_eq!(still(Some(&Focus::Stream("bev".into())), &room, &found), None, "bev shares nothing");
        assert_eq!(still(Some(&Focus::Person("bev".into())), &room, &found), Some(&Focus::Person("bev".into())));
        assert_eq!(still(Some(&Focus::Person("cal".into())), &room, &found), None, "cal is not in the call");
    }
}
