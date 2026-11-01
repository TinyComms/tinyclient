//! First run on a phone (`docs/design/08-first-run-settings.md` §1 in one column): the app's
//! mark, the welcome and its line, then one card with a row per network that signs in on a
//! phone. Each row is the network's Dashboard Icons logo, its name, one line that wraps under
//! the name, and a quiet "Sign in"; the whole row is the target. Signal and Google Messages link
//! from a computer (a helper there pairs them), so they are not offered here; the footnote says
//! where they are.

use crate::gui::components::{brand_mark, network_icon};
use crate::gui::layout::NETWORK_ORDER;
use crate::gui::phone::tokens::{self, PhoneType};
use crate::gui::state::Shell;
use crate::gui::theme;
use gpui::{div, prelude::*, px, Context, Div, SharedString};
use protocol::Network;

/// Whether a phone can sign in to `network`. Signal and Messages link this device from the phone
/// that holds the account, through a helper only the desktop app has.
pub fn on_phone(network: Network) -> bool {
    !matches!(network, Network::Signal | Network::Messages)
}

/// The networks a phone offers, in the desktop's fixed order.
pub fn phone_networks() -> impl Iterator<Item = Network> {
    NETWORK_ORDER.into_iter().filter(|network| on_phone(*network))
}

/// The line under a network's name on the phone: what signing in there gets you.
fn line(network: Network) -> &'static str {
    match network {
        Network::Discord => "Servers, DMs and voice",
        Network::Slack => "All your workspaces",
        Network::Telegram => "Your number and a code",
        Network::Teams => "Work, school or personal",
        Network::Tiny => "An invite, or your own",
        Network::Signal | Network::Messages => "Link from tinyclient on a computer",
    }
}

fn name(network: Network) -> &'static str {
    match network {
        Network::Teams => "Microsoft Teams",
        other => other.label(),
    }
}

/// The row's quiet button: a pill on `selected`, as the desktop's.
fn pill(words: &'static str) -> Div {
    div()
        .flex_none()
        .h(px(36.))
        .px(px(16.))
        .rounded(px(18.))
        .bg(theme::selected())
        .flex()
        .items_center()
        .text_size(px(tokens::scaled(15.)))
        .line_height(px(tokens::scaled(20.)))
        .font_weight(theme::MEDIUM)
        .text_color(theme::text())
        .child(words)
}

impl Shell {
    /// Nothing is signed in, on a phone.
    pub(in crate::gui) fn phone_first_run(&self, cx: &mut Context<Self>) -> Div {
        let mut card = div()
            .w_full()
            .rounded(px(14.))
            .bg(theme::raised())
            .border_1()
            .border_color(theme::hairline())
            .overflow_hidden()
            .flex()
            .flex_col();
        for (ix, network) in phone_networks().enumerate() {
            let waiting = self.signing_in == Some(network);
            let dimmed = self.signing_in.is_some() && !waiting;
            let failed = self.sign_in_failed.as_ref().filter(|(failed, _)| *failed == network).map(|(_, why)| why.clone());
            let words: SharedString = if waiting { format!("Finish signing in on {}'s page.", name(network)).into() } else { line(network).into() };
            let row = div()
                .id(("phone-network", ix))
                .min_h(px(76.))
                .px(px(16.))
                .py(px(14.))
                .flex()
                .items_center()
                .gap(px(14.))
                .when(ix > 0, |row| row.border_t_1().border_color(theme::hairline()))
                .when(dimmed, |row| row.opacity(0.4))
                .when(waiting, |row| row.bg(theme::hover()))
                .when(!dimmed && !waiting, |row| {
                    row.cursor_pointer()
                        .active(|style| style.bg(theme::pressed()))
                        .on_click(cx.listener(move |this, _, _, cx| this.sign_in(network, cx)))
                })
                .child(network_icon(network, 40.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(
                            div()
                                .text_size(px(tokens::scaled(16.)))
                                .line_height(px(tokens::scaled(21.)))
                                .font_weight(theme::SEMIBOLD)
                                .text_color(theme::text())
                                .child(name(network)),
                        )
                        // Wraps under the name, left-aligned, inside the row's own height.
                        .child(div().w_full().text_align(gpui::TextAlign::Left).phone_preview().text_color(theme::text_soft()).child(words))
                        .children(failed.map(|why| div().w_full().phone_preview().text_color(theme::danger()).child(why))),
                )
                .child(pill(if waiting { "Waiting…" } else { "Sign in" }).when(waiting, |pill| pill.text_color(theme::text_soft())));
            card = card.child(row);
        }
        let note = |words: &'static str| {
            div().w_full().text_align(gpui::TextAlign::Center).phone_preview().text_color(theme::text_faint()).child(words)
        };
        div().size_full().flex().flex_col().child(
            div()
                .id("phone-first-run")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .child(
                    div()
                        .min_h_full()
                        .pt(px(tokens::safe_top() + 40.))
                        .pb(px(tokens::safe_bottom() + 24.))
                        .px(px(tokens::GUTTER))
                        .flex()
                        .flex_col()
                        .items_center()
                        .child(brand_mark(64.))
                        .child(
                            div()
                                .mt(px(20.))
                                .text_size(px(tokens::scaled(26.)))
                                .line_height(px(tokens::scaled(32.)))
                                .font_weight(theme::SEMIBOLD)
                                .text_color(theme::text())
                                .child("Welcome to tinyclient"),
                        )
                        .child(
                            div()
                                .mt(px(8.))
                                .max_w(px(320.))
                                .text_align(gpui::TextAlign::Center)
                                .text_size(px(tokens::scaled(16.)))
                                .line_height(px(tokens::scaled(22.)))
                                .text_color(theme::text_soft())
                                .child("Sign in to one network to start. Add the others whenever you like."),
                        )
                        .child(div().mt(px(32.)).w_full().child(card))
                        .child(div().mt(px(20.)).w_full().flex().flex_col().gap(px(6.)).child(note("Signal and Google Messages link from tinyclient on a computer.")).child(note("Your sign-ins stay on this phone."))),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_phone_offers_every_network_but_the_two_a_computer_links() {
        let offered: Vec<Network> = phone_networks().collect();
        assert_eq!(offered, vec![Network::Discord, Network::Slack, Network::Telegram, Network::Teams, Network::Tiny]);
        assert!(!on_phone(Network::Signal) && !on_phone(Network::Messages));
        assert!(offered.iter().all(|network| !line(*network).is_empty()));
    }

    #[test]
    fn a_phone_shows_the_settings_it_can_act_on() {
        use crate::gui::settings::{on_phone, Section};
        let shown: Vec<Section> = Section::ALL.into_iter().filter(|section| on_phone(*section)).collect();
        assert_eq!(shown, vec![Section::Accounts, Section::Sounds, Section::Appearance, Section::Messages, Section::Privacy, Section::Storage, Section::About]);
    }
}
