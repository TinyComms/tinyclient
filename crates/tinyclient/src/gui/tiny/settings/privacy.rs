//! Privacy: what this server keeps about its members. Everyone on a server with first-run setup
//! may read it. An owner reads it in the server's own words and changes it with the form the
//! first-run screen uses (`setup`), saved from the unsaved-changes bar like the other pages. A
//! member who is not an owner cannot ask the server for those words, so they read a short fixed
//! description of the three values `GET /v1/server` gives everyone (`crate::tiny_setup::keeps`).

use super::kit::card;
use super::{page_head, section};
use crate::gui::components::{button, button_disabled, glyph, ButtonKind};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use crate::gui::tiny::Page as Facts;
use crate::tiny_setup;
use gpui::{div, prelude::*, px, AnyElement, Context, Div, SharedString, Window};
use protocol::TinyServerInfo;

/// One line of a card: what is kept, then what that means.
fn line(heading: &str, value: Option<String>, words: impl Into<SharedString>, last: bool) -> Div {
    div()
        .px(px(14.))
        .py(px(12.))
        .flex()
        .flex_col()
        .gap(px(3.))
        .when(!last, |row| row.border_b_1().border_color(theme::hairline()))
        .child(div().type_micro().text_color(theme::text_faint()).child(heading.to_uppercase()))
        .children(value.map(|value| div().type_ui().font_weight(theme::MEDIUM).text_color(theme::text()).child(value)))
        .child(div().whitespace_normal().type_small().text_color(theme::text_soft()).child(words.into()))
}

/// What a member who is not an owner reads: the three values, in fixed words, and whether the
/// owner has chosen them yet.
fn members_view(server: &TinyServerInfo) -> Div {
    let lines = tiny_setup::keeps(server);
    if lines.is_empty() {
        return card().p(px(14.)).child(div().type_ui().text_color(theme::text_soft()).child("This server doesn't say what it keeps."));
    }
    let mut kept = card();
    let count = lines.len();
    for (ix, keep) in lines.into_iter().enumerate() {
        kept = kept.child(line(keep.heading, None, keep.words, ix + 1 == count));
    }
    div()
        .flex()
        .flex_col()
        .gap(px(10.))
        .child(kept)
        .child(div().whitespace_normal().type_small().text_color(theme::text_faint()).child(tiny_setup::decided_words(server)))
}

pub(in crate::gui::tiny) fn privacy(shell: &Shell, facts: &Facts, window: &mut Window, cx: &mut Context<Shell>) -> AnyElement {
    let _ = window;
    let community = facts.community;
    let owner = tiny_setup::may_choose(community);
    let server = community.server.clone().unwrap_or_default();
    let about = if owner { "What this server keeps about its members. You can change it here." } else { "What this server keeps about its members. Only the owner can change it." };
    let mut page = div().flex().flex_col().gap(px(28.)).child(page_head("Privacy", about));
    if !owner {
        return page.child(section("What this server keeps").child(members_view(&server))).into_any_element();
    }
    let state = &shell.tiny_state.setup;
    let Some(setup) = state.data.as_ref() else {
        // Not read yet, or it could not be: say what is known, and offer another try.
        let mut waiting = section("What this server keeps").child(members_view(&server));
        waiting = match (&state.read_error, state.reading) {
            (Some(detail), _) => waiting.child(div().whitespace_normal().type_small().text_color(theme::danger()).child(detail.clone())).child(
                div().flex().child(button("tiny-privacy-retry", ButtonKind::Secondary, "Try again").on_click(cx.listener(|this, _, _, cx| this.tiny_privacy_reread(cx)))),
            ),
            (None, _) => waiting.child(div().type_small().text_color(theme::text_faint()).child("Reading the choices from the server…")),
        };
        return page.child(waiting).into_any_element();
    };

    // Setup was never answered: say so, with the answer that changes nothing.
    if setup.needed {
        let keep = if state.sending {
            button_disabled("Keep what I have").into_any_element()
        } else {
            button("tiny-privacy-keep", ButtonKind::Secondary, "Keep what I have").on_click(cx.listener(|this, _, _, cx| this.tiny_privacy_keep(cx))).into_any_element()
        };
        page = page.child(
            card()
                .p(px(14.))
                .flex_row()
                .items_center()
                .gap(px(16.))
                .child(glyph("icons/info.svg", 16., theme::text_faint()))
                .child(div().flex_1().min_w_0().whitespace_normal().type_ui().text_color(theme::text_soft()).child("You haven't chosen yet, so the server keeps what it always has. Keeping it as it is is an answer too."))
                .child(keep),
        );
    }
    let mut now = card();
    let kept = tiny_setup::right_now(setup);
    let count = kept.len();
    for (ix, (heading, option, effect)) in kept.into_iter().enumerate() {
        now = now.child(line(&heading, Some(option), effect, ix + 1 == count));
    }
    page = page.child(section("Right now").child(now));
    page = page.child(shell.tiny_setup_form("tiny-privacy", "Change it", setup, cx));
    page.into_any_element()
}
