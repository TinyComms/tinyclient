//! Who is in the call: the avatar stack on the dock, the list that opens from it, and the rows
//! under a voice channel in the tree. A speaking ring is a border that is on or off; nothing
//! here animates.

use super::in_call;
use crate::gui::components::{glyph, Avatar};
use crate::gui::theme;
use gpui::{div, prelude::*, px, AnyElement, Div, FontWeight, Hsla};
use palette::IntoColor;
use protocol::{Call, Content, Direction, Key, Participant};

/// Rows drawn under a voice channel before "+ N more".
const OCCUPANTS: usize = 12;

/// An avatar in its speaking-ring slot: a 2 px gap and a 2 px border around it, always laid out
/// and transparent when off, so a ring switching on moves nothing.
pub(in crate::gui) fn ringed(avatar: Div, size: f32, speaking: bool) -> Div {
    let ring: Hsla = if speaking { theme::positive().into_color() } else { gpui::transparent_black() };
    div().relative().size(px(size)).flex_none().child(avatar).child(
        div()
            .absolute()
            .top(px(-4.))
            .left(px(-4.))
            .size(px(size + 8.))
            .rounded(px(size / 2. + 4.))
            .border_2()
            .border_color(ring),
    )
}

fn person(call: &Call, who: &Participant, size: f32, picture: Option<std::path::PathBuf>) -> Div {
    let key = Key {
        network: call.conversation.network,
        account: call.conversation.account.clone(),
        id: who.id.clone(),
    };
    Avatar::new(&key, &who.name, size).image_at(picture).build()
}

/// Up to three 20 px avatars overlapping 6, then "+N". A speaker's edge turns `positive`.
/// The box is sized outright and each avatar placed in it, so the title row measures it right.
pub(super) fn stack(call: &Call, mut picture: impl FnMut(&Participant) -> Option<std::path::PathBuf>) -> Div {
    let active: Vec<&Participant> = call.participants.iter().filter(|p| !p.ringing).collect();
    let shown = active.len().min(3);
    let more = active.len() - shown;
    let chip = if more > 9 { 32. } else { 26. };
    let width = shown as f32 * 18. + if more > 0 { chip } else { 6. };
    let slot = |ix: usize, speaking: bool, width: f32| {
        div()
            .absolute()
            .top_0()
            .left(px(ix as f32 * 18.))
            .w(px(width))
            .h(px(24.))
            .rounded(px(12.))
            .border_2()
            .border_color(if speaking { theme::positive() } else { theme::raised() })
            .bg(theme::raised())
    };
    div()
        .relative()
        .flex_none()
        .w(px(width))
        .h(px(24.))
        .children(
            active
                .into_iter()
                .take(3)
                .enumerate()
                .map(|(ix, who)| slot(ix, who.speaking, 24.).child(person(call, who, 20., picture(who)))),
        )
        .when(more > 0, |stack| {
            stack.child(
                slot(shown, false, chip).child(
                    div()
                        .h(px(20.))
                        .w_full()
                        .rounded(px(10.))
                        .bg(theme::selected())
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(10.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::text_soft())
                        .child(format!("+{more}")),
                ),
            )
        })
}

/// The 13 px glyphs at the right of a person's row: server-imposed moderation in `danger`
/// (or `warning` for suppressed), distinct from self mute/deafen in `text_faint`.
#[allow(clippy::too_many_arguments)]
fn status(
    muted: bool,
    deafened: bool,
    server_muted: bool,
    server_deafened: bool,
    suppressed: bool,
    camera: bool,
    sharing: bool,
    hand_raised: bool,
    reaction: Option<&str>,
) -> Div {
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(5.))
        .children(reaction.filter(|r| !r.is_empty()).map(|emoji| div().text_size(px(13.)).child(emoji.to_owned())))
        .when(hand_raised, |status| status.child(glyph("icons/hand.svg", 13., theme::warning())))
        .when(sharing, |status| status.child(glyph("icons/monitor-up.svg", 13., theme::text_soft())))
        .when(camera, |status| status.child(glyph("icons/video.svg", 13., theme::text_soft())))
        .when(server_muted, |status| status.child(glyph("icons/mic-off.svg", 13., theme::danger())))
        .when(!server_muted && suppressed, |status| status.child(glyph("icons/mic-off.svg", 13., theme::warning())))
        .when(!server_muted && !suppressed && muted && !deafened && !server_deafened, |status| {
            status.child(glyph("icons/mic-off.svg", 13., theme::text_faint()))
        })
        .when(server_deafened, |status| {
            status.child(glyph("icons/headphone-off.svg", 13., theme::danger()))
        })
        .when(!server_deafened && deafened, |status| {
            status.child(glyph("icons/headphone-off.svg", 13., theme::text_faint()))
        })
}

