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

/// Where a message leads for "Copy link". Prefers the canonical HTTPS `Message::permalink`
/// when supplied by the adapter (e.g. Slack `https://<workspace>.slack.com/archives/...`),
/// falling back to `Unsupported { link }` or per-network constructed addresses.
pub fn message_link(conversation: &protocol::Key, space: Option<&str>, message: &protocol::Message) -> Option<String> {
    if !message.permalink.is_empty() {
        return Some(message.permalink.clone());
    }
    if let protocol::MessageKind::Unsupported { link } = &message.kind {
        if !link.is_empty() {
            return Some(link.clone());
        }
    }
    if message.state != protocol::SendState::Sent || message.deleted {
        return None;
    }
    let (channel, id) = (&conversation.id, &message.id);
    match conversation.network {
        protocol::Network::Discord => {
            let place = space.unwrap_or("@me");
            Some(format!("https://discord.com/channels/{place}/{channel}/{id}"))
        }
        protocol::Network::Slack => space.map(|team| format!("slack://channel?team={team}&id={channel}&message={id}")),
        protocol::Network::Tiny => space.and_then(|origin| protocol::message_link(origin, channel, id)),
        protocol::Network::Telegram | protocol::Network::Teams | protocol::Network::Signal | protocol::Network::Messages => None,
    }
}

/// Where "Open in <network>" opens a message in the network's native app (keeps `slack://`
/// for Slack when a team id is known so the desktop app handles it directly).
pub fn open_in_app_link(conversation: &protocol::Key, space: Option<&str>, message: &protocol::Message) -> Option<String> {
    if conversation.network == protocol::Network::Slack && message.state == protocol::SendState::Sent && !message.deleted {
        if let Some(team) = space {
            return Some(format!("slack://channel?team={team}&id={}&message={}", conversation.id, message.id));
        }
    }
    message_link(conversation, space, message)
}

