//! The sign-in sheet on a phone: the same steps, drawn as a page of their own instead of a
//! floating panel (`docs/design/08-first-run-settings.md` §2, adapted to one column). A top bar
//! with the way back and Cancel under the status bar; the network's icon and the step's title
//! large; fields 52 high; the main action full width under what it acts on; the quiet links
//! under it, each a 44 pt target. The page keeps clear of the keyboard and the home indicator.
//!
//! Discord, Slack and Teams sign in on the network's own page. On a phone there is no helper
//! window beside this one to come back from, so the sheet first says what happens, then hands
//! over to the shell's sign-in and waits with it (`Face::Web`).

use super::{error_line, footer, Face, Sheet};
use crate::gui::components::{button, button_disabled, glyph, network_icon, ButtonKind};
use crate::gui::phone::tokens::{self, PhoneType};
use crate::gui::theme;
use gpui::{div, prelude::*, px, AnyElement, Context, Div, SharedString, Stateful};
use protocol::{Network, SignInInput};

/// A field on a phone: tall enough for a thumb and 17 pt type.
pub(super) const FIELD: f32 = 52.;
/// The main action.
const BUTTON: f32 = 50.;
/// The page's side margin.
const SIDE: f32 = 20.;

/// A network by the name the person knows it by.
fn name(network: Network) -> &'static str {
    match network {
        Network::Teams => "Microsoft Teams",
        other => other.label(),
    }
}

/// A button as the page's main action: full width and tall on a phone; unchanged elsewhere.
pub(super) fn wide<E: Styled>(button: E) -> E {
    if !crate::gui::phone::active() {
        return button;
    }
    button.w_full().h(px(BUTTON)).rounded(px(12.)).text_size(px(tokens::scaled(16.))).font_weight(theme::SEMIBOLD)
}

/// A link on a phone: 15 pt, and a 44 pt row to land a thumb on.
pub(super) fn touch_link(link: Stateful<Div>) -> Stateful<Div> {
    link.min_h(px(tokens::TOUCH)).flex().items_center().justify_center().text_size(px(tokens::scaled(15.))).line_height(px(tokens::scaled(20.)))
}

/// A text field on a phone.
pub(super) fn touch_field(field: Div) -> Div {
    field.h(px(FIELD)).px(px(14.)).rounded(px(10.)).text_size(px(tokens::scaled(17.))).line_height(px(tokens::scaled(22.)))
}

/// The whole screen: under the status bar, over the home indicator and the keyboard. The steps
/// scroll when the keyboard leaves them too little room.
pub(super) fn page(body: Div, cx: &Context<Sheet>) -> Div {
    let _ = cx;
    let bottom = tokens::safe_bottom().max(tokens::keyboard()) + 24.;
    div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .bg(theme::canvas())
        .font_family(theme::FONT)
        .text_size(px(tokens::scaled(16.)))
        .line_height(px(tokens::scaled(22.)))
        .text_color(theme::text())
        .occlude()
        .flex()
        .flex_col()
        .pt(px(tokens::safe_top()))
        .child(
            div()
                .id("signin-page")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .child(div().px(px(SIDE)).pb(px(bottom)).flex().flex_col().child(body)),
        )
}

