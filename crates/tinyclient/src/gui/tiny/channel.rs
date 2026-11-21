//! One voice channel in the pane: its topic, how full it is, push-to-talk and bitrate in the top
//! bar, then the shared call stage (`gui/call/view.rs`), wrapped with room-only states (empty room
//! with Join, full room, join-blocked, preset mute/deafen, server-held mute).

use super::{join_blocked, model, Page};
use crate::gui::call::stage::{Stage, self};
use crate::gui::call::view::ViewBar;
use crate::gui::components::{button, button_disabled, button_with_icon, glyph, ButtonKind};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use gpui::{div, prelude::*, px, AnyElement, Context, Div};
use protocol::{Call, CallState, TinyRoom};

pub(in crate::gui) use crate::gui::call::stage::{as_heard, with_me};
pub(in crate::gui) use crate::gui::call::view::chip;

impl Shell {
    /// What the top bar says of a channel after its name.
    pub(super) fn tiny_channel_facts(&self, page: &Page, room: &TinyRoom) -> Div {
        let seated_here = self
            .tiny_call_here(room)
            .is_some_and(|call| matches!(call.state, CallState::Joined | CallState::Reconnecting));
        let seated = seated_here.then(|| with_me(room, page.me)).flatten();
        let shown = seated.as_ref().unwrap_or(room);
        let mut facts = div().flex().items_center().gap(px(6.)).min_w_0();
        if !shown.topic.trim().is_empty() {
            facts = facts
                .child(div().flex_none().w(px(1.)).h(px(16.)).bg(theme::hairline()))
                .child(div().min_w_0().truncate().type_ui().text_color(theme::text_soft()).child(shown.topic.clone()));
        }
        facts = facts.child(chip(
            model::channel_occupancy(shown),
            if model::full(shown) { theme::warning() } else { theme::text_soft() },
            if model::full(shown) { theme::warning_soft() } else { theme::hover() },
        ));
        if shown.ptt_only {
            facts = facts.child(chip("Push to talk", theme::accent_text(), theme::accent_soft()));
        }
        if shown.bitrate_kbps > 0 {
            facts = facts.child(chip(format!("{} kb/s", shown.bitrate()), theme::text_soft(), theme::hover()));
        }
        facts
    }

    /// My call, when it is in this room.
    fn tiny_call_here(&self, room: &TinyRoom) -> Option<Call> {
        self.tiny_call().filter(|call| call.conversation.id == room.id)
    }

    /// The call header's security slot, in a channel's top bar while I am in its call.
    pub(super) fn tiny_channel_security(&self, room: &TinyRoom, cx: &mut Context<Self>) -> Option<AnyElement> {
        let call = self.tiny_call_here(room).filter(|call| call.state == CallState::Joined)?;
        self.call_security(&call, crate::gui::call::security::Place::TopBar, cx)
    }

    pub(super) fn tiny_channel(&self, page: &Page, room: &TinyRoom, cx: &mut Context<Self>) -> AnyElement {
        let mine = page.mine == Some(room.id.as_str());
        let call = self.tiny_call_here(room);
        let banner = call.as_ref().and_then(|call| self.call_view_banner(&room.name, call, cx));
        let up = call.as_ref().is_some_and(|call| !matches!(call.state, CallState::Failed { .. }));
        let seated_here = call.as_ref().is_some_and(|call| matches!(call.state, CallState::Joined | CallState::Reconnecting));
        let stage: AnyElement = if room.participants.is_empty() && !seated_here {
            self.tiny_empty_stage(page, room, cx)
        } else {
            let model = Stage::of_room(page, room, call.as_ref());
            self.call_stage(&model, page, call.as_ref(), true, cx)
        };
        let bar = if room.participants.is_empty() && !mine && !up {
            None
        } else {
            Some(self.tiny_call_bar(page, room, mine || up, call.as_ref(), cx))
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .children(banner)
            .child(stage)
            .children(bar)
            .into_any_element()
    }

    fn tiny_empty_stage(&self, page: &Page, room: &TinyRoom, cx: &mut Context<Self>) -> AnyElement {
        let blocked = join_blocked(page.live, &page.community.media);
        let join: AnyElement = match blocked {
            _ if !page.community.allowed("community.calls.join") => div().into_any_element(),
            Some(_) => button_disabled("Join channel").h(px(32.)).into_any_element(),
            None if self.tiny_state.call_busy => button("tiny-cancel-empty", ButtonKind::Danger, "Cancel")
                .h(px(32.))
                .on_click(cx.listener(|this, _, _, cx| this.tiny_leave(cx)))
                .into_any_element(),
            None => {
                let room_id = room.id.clone();
                button_with_icon("tiny-join-empty", ButtonKind::Primary, "icons/volume-2.svg", "Join channel")
                    .h(px(32.))
                    .on_click(cx.listener(move |this, _, _, cx| this.tiny_join(room_id.clone(), cx)))
                    .into_any_element()
            }
        };
        let devices = self.tiny_device_hint(page);
        div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(12.))
            .px(px(24.))
            .child(
                div()
                    .size(px(56.))
                    .rounded_full()
                    .bg(theme::raised())
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(glyph("icons/volume-2.svg", 24., theme::text_faint())),
            )
            .child(div().type_title().text_color(theme::text()).child(format!("Nobody is in {} yet", room.name)))
            .child(
                div()
                    .max_w(px(380.))
                    .text_center()
                    .whitespace_normal()
                    .type_ui()
                    .text_color(theme::text_soft())
                    .child(blocked.map(str::to_owned).unwrap_or(devices)),
            )
            .child(join)
            .children(
                self.tiny_state
                    .call_error
                    .clone()
                    .map(|detail| div().max_w(px(380.)).text_center().whitespace_normal().type_small().text_color(theme::danger()).child(detail)),
            )
            .into_any_element()
    }

