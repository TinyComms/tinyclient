//! Opening a link from a message: where it really goes, said before the browser is started.
//!
//! The decision is `protocol::link::Destination` (host, registrable domain, punycode, lookalike
//! check, tracking strip). This file is only the question and what the answer does. The browser
//! is started with the system's own opener (`cx.open_url`): it is handed the address alone, so no
//! page is the referrer and no `Referer` is sent. Only http and https ever get that far.

use super::Pane;
use crate::gui::app::prefs;
use crate::gui::components::{button, fade_in, modal, modal_buttons, ButtonKind};
use crate::gui::theme::{self, Type};
use gpui::{anchored, deferred, div, point, prelude::*, px, AnyElement, ClipboardItem, Context, KeyDownEvent, MouseButton, Window};
use protocol::link::{Destination, Risk};

/// The question on screen: the link, and whether "Don't ask again" is ticked.
pub(super) struct LinkAsk {
    pub dest: Destination,
    pub remember: bool,
}

impl LinkAsk {
    /// What the browser is given, with the tracking strip applied if the person turned it on.
    fn target(&self) -> String {
        self.dest.target(prefs::choices().strip_tracking)
    }

    /// A domain this device may stop asking about. Never one that looks like a lookalike.
    fn can_remember(&self) -> bool {
        self.dest.risk != Risk::Lookalike && !self.dest.has_userinfo
    }
}

/// A `mailto:` link, which other networks put on an address. It opens the mail app as it is:
/// no question, no tracking strip. Every other scheme that is not http or https stays refused.
pub(super) fn is_mailto(url: &str) -> bool {
    url.get(..7).is_some_and(|scheme| scheme.eq_ignore_ascii_case("mailto:"))
        && url.len() <= 2048
        && !url.chars().any(|c| c.is_whitespace() || c.is_control() || c == '"')
}

/// Whether a link to `dest` opens without a question, given what the person chose.
pub(super) fn opens_at_once(dest: &Destination, confirm: bool, trusted: bool) -> bool {
    // A lookalike or a "user@" address is always asked about: that is what the question is for.
    let suspicious = dest.risk == Risk::Lookalike || dest.has_userinfo;
    !suspicious && (!confirm || trusted)
}

impl Pane {
    /// A click on a link in a message. Anything that is not a web address does nothing.
    pub(super) fn open_link(&mut self, url: &str, window: &mut Window, cx: &mut Context<Self>) {
        // An e-mail address Telegram or Slack made a link of: the mail app's, not a site.
        if is_mailto(url) {
            cx.open_url(url);
            return;
        }
        let Some(dest) = Destination::parse(url) else {
            return;
        };
        let choices = prefs::choices();
        let trusted = prefs::trusted_links().contains(&dest.domain);
        if opens_at_once(&dest, choices.confirm_links, trusted) {
            cx.open_url(&dest.target(choices.strip_tracking));
            return;
        }
        self.link_ask = Some(LinkAsk { dest, remember: false });
        self.hovered = None;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Opens what is asked about, and remembers its domain if that was ticked.
    fn go_to_link(&mut self, cx: &mut Context<Self>) {
        let Some(ask) = self.link_ask.take() else {
            return;
        };
        if ask.remember && ask.can_remember() {
            let mut trusted = prefs::trusted_links();
            if trusted.add(&ask.dest.domain) {
                prefs::set_trusted_links(trusted);
                let _ = self.shell.update(cx, |shell, cx| shell.save_prefs(cx));
            }
        }
        cx.open_url(&ask.target());
        cx.notify();
    }

    pub(super) fn close_link_ask(&mut self, cx: &mut Context<Self>) {
        if self.link_ask.take().is_some() {
            cx.notify();
        }
    }

    /// Keys while the question is up. True when the key was its.
    pub(super) fn link_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        if self.link_ask.is_none() {
            return false;
        }
        match event.keystroke.key.as_str() {
            "enter" => self.go_to_link(cx),
            "escape" => self.close_link_ask(cx),
            _ => {}
        }
        true
    }