impl Sheet {
    /// The top of a step on a phone: the bar (back on the left, Cancel on the right), then the
    /// network's icon and the step's title, large and left-aligned.
    pub(super) fn phone_header(&self, title: SharedString, back: bool, cx: &Context<Sheet>) -> Div {
        let back = back.then(|| {
            div()
                .id("signin-back")
                .size(px(tokens::TOUCH))
                .ml(px(-12.))
                .rounded(px(22.))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .active(|style| style.bg(theme::pressed()))
                .child(glyph("icons/chevron-left.svg", 24., theme::text()))
                .on_click(cx.listener(|this, _, _, cx| this.answer(SignInInput::Back, cx)))
        });
        let cancel = div()
            .id("signin-close")
            .h(px(tokens::TOUCH))
            .px(px(12.))
            .mr(px(-12.))
            .rounded(px(22.))
            .flex()
            .items_center()
            .cursor_pointer()
            .active(|style| style.bg(theme::pressed()))
            .text_size(px(tokens::scaled(16.)))
            .text_color(theme::accent_text())
            .child("Cancel")
            .on_click(cx.listener(|this, _, _, cx| this.close(true, cx)));
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(tokens::NAV))
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(back.map_or_else(|| div().into_any_element(), IntoElement::into_any_element))
                    .child(cancel),
            )
            .child(div().mt(px(12.)).child(network_icon(self.network, 48.)))
            .child(
                div()
                    .mt(px(16.))
                    .text_size(px(tokens::scaled(26.)))
                    .line_height(px(tokens::scaled(32.)))
                    .font_weight(theme::SEMIBOLD)
                    .child(title),
            )
    }

    /// Telegram's first step on a phone: the number is how this phone signs in; the code is
    /// there for linking it from another device that already has Telegram.
    pub(super) fn phone_qr_step(&self, tile: Div, steps: [&'static str; 3], cx: &Context<Sheet>) -> Div {
        let use_phone = if self.busy {
            wide(button_disabled("Use your phone number")).into_any_element()
        } else {
            wide(button("signin-use-phone", ButtonKind::Primary, "Use your phone number"))
                .on_click(cx.listener(|this, _, _, cx| this.answer(SignInInput::UsePhone, cx)))
                .into_any_element()
        };
        let _ = steps;
        div()
            .flex()
            .flex_col()
            .child(self.header(self.title(), false, cx))
            .child(
                div()
                    .mt(px(8.))
                    .text_color(theme::text_soft())
                    .child("Telegram sends a code to your other devices, or by SMS."),
            )
            .child(div().mt(px(24.)).child(use_phone))
            .child(
                div()
                    .mt(px(32.))
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(div().flex_1().h(px(1.)).bg(theme::hairline()))
                    .child(div().phone_preview().text_color(theme::text_faint()).child("or link from another device"))
                    .child(div().flex_1().h(px(1.)).bg(theme::hairline())),
            )
            .child(div().mt(px(20.)).flex().justify_center().child(tile))
            .child(
                div()
                    .mt(px(16.))
                    .phone_preview()
                    .text_color(theme::text_soft())
                    .text_center()
                    .child("In Telegram on that device: Settings > Devices > Link Desktop Device, then scan this code."),
            )
    }

    /// Whether Discord, Slack or Teams is being signed in by the shell now, and what it said
    /// when it last failed.
    fn web_state(&self, cx: &gpui::App) -> (bool, Option<String>) {
        let Some(shell) = self.shell.as_ref().and_then(|shell| shell.upgrade()) else {
            return (false, None);
        };
        let shell = shell.read(cx);
        let failed = shell.sign_in_failed.as_ref().filter(|(network, _)| *network == self.network).map(|(_, words)| words.clone());
        (shell.signing_in == Some(self.network), failed)
    }

    /// The shell's web sign-in ended: closes the page after a success, back to its first face
    /// with the shell's words after a failure. `false` once it has closed.
    pub(super) fn web_moved(&mut self, cx: &mut Context<Sheet>) -> bool {
        if self.canned || !matches!(self.face, Some(Face::Web { started: true })) {
            return true;
        }
        let (signing, failed) = self.web_state(cx);
        if signing {
            return true;
        }
        if failed.is_some() {
            self.face = Some(Face::Web { started: false });
            return true;
        }
        self.close(false, cx);
        false
    }

    /// Hands a Discord, Slack or Teams sign-in to the shell, which runs it as it always has. The
    /// shell asks the sheet first, so the sheet lets that one through (`pass`).
    fn continue_on_web(&mut self, cx: &mut Context<Sheet>) {
        if self.canned {
            return;
        }
        let Some(shell) = self.shell.clone() else {
            return;
        };
        let network = self.network;
        self.pass = Some(network);
        self.face = Some(Face::Web { started: true });
        // After this update: the shell's sign-in asks this sheet again.
        cx.defer(move |cx| {
            let _ = shell.update(cx, |shell, cx| shell.sign_in(network, cx));
        });
        cx.notify();
    }

    /// Discord, Slack or Teams on a phone: what happens next, then the wait for it.
    pub(super) fn web_step(&self, started: bool, cx: &Context<Sheet>) -> Div {
        let (signing, failed) = self.web_state(cx);
        let waiting = started && (signing || self.canned);
        let network = self.network;
        let name = name(network);
        let owner = if network == Network::Teams { "Microsoft" } else { name };
        let (title, line): (SharedString, String) = if waiting {
            ("Finish signing in".into(), format!("Sign in to {name} on the page that opened. This moves on by itself when you're done."))
        } else {
            let line = match network {
                Network::Slack => "Slack's own sign-in page opens next. Every workspace on that sign-in comes in at once.",
                Network::Teams => "Microsoft's own sign-in page opens next. Use a work, school or personal account.",
                _ => "Discord's own sign-in page opens next. Your servers, direct messages and voice come in here.",
            };
            (format!("Sign in to {name}").into(), line.to_owned())
        };
        let action: AnyElement = if waiting {
            wide(button_disabled("Waiting…")).into_any_element()
        } else {
            let words = if failed.is_some() { "Try again".to_owned() } else { format!("Continue to {name}") };
            wide(button("signin-primary", ButtonKind::Primary, words)).on_click(cx.listener(|this, _, _, cx| this.continue_on_web(cx))).into_any_element()
        };
        let kept = div()
            .mt(px(20.))
            .p(px(14.))
            .rounded(px(12.))
            .bg(theme::raised())
            .flex()
            .gap(px(12.))
            .child(div().pt(px(1.)).child(glyph("icons/lock.svg", 18., theme::text_soft())))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .phone_preview()
                    .text_color(theme::text_soft())
                    .child(format!("You sign in on {owner}'s own page. tinyclient keeps only the session it gets back, on this phone.")),
            );
        div()
            .flex()
            .flex_col()
            .child(self.header(title, false, cx))
            .child(div().mt(px(8.)).text_color(theme::text_soft()).child(line))
            .children(failed.filter(|_| !waiting).map(|words| error_line(words).mt(px(12.))))
            .children((!waiting).then_some(kept))
            .child(footer(20.).child(div()).child(action))
    }
}