    /// Which microphone and speaker a join will use, as far as is known.
    fn tiny_device_hint(&self, page: &Page) -> String {
        if !page.audio && !page.community.media.is_empty() {
            return "This server has no audio turned on, so joining only takes a seat.".to_owned();
        }
        match self.snapshot.as_ref().and_then(|snapshot| snapshot.call.as_ref()).filter(|call| !call.input.is_empty()) {
            Some(call) => format!(
                "You'll join with {} and hear through {}.",
                call.input,
                if call.output.is_empty() { "your speaker" } else { &call.output }
            ),
            None => "You'll join with your default microphone and speaker. Change them in Settings › Audio & video.".to_owned(),
        }
    }

    /// Under the stage: Join for a channel I am not in, else the shared call control bar with
    /// room moderation states and presets.
    fn tiny_call_bar(&self, page: &Page, room: &TinyRoom, mine: bool, call: Option<&Call>, cx: &mut Context<Self>) -> Div {
        if !mine {
            let bar = div().w_full().flex_none().pb(px(20.)).pt(px(8.)).flex().flex_col().items_center().gap(px(8.));
            let blocked = join_blocked(page.live, &page.community.media);
            let join: AnyElement = if !page.community.allowed("community.calls.join") {
                div().type_ui().text_color(theme::text_soft()).child("You can't join calls on this server.").into_any_element()
            } else if model::full(room) {
                button_disabled(format!("{} is full · {} of {}", room.name, room.participants.len(), room.user_limit))
                    .h(px(36.))
                    .into_any_element()
            } else if self.tiny_state.call_busy {
                button("tiny-cancel", ButtonKind::Danger, "Cancel")
                    .h(px(36.))
                    .px(px(18.))
                    .on_click(cx.listener(|this, _, _, cx| this.tiny_leave(cx)))
                    .into_any_element()
            } else if blocked.is_some() {
                button_disabled("Join channel").h(px(36.)).into_any_element()
            } else {
                let room_id = room.id.clone();
                button_with_icon("tiny-join", ButtonKind::Primary, "icons/volume-2.svg", "Join channel")
                    .h(px(36.))
                    .px(px(18.))
                    .on_click(cx.listener(move |this, _, _, cx| this.tiny_join(room_id.clone(), cx)))
                    .into_any_element()
            };
            let under = if model::full(room) {
                "Someone has to leave before you can join.".to_owned()
            } else {
                blocked.map(str::to_owned).unwrap_or_else(|| self.tiny_device_hint(page))
            };
            return bar
                .child(join)
                .child(div().type_small().text_color(theme::text_faint()).child(under))
                .children(self.tiny_state.call_error.clone().map(|detail| div().type_small().text_color(theme::danger()).child(detail)));
        }
        let my_seat = room.participants.iter().find(|seat| seat.user_id == page.me);
        let held = my_seat.and_then(|seat| {
            if seat.server_muted {
                Some("A moderator muted you here")
            } else if seat.suppressed {
                Some("You can't speak in this channel")
            } else {
                None
            }
        });
        let held_ears = my_seat.is_some_and(|seat| seat.server_deafened);
        let no_sound = !page.audio && !page.community.media.is_empty();
        self.call_view_bar(
            call,
            &ViewBar {
                held,
                held_ears,
                ptt_only: room.ptt_only,
                video_allowed: room.video_enabled,
                room_context: true,
                no_sound,
                audio_up: page.audio,
            },
            cx,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::call::view::{talk, Talk};
    use crate::gui::call::watching;
    use protocol::{Content, Direction, Key, Network, Participant, TinySeat};

    fn joined(muted: bool, deafened: bool, speaking: bool) -> Call {
        Call {
            conversation: Key { network: Network::Tiny, account: "night".into(), id: "lounge".into() },
            state: CallState::Joined,
            participants: vec![Participant { id: "mara".into(), name: "Mara".into(), speaking: true, ..Participant::default() }],
            muted,
            deafened,
            server_muted: false,
            server_deafened: false,
            suppressed: false,
            joined: 1,
            input: String::new(),
            output: String::new(),
            engine_linked: true,
            video: Vec::new(),
            speaking,
            level: None,
            inputs: Vec::new(),
            outputs: Vec::new(),
            output_level: None,
            input_id: String::new(),
            output_id: String::new(),
            device_notice: None,
            device_stuck: false,
            security: None,
            picture: None,
            self_view: None,
            share_view: None,
            picture_source: None,
            video_allowed: true,
        }
    }

    fn seat(user_id: &str) -> TinySeat {
        TinySeat { user_id: user_id.into(), ..TinySeat::default() }
    }

    #[test]
    fn a_call_up_in_a_room_that_does_not_list_me_yet_still_has_my_tile() {
        let mut room = TinyRoom::new("general", "General");
        let seated = with_me(&room, "me").expect("my seat is added");
        assert_eq!(seated.participants, [seat("me")], "the stage has a tile to draw");
        room.participants = vec![seat("bea"), seat("me")];
        assert!(with_me(&room, "me").is_none(), "the server's own word of my seat is the one drawn");
    }

    #[test]
    fn my_camera_and_my_share_are_two_feeds_that_do_not_clobber_each_other() {
        use super::super::feed::{named, Which};
        let picture = |path: &str, generation, kind| protocol::CallPicture { path: path.into(), width: 640, height: 360, generation, user_id: String::new(), kind };
        let mut call = joined(false, false, false);
        let of = |call: &Call, which: fn(&Which) -> bool| named(Some(call)).into_iter().find(|(feed, _)| which(feed)).and_then(|(_, frame)| frame);
        let (mine, screen): (fn(&Which) -> bool, fn(&Which) -> bool) = (|which| matches!(which, Which::Mine), |which| matches!(which, Which::Screen));
        assert_eq!((of(&call, mine), of(&call, screen)), (None, None));
        call.share_view = Some(picture("share.jpg", 7, protocol::PictureKind::Share));
        assert_eq!((of(&call, mine), of(&call, screen)), (None, Some((7, "share.jpg".to_owned()))), "a share alone leaves the camera's feed empty");
        call.self_view = Some(picture("camera.jpg", 2, protocol::PictureKind::Camera));
        assert_eq!((of(&call, mine), of(&call, screen)), (Some((2, "camera.jpg".to_owned())), Some((7, "share.jpg".to_owned()))));
        call.share_view = None;
        assert_eq!((of(&call, mine), of(&call, screen)), (Some((2, "camera.jpg".to_owned())), None), "the share stopping lets go of its feed only");
    }

    #[test]
    fn my_tile_shows_my_call_at_once_and_others_light_on_the_quicker_edge() {
        let mine = as_heard(&seat("robin"), true, Some(&joined(true, false, false)));
        assert!(mine.muted && !mine.speaking, "muted here before the server says so");
        let deaf = as_heard(&seat("robin"), true, Some(&joined(false, true, false)));
        assert!(deaf.muted && deaf.deafened, "deafened is muted too");
        assert!(as_heard(&seat("robin"), true, Some(&joined(false, false, true))).speaking);
        assert!(as_heard(&seat("mara"), false, Some(&joined(false, false, false))).speaking, "mediad heard Mara first");
        assert!(!as_heard(&seat("dev"), false, Some(&joined(false, false, false))).speaking);
        let connecting = Call { state: CallState::Connecting, ..joined(true, false, true) };
        assert_eq!(as_heard(&seat("robin"), true, Some(&connecting)), seat("robin"), "only a call that is up speaks for me");
    }

    #[test]
    fn sending_is_our_encode_and_watching_is_that_person() {
        let mut call = joined(false, false, false);
        assert!(!stage::sending(&call, Content::Share));
        call.video.push(protocol::VideoStream { direction: Direction::Encode, content: Content::Camera, source: String::new() });
        assert!(stage::sending(&call, Content::Camera));
        assert!(!stage::sending(&call, Content::Share));
        assert!(!watching(&call, "ada"));
        call.video.push(protocol::VideoStream { direction: Direction::Decode, content: Content::Share, source: "ada".into() });
        assert!(watching(&call, "ada"));
        assert!(!watching(&call, "bev"));
    }

    #[test]
    fn the_push_to_talk_pill_names_the_key_and_lights_while_it_is_held() {
        assert_eq!(talk(false, None, Some("F".into())), Talk::Open);
        assert_eq!(talk(true, None, Some("F".into())), Talk::Waiting(Some("F".into())));
        assert_eq!(talk(true, None, None), Talk::Waiting(None));
        assert_eq!(talk(true, Some(false), None), Talk::Talking);
        assert_eq!(talk(false, Some(true), None), Talk::Priority, "the priority key talks in either mode");
    }
}
