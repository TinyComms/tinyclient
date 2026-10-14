//! Who is in the call: the avatar stack on the dock, the list that opens from it, and the rows
//! under a voice channel in the tree. A speaking ring is a border that is on or off; nothing
//! here animates.

use super::{in_call, Subject};
use crate::gui::components::{glyph, Avatar};
use crate::gui::theme;
use gpui::{div, prelude::*, px, AnyElement, Div, FontWeight, Hsla};
use protocol::{Call, Key, Participant};

/// Rows drawn under a voice channel before "+ N more".
const OCCUPANTS: usize = 12;
/// Rows of the participant list on screen before it scrolls.
const LISTED: usize = 10;

/// An avatar in its speaking-ring slot: a 2 px gap and a 2 px border around it, always laid out
/// and transparent when off, so a ring switching on moves nothing.
pub(super) fn ringed(avatar: Div, size: f32, speaking: bool) -> Div {
    let ring: Hsla = if speaking { theme::positive().into() } else { gpui::transparent_black() };
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

fn person(call: &Call, who: &Participant, size: f32) -> Div {
    let key = Key {
        network: call.conversation.network,
        account: call.conversation.account.clone(),
        id: who.id.clone(),
    };
    Avatar::new(&key, &who.name, size).build()
}

/// Up to three 20 px avatars overlapping 6, then "+N". A speaker's edge turns `positive`.
/// The box is sized outright and each avatar placed in it, so the title row measures it right.
pub(super) fn stack(call: &Call) -> Div {
    let shown = call.participants.len().min(3);
    let more = call.participants.len() - shown;
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
            call.participants
                .iter()
                .take(3)
                .enumerate()
                .map(|(ix, who)| slot(ix, who.speaking, 24.).child(person(call, who, 20.))),
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

/// The 13 px glyphs at the right of a person's row.
fn status(muted: bool, deafened: bool) -> Div {
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(5.))
        .when(muted, |status| status.child(glyph("icons/mic-off.svg", 13., theme::text_faint())))
        .when(deafened, |status| {
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
const IN_LIST: Sizes = Sizes { height: 32., indent: 12., right: 10., avatar: 22., text: 13. };

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
    let heard = call.speaking && !call.muted && !call.deafened;
    let first = row(sizes, own, mine, true, heard, status(call.muted || call.deafened, call.deafened));
    let others = roster(call).into_iter().map(|who| {
        let avatar = person(call, who, sizes.avatar);
        row(sizes, avatar, &who.name, false, who.speaking, status(who.muted, who.deafened))
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

/// What the participant list holds: "LOUNGE · 21 PEOPLE", then a row each. Ten rows show and
/// the rest scroll; it is a plain column, not a virtual list.
pub(super) fn list(call: &Call, subject: &Subject) -> Div {
    let all = rows(call, None, &IN_LIST);
    let count = all.len();
    let noun = if count == 1 { "PERSON" } else { "PEOPLE" };
    div()
        .flex()
        .flex_col()
        .child(
            div()
                .h(px(26.))
                .flex_none()
                .px(px(12.))
                .flex()
                .items_center()
                .text_size(px(11.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::text_faint())
                .child(format!("{} · {count} {noun}", subject.title.to_uppercase())),
        )
        .child(
            div()
                .id("call-people-rows")
                .max_h(px(IN_LIST.height * LISTED as f32))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .children(all),
        )
}