/// Chooses the channel key for `message_link`: a Slack thread's replies link into their parent
/// channel, while a Discord thread is itself a channel.
pub(super) fn message_link_for(
    pane_key: Option<&protocol::Key>,
    open: &protocol::Conversation,
    message: &protocol::Message,
) -> Option<String> {
    let key = match (pane_key, open.key.network) {
        (Some(mine), protocol::Network::Discord) => mine,
        _ => &open.key,
    };
    message_link(key, open.space.as_deref(), message)
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

/// Builds a compact label for a message/channel link when the target conversation is already
/// present in the local snapshot (zero network prefetch).
pub(super) fn network_link_chip(url: &str, conversations: &[protocol::Conversation]) -> Option<String> {
    let parsed = protocol::link::parse_network_link(url)?;
    let hit = conversations
        .iter()
        .find(|c| c.key.network == parsed.network && c.key.id == parsed.channel)?;
    let name = if hit.title.is_empty() { &hit.key.id } else { &hit.title };
    let label = match hit.kind {
        protocol::ConversationKind::Channel | protocol::ConversationKind::Announcement | protocol::ConversationKind::Forum => {
            if name.starts_with('#') { name.to_owned() } else { format!("#{name}") }
        }
        _ => name.to_owned(),
    };
    Some(if parsed.message_id.is_some() { format!("{label} ↗") } else { label })
}

/// Web URL fallback when a `#channel` mention is clicked but chatd has no row for it.
pub(super) fn channel_mention_fallback_url(
    network: protocol::Network,
    space: Option<&str>,
    channel_id: &str,
) -> Option<String> {
    if network != protocol::Network::Discord || channel_id.is_empty() || !channel_id.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let place = space
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .unwrap_or("@me");
    Some(format!("https://discord.com/channels/{place}/{channel_id}"))
}

impl Pane {
    /// Opens a link externally (in the system browser), asking for confirmation per preferences.
    pub(super) fn open_external_link(&mut self, url: &str, window: &mut Window, cx: &mut Context<Self>) {
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

    /// A click on a link in a message. If `external` is false and the URL names a conversation
    /// or message on a signed-in network, asks chatd to resolve it and opens it in-app; otherwise
    /// falls through to the external browser. Alt-click (`external == true`) always opens outside.
    pub(super) fn open_link(&mut self, url: &str, external: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !external {
            if let Some((origin, conversation_id, message_id)) = protocol::parse_message_link(url) {
                let _ = self.shell.update(cx, |shell, cx| {
                    shell.tiny_open_message_link(&origin, &conversation_id, &message_id, cx);
                });
                return;
            }
        }
        if !external && protocol::link::parse_network_link(url).is_some() {
            let owned = url.to_owned();
            let request = protocol::Request::Resolve { url: owned.clone() };
            let window_handle = window.window_handle();
            cx.spawn(async move |this, cx| {
                let answer = cx.background_executor().spawn(async move { crate::client::ask(&request) }).await;
                if let Ok(protocol::Response::Resolved { conversation: Some(key), message_id, title }) = answer {
                    let _ = this.update(cx, |this, cx| {
                        let _ = this.shell.update(cx, |shell, cx| shell.open_resolved_link(key, message_id, &title, cx));
                    });
                    return;
                }
                let _ = cx.update_window(window_handle, |_, window, cx| {
                    let _ = this.update(cx, |this, cx| this.open_external_link(&owned, window, cx));
                });
            })
            .detach();
            return;
        }
        self.open_external_link(url, window, cx);
    }

    /// A click on a `#channel` mention: resolves the channel via `Request::Conversation` and opens
    /// it in-app. If chatd does not know the row (or Alt is held), falls back to the web link.
    pub(super) fn open_channel_mention(
        &mut self,
        channel_id: &str,
        external: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(open) = self.open.as_ref() else {
            return;
        };
        let key = protocol::Key::new(&open.key.account(), channel_id);
        let space = self
            .shell
            .upgrade()
            .and_then(|shell| shell.read(cx).snapshot.as_ref()?.open.as_ref()?.space.clone());
        let fallback = channel_mention_fallback_url(open.key.network, space.as_deref(), channel_id);
        if external {
            if let Some(url) = fallback {
                self.open_external_link(&url, window, cx);
            }
            return;
        }
        let request = protocol::Request::Conversation { conversation: key };
        let window_handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let answer = cx.background_executor().spawn(async move { crate::client::ask(&request) }).await;
            if let Ok(protocol::Response::Conversation { conversation }) = answer {
                let _ = this.update(cx, |this, cx| {
                    let _ = this.shell.update(cx, |shell, cx| {
                        shell.open_resolved_link(conversation.key, None, &conversation.title, cx);
                    });
                });
                return;
            }
            if let Some(url) = fallback {
                let _ = cx.update_window(window_handle, |_, window, cx| {
                    let _ = this.update(cx, |this, cx| this.open_external_link(&url, window, cx));
                });
            }
        })
        .detach();
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

    #[test]
    fn message_link_round_trips_through_parse_network_link() {
        let mut key = protocol::Key { network: protocol::Network::Discord, account: "1".into(), id: "12345".into() };
        let said = protocol::Message::plain("67890", "Mara", "hello", 1, false);
        let url = message_link(&key, Some("4000"), &said).unwrap();
        assert_eq!(url, "https://discord.com/channels/4000/12345/67890");
        let parsed = protocol::link::parse_network_link(&url).unwrap();
        assert_eq!(parsed.network, protocol::Network::Discord);
        assert_eq!(parsed.space.as_deref(), Some("4000"));
        assert_eq!(parsed.channel, "12345");
        assert_eq!(parsed.message_id.as_deref(), Some("67890"));

        let dm_url = message_link(&key, None, &said).unwrap();
        let parsed_dm = protocol::link::parse_network_link(&dm_url).unwrap();
        assert_eq!(parsed_dm.space, None);
        assert_eq!(parsed_dm.channel, "12345");

        key.network = protocol::Network::Slack;
        key.id = "C9".into();
        let slack_url = message_link(&key, Some("T1"), &said).unwrap();
        assert_eq!(slack_url, "slack://channel?team=T1&id=C9&message=67890");
        let parsed_slack = protocol::link::parse_network_link(&slack_url).unwrap();
        assert_eq!(parsed_slack.network, protocol::Network::Slack);
        assert_eq!(parsed_slack.space.as_deref(), Some("T1"));
        assert_eq!(parsed_slack.channel, "C9");
        assert_eq!(parsed_slack.message_id.as_deref(), Some("67890"));
    }

    #[test]
    fn network_link_chip_labels_known_channel_without_prefetch() {
        let conv = protocol::Conversation::new(
            protocol::Key { network: protocol::Network::Discord, account: "1".into(), id: "12345".into() },
            protocol::ConversationKind::Channel,
            "engineering",
        );
        assert_eq!(
            network_link_chip("https://discord.com/channels/4000/12345/67890", &[conv.clone()]).as_deref(),
            Some("#engineering ↗")
        );
        assert_eq!(
            network_link_chip("https://discord.com/channels/4000/99999/67890", &[conv]),
            None
        );
    }

    #[test]
    fn channel_mention_fallback_url_validates_discord_snowflakes() {
        assert_eq!(
            channel_mention_fallback_url(protocol::Network::Discord, Some("4000"), "4102").as_deref(),
            Some("https://discord.com/channels/4000/4102")
        );
        assert_eq!(
            channel_mention_fallback_url(protocol::Network::Discord, None, "4102").as_deref(),
            Some("https://discord.com/channels/@me/4102")
        );
        assert_eq!(
            channel_mention_fallback_url(protocol::Network::Discord, Some("4000"), "bad/id"),
            None
        );
        assert_eq!(
            channel_mention_fallback_url(protocol::Network::Slack, Some("T1"), "C1"),
            None
        );
    }
}