struct Sizes {
    height: f32,
    indent: f32,
    right: f32,
    avatar: f32,
    text: f32,
}

const IN_TREE: Sizes = Sizes { height: 28., indent: 38., right: 16., avatar: 20., text: 12.5 };

/// Me first, then everyone by name. It never reorders on speech.
fn roster(call: &Call) -> Vec<&Participant> {
    let mut people: Vec<&Participant> = call.participants.iter().collect();
    people.sort_by_key(|who| who.name.to_lowercase());
    people
}

fn row(sizes: &Sizes, avatar: Div, name: &str, you: bool, speaking: bool, glyphs: Div) -> Div {
    div()
        .w_full()
        .h(px(sizes.height))
        .flex_none()
        .pl(px(sizes.indent))
        .pr(px(sizes.right))
        .flex()
        .items_center()
        .gap(px(10.))
        .text_size(px(sizes.text))
        .text_color(if speaking { theme::text() } else { theme::text_soft() })
        .child(ringed(avatar, sizes.avatar, speaking))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .gap(px(4.))
                .child(div().min_w_0().truncate().child(name.to_owned()))
                .when(you && name != "You", |name| {
                    name.child(div().flex_none().text_color(theme::text_faint()).child("(you)"))
                }),
        )
        .child(glyphs)
}

fn rows(call: &Call, me: Option<&str>, sizes: &Sizes) -> Vec<Div> {
    let mine = me.filter(|name| !name.is_empty()).unwrap_or("You");
    let own = Avatar::named(mine, sizes.avatar).build();
    let heard = call.speaking && !call.muted && !call.deafened && !call.server_muted && !call.suppressed;
    let my_camera = call.self_view.is_some()
        || call.video.iter().any(|v| v.direction == Direction::Encode && v.content == Content::Camera);
    let my_sharing = call.share_view.is_some()
        || call.video.iter().any(|v| v.direction == Direction::Encode && v.content == Content::Share);
    let first = row(
        sizes,
        own,
        mine,
        true,
        heard,
        status(
            call.muted || call.deafened,
            call.deafened,
            call.server_muted,
            call.server_deafened,
            call.suppressed,
            my_camera,
            my_sharing,
            call.hand_raised,
            call.reaction.as_deref(),
        ),
    );
    let others = roster(call).into_iter().map(|who| {
        let avatar = person(call, who, sizes.avatar, who.avatar.as_ref().and_then(|f| f.path.as_deref()).map(std::path::PathBuf::from));
        let speaking = who.speaking && !who.server_muted && !who.suppressed;
        row(
            sizes,
            avatar,
            &who.name,
            false,
            speaking,
            status(
                who.muted,
                who.deafened,
                who.server_muted,
                who.server_deafened,
                who.suppressed,
                who.camera,
                who.sharing,
                who.hand_raised,
                who.reaction.as_deref(),
            ),
        )
    });
    std::iter::once(first).chain(others).collect()
}

/// The people under a voice channel row in the tree: 28 px rows, me first, at most twelve and
/// then "+ N more". `me` is my own name there, when the caller knows it.
///
/// This is the roster of the call I am in, with its speaking rings: rows for `channel` when it
/// is the joined call's conversation and nothing for any other. Who is in the other voice
/// channels is on their rows (`Conversation::occupants`, `Change::Occupants`), which the tree
/// draws itself; Discord says who is speaking only on the connection I joined.
#[allow(dead_code)]
pub(in crate::gui) fn occupants(call: Option<&Call>, channel: &Key, me: Option<&str>) -> Vec<AnyElement> {
    let Some(call) = call.filter(|call| &call.conversation == channel && in_call(call)) else {
        return Vec::new();
    };
    let mut all = rows(call, me, &IN_TREE);
    let more = all.len().saturating_sub(OCCUPANTS);
    all.truncate(OCCUPANTS);
    if more > 0 {
        all.push(
            div()
                .w_full()
                .h(px(IN_TREE.height))
                .flex_none()
                .pl(px(IN_TREE.indent + IN_TREE.avatar + 10.))
                .flex()
                .items_center()
                .text_size(px(IN_TREE.text))
                .text_color(theme::text_faint())
                .child(format!("+ {more} more")),
        );
    }
    all.into_iter().map(IntoElement::into_any_element).collect()
}