    pub(super) fn link_layer(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let ask = self.link_ask.as_ref()?;
        let dest = &ask.dest;
        let target = ask.target();

        let mut body = div().mt(px(12.)).flex().flex_col().gap(px(10.));
        let mut site = div().flex().flex_col().gap(px(2.)).child(div().type_title().font_weight(theme::SEMIBOLD).child(dest.domain_shown.clone()));
        if dest.host_shown != dest.domain_shown {
            site = site.child(div().type_ui().text_color(theme::text_soft()).child(dest.host_shown.clone()));
        }
        body = body.child(site);

        let note = |warn: bool, words: String| {
            div()
                .p(px(10.))
                .rounded(px(theme::R_CARD))
                .bg(if warn { theme::warning_soft() } else { theme::raised() })
                .type_ui()
                .text_color(if warn { theme::text() } else { theme::text_soft() })
                .child(words)
        };
        match dest.risk {
            Risk::Lookalike => {
                body = body.child(note(
                    true,
                    format!(
                        "Careful: this name mixes alphabets, or uses letters that look like Latin ones. It may be pretending to be another site. Its real address is {}.",
                        dest.host
                    ),
                ));
            }
            Risk::Idn => {
                body = body.child(note(false, format!("This site's name uses letters outside A to Z. Its address on the internet is {}.", dest.host)));
            }
            Risk::None => {}
        }
        if dest.has_userinfo {
            body = body.child(note(true, "Careful: the part of the address before the @ is a user name, not the site. The site is the one above.".to_owned()));
        }
        body = body.child(
            div()
                .id("link-address")
                .p(px(10.))
                .rounded(px(theme::R_CARD))
                .bg(theme::raised())
                .line_clamp(4)
                .type_small()
                .text_color(theme::text_soft())
                .child(target.clone()),
        );
        if ask.can_remember() {
            let on = ask.remember;
            body = body.child(
                div()
                    .id("link-remember")
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .cursor_pointer()
                    .type_ui()
                    .child(
                        div()
                            .flex_none()
                            .size(px(16.))
                            .rounded(px(4.))
                            .border_1()
                            .border_color(if on { theme::accent() } else { theme::border_strong() })
                            .when(on, |box_| box_.bg(theme::accent()))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_color(theme::on_accent())
                            .child(if on { "✓" } else { "" }),
                    )
                    .child(format!("Don't ask again for {}", dest.domain_shown))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(ask) = this.link_ask.as_mut() {
                            ask.remember = !ask.remember;
                            cx.notify();
                        }
                    })),
            );
        }

        // A phone's dialog keeps 16 pt of the window either side.
        let room = f32::from(window.viewport_size().width) - 32.;
        let copy = target.clone();
        let panel = modal("Open this link?")
            .w(px(if crate::gui::phone::active() { room.min(440.) } else { 440. }))
            .child(body)
            .child(
                modal_buttons()
                    .child(button("link-cancel", ButtonKind::Ghost, "Cancel").on_click(cx.listener(|this, _, _, cx| this.close_link_ask(cx))))
                    .child(button("link-copy", ButtonKind::Secondary, "Copy link").on_click(cx.listener(move |this, _, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                        this.close_link_ask(cx);
                    })))
                    .child(button("link-open", ButtonKind::Primary, "Open").on_click(cx.listener(|this, _, _, cx| this.go_to_link(cx)))),
            )
            // A press on the panel is not a press on the scrim behind it.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        let size = window.viewport_size();
        let scrim = div()
            .id("link-ask")
            .w(size.width)
            .h(size.height)
            .bg(theme::scrim())
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.close_link_ask(cx)))
            .child(fade_in("link-ask-panel", 160, panel));
        Some(deferred(anchored().position(point(px(0.), px(0.))).child(scrim)).priority(1).into_any_element())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dest(url: &str) -> Destination {
        Destination::parse(url).unwrap()
    }

    #[test]
    fn a_link_is_asked_about_unless_it_is_trusted_or_the_question_is_off() {
        let plain = dest("https://example.com/a");
        assert!(!opens_at_once(&plain, true, false), "the default asks");
        assert!(opens_at_once(&plain, true, true), "a trusted domain does not");
        assert!(opens_at_once(&plain, false, false), "with the question off nothing asks");
    }

    #[test]
    fn mailto_passes_unchanged_and_every_other_non_web_scheme_is_still_dropped() {
        for ok in ["mailto:a@example.com", "MAILTO:a@example.com?subject=Hi%20there&cc=b@example.com"] {
            assert!(is_mailto(ok), "{ok}");
            assert!(Destination::parse(ok).is_none(), "{ok} is not a web destination: no dialog, no strip");
        }
        for bad in [
            "javascript:alert(1)",
            "file:///c:/windows/system32",
            "intent://x#Intent;scheme=http;end",
            "tg://resolve?domain=x",
            "ftp://example.com/",
            "data:text/html,x",
            "mailto:a b@example.com",
            "mailto:a\u{0}@example.com",
            "xmailto:a@example.com",
            "mailto",
            "",
        ] {
            assert!(!is_mailto(bad), "{bad:?}");
            assert!(Destination::parse(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn a_lookalike_or_a_user_name_is_asked_about_whatever_was_chosen() {
        for url in ["https://\u{0430}pple.com/", "https://paypal.com@evil.example/"] {
            let d = dest(url);
            assert!(!opens_at_once(&d, false, true), "{url}");
            let ask = LinkAsk { dest: d, remember: true };
            assert!(!ask.can_remember(), "{url} cannot be trusted");
        }
        let idn = dest("https://bücher.example/");
        assert!(opens_at_once(&idn, true, true));
        assert!(LinkAsk { dest: idn, remember: false }.can_remember());
    }
}
