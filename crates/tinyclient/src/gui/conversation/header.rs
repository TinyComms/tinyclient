//! The strip above the messages: where this is, and the few controls that act on the whole
//! conversation. The buttons belong to the slices that own what they do; this places them.

use crate::gui::pane::Layer;
use crate::gui::components::place::{placed, Layer as OverlayLayer, Place};
use crate::gui::components::{glyph, icon_button, menu, menu_item, network_icon, tip, Avatar, Shape};
use crate::gui::chrome::{self, End};
use crate::gui::layout::{drags_window, TOP_BAR};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use gpui::{canvas, div, prelude::*, px, AnyElement, Context, Div, MouseButton, Window};
use protocol::{Conversation, ConversationKind, Key, Link, Mark, MediaFile, Network};

impl Shell {
    /// The picture of a conversation: its own, or for a direct one the other person's as the
    /// people hub knows it. Asked for when it is not on disk.
    pub(super) fn conversation_picture(&self, open: &Conversation, cx: &mut Context<Self>) -> Option<MediaFile> {
        let hub = self.people_state.hub();
        let peer = open.peer.as_ref().map(|peer| Key { id: peer.clone(), ..open.key.clone() });
        let file = open.avatar.clone().or_else(|| peer.and_then(|peer| hub.person(&peer)).and_then(|person| person.avatar));
        hub.avatar(&open.key.account(), file.as_ref(), cx)
    }

    /// Where that picture is on disk. A Tiny member's picture, already fetched for the server
    /// page, is used when the people hub has none. The bytes are not logged.
    pub(in crate::gui) fn conversation_face(&self, open: &Conversation, cx: &mut Context<Self>) -> Option<std::path::PathBuf> {
        if let Some(path) = self.conversation_picture(open, cx).and_then(|file| file.path).filter(|path| !path.is_empty()) {
            return Some(std::path::PathBuf::from(path));
        }
        let id = open.peer.as_deref().filter(|id| !id.is_empty())?;
        self.tiny_member_picture(&open.key.account(), id)
    }

    /// My own picture on the account a conversation belongs to.
    pub(super) fn own_picture(&self, conversation: &Key) -> Option<MediaFile> {
        let account = conversation.account();
        let accounts = self.snapshot.iter().flat_map(|snapshot| &snapshot.accounts);
        accounts.filter(|status| status.key == account).find_map(|status| status.avatar.clone())
    }

    /// The strip above the messages (07 §1): panel, back and forward, picture, title and the
    /// line after it on one center, the call, then search and the pane's buttons, Details last.
    /// The encryption chip carries an icon, so the row centers instead of sharing a text
    /// baseline: a baseline puts that chip below the name and the presence word.
    pub(super) fn strip(&self, open: &Conversation, link: Link, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let direct = open.kind == ConversationKind::Direct;
        let teams_channel = open.key.network == protocol::Network::Teams && open.kind == ConversationKind::Channel;
        let saved = open.special == protocol::Special::Saved && open.key.network == protocol::Network::Telegram;
        let picture = match open.kind {
            ConversationKind::Direct | ConversationKind::Group => Avatar::new(&open.key, &open.title, 24.),
            ConversationKind::Voice => Avatar::channel("icons/volume-2.svg", 24.),
            ConversationKind::Announcement => Avatar::channel("icons/megaphone.svg", 24.),
            ConversationKind::Thread | ConversationKind::Forum => Avatar::channel("icons/messages-square.svg", 24.),
            ConversationKind::Channel if open.private => Avatar::channel("icons/lock.svg", 24.),
            ConversationKind::Channel => Avatar::channel("icons/hash.svg", 24.),
        };
        let shape = if open.kind.is_direct() { Shape::Person } else { Shape::Place };
        let picture = picture.shape(shape).image_at(self.conversation_face(open, cx));
        // A direct conversation and a group share one face with their row: the picture, and
        // for a person the presence dot.
        let face = if direct || open.kind == ConversationKind::Group || teams_channel {
            let base = self.conversation_avatar(open, 24., theme::canvas(), cx);
            let story_peer = direct
                .then(|| open.peer.as_ref().map(|peer| Key { id: peer.clone(), ..open.key.clone() }))
                .flatten();
            if let Some((peer_key, ring)) = story_peer.and_then(|k| self.story_ring_for(&k).map(|r| (k, r.clone()))) {
                let wrapped = crate::gui::stories::wrap_avatar_ring(base, 24., ring.has_unseen(), ring.close_friends);
                div()
                    .id("header-story-avatar")
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_story_viewer(peer_key.clone(), None, cx);
                    }))
                    .child(wrapped)
                    .into_any_element()
            } else {
                base.into_any_element()
            }
        } else if saved {
            Avatar::channel("icons/bookmark.svg", 24.).build().into_any_element()
        } else {
            picture.build().into_any_element()
        };
        let trouble = match link {
            Link::Connected => None,
            Link::Offline => Some("Reconnecting"),
            Link::Expired => Some("Signed out by the network"),
            Link::SignedOut => Some("Signed out"),
        };
        // The network's icon leads a title only where networks are mixed: a conversation in
        // no Space was reached from the Inbox, which lists every network together. One with a
        // Space says where it is in words.
        let mixed = open.space.is_none();
        let controls = self.strip_controls(window, cx);
        let sidebar_shown = self.panel_button(window, cx).is_none();
        // True usable width in the conversation header, subtracting left chrome and
        // Windows/Linux caption buttons (`chrome::end_pad`).
        let raw_room = self.conversation_room();
        let room = if raw_room > 0. { raw_room } else { f32::from(window.viewport_size().width) };
        let left_chrome = (if !sidebar_shown { 36. } else { 0. }) + 56. + 44.;
        let end_pad = chrome::end_pad(End::Conversation, 12.);
        let usable = (room - theme::PANE_PAD - end_pad - left_chrome).max(0.);
        let compact = usable < 460.;
        let overflow_narrow = usable < 520.;
        if !overflow_narrow {
            self.conversation_state.header_overflow.set(false);
        }
        let beside = (usable >= 360.).then(|| self.beside(open)).flatten();
        let role = (usable >= 380.).then(|| self.tiny_header_role(open)).flatten();
        let search = icon_button("search", "icons/search.svg", self.pane_shows(|layer| matches!(layer, Layer::Search)))
            .aria_label("Search in conversation")
            .tooltip(tip("Search in conversation", Some("⌘F")))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, _, window, cx| this.toggle_conversation_search(window, cx)));
        let can_add = self.can_add_people(open);
        let add_people = (!overflow_narrow && can_add).then(|| {
            let conversation = open.clone();
            icon_button("add-people", "icons/user-plus.svg", false)
                .tooltip(tip("Add people", None))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, window, cx| this.open_add_people(&conversation, window, cx)))
        });
        let can_threads = open.key.network == Network::Discord
            && matches!(open.kind, ConversationKind::Channel | ConversationKind::Announcement);
        let threads_btn = (!overflow_narrow && can_threads).then(|| {
            icon_button("channel-threads", "icons/messages-square.svg", self.pane_shows(|layer| matches!(layer, Layer::Threads(_))))
                .aria_label("Threads")
                .tooltip(tip("Threads", None))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(|this, _, _, cx| this.toggle_channel_threads(cx)))
        });
        let can_thread_settings = open.key.network == Network::Discord
            && open.kind == ConversationKind::Thread
            && (open.rights.manage_threads || open.rights.rename);
        let thread_settings = (!overflow_narrow && can_thread_settings).then(|| {
            icon_button("thread-settings", "icons/pencil.svg", false)
                .aria_label("Thread settings")
                .tooltip(tip("Thread settings", None))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(|this, _, _, cx| this.composer_toggle_thread_settings(cx)))
        });
        let thread_membership = (open.key.network == Network::Discord && open.kind == ConversationKind::Thread).then(|| {
            let key = open.key.clone();
            let joined = open.followed;
            let full_label = if joined { "Leave thread" } else { "Join thread" };
            let label = if compact {
                if joined { "Leave" } else { "Join" }
            } else {
                full_label
            };
            div()
                .id("thread-follow-toggle")
                .role(gpui::Role::Button)
                .aria_label(full_label)
                .tooltip(tip(full_label, None))
                .tab_index(0)
                .focus_visible(|style| style.shadow(theme::focus_ring()))
                .h(px(theme::CONTROL))
                .px(px(8.))
                .rounded(px(theme::R_CONTROL))
                .border_1()
                .border_color(theme::border())
                .flex()
                .items_center()
                .type_small()
                .text_color(theme::text_soft())
                .cursor_pointer()
                .hover(|s| s.bg(theme::hover()).text_color(theme::text()))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_follow_thread(key.clone(), !joined, cx)))
                .child(label)
        });
        let can_setup = open.key.network == Network::Discord && open.space.is_some();
        let setup = (!overflow_narrow && can_setup).then(|| {
            let space = Key { id: open.space.clone().unwrap_or_default(), ..open.key.clone() };
            icon_button("server-setup", "icons/list.svg", false)
                .aria_label("Server setup")
                .tooltip(tip("Server setup", None))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, _, cx| this.open_server_gate(space.clone(), cx)))
        });
        // When the sidebar is visible, the call dock already shows the active call; only show the
        // top-bar live-call hint when the sidebar is collapsed/hidden.
        let live_hint = (!sidebar_shown)
            .then(|| self.snapshot.as_ref().and_then(|snapshot| self.call_live_hint(snapshot, cx)))
            .flatten();
        let call = self.snapshot.as_ref().and_then(|snapshot| self.call_pill(snapshot, compact, cx)).map(|call| {
            call.whitespace_nowrap()
        });
        let stage_pill = self.snapshot.as_ref().and_then(|snapshot| self.call_return_stage_pill(snapshot, cx));
        // The window's own buttons sit over the strip's right end: a hairline keeps the actions off them.
        let window_buttons = end_pad > 12.;
        // Lit means "this kind is in the pane now". In a channel, Members and Details are two
        // values of one switch; beside a direct conversation Details is the other person.
        let members = (!direct && !teams_channel).then(|| {
            icon_button("members", "icons/users.svg", self.pane_shows(|layer| matches!(layer, Layer::Members(_))))
                .aria_label("Members")
                .tooltip(tip("Members", None))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(|this, _, _, cx| this.toggle_member_list(cx)))
        });
        // Beside a channel or group Details has a button; beside a direct conversation the title
        // is the way to the person, a link that opens their profile, so no button is drawn there.
        let details = (!direct).then(|| {
            icon_button("details", "icons/info.svg", self.pane_shows(|layer| matches!(layer, Layer::Details(_))))
                .aria_label("Details")
                .tooltip(tip("Details", Some("⌘.")))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(|this, _, _, cx| this.toggle_details(cx)))
        });
        let pop_key = open.key.clone();
        let windowed = (!overflow_narrow).then(|| {
            icon_button("pop-out-conversation", "icons/picture-in-picture-2.svg", false)
                .aria_label("Open in New Window")
                .tooltip(tip("Open in New Window", None))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, _, cx| this.pop_out_conversation(pop_key.clone(), cx)))
        });
        let overflow_open = overflow_narrow && self.conversation_state.header_overflow.get();
        let overflow_btn = overflow_narrow.then(|| {
            let anchor = self.conversation_state.header_overflow_anchor.clone();
            icon_button("header-overflow", "icons/ellipsis.svg", overflow_open)
                .relative()
                .aria_label("More conversation actions")
                .tooltip(tip("More actions", None))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(|this, _, _, cx| {
                    let next = !this.conversation_state.header_overflow.get();
                    this.conversation_state.header_overflow.set(next);
                    cx.notify();
                }))
                .child(
                    canvas(move |bounds, _, _| anchor.set(Some(bounds)), |_, _, _, _| {})
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full(),
                )
        });
        let overflow_menu = overflow_open
            .then(|| self.header_overflow_menu(open, can_setup, can_thread_settings, can_threads, can_add, window, cx))
            .flatten();
        // The title keeps 96 (48 when narrow), or its whole width when shorter, so a short name is
        // not padded away from what follows. The line after it truncates first, then the title.
        let title_tip = if direct { "Open profile".to_owned() } else { open.title.clone() };
        let title_cap = if usable < 460. { 48. } else { 96. };
        let title = div()
            .id("conversation-title")
            .role(if direct { gpui::Role::Button } else { gpui::Role::Heading })
            .aria_label(if direct { format!("Open profile for {}", open.title) } else { open.title.clone() })
            .flex_initial()
            .min_w(title_floor(&open.title, window, title_cap))
            .truncate()
            .type_title()
            .tooltip(tip(title_tip, direct.then_some("⌘.")))
            .when_some(role.as_ref().and_then(|role| role.color), |title, color| title.text_color(theme::role_color(color)))
            .child(open.title.clone());
        let title = if direct {
            title
                .px(px(4.))
                .mx(px(-4.))
                .rounded(px(theme::R_CONTROL))
                .tab_index(0)
                .focus_visible(|style| style.shadow(theme::focus_ring()))
                .cursor_pointer()
                .hover(|style| style.underline())
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(|this, _, _, cx| this.toggle_details(cx)))
        } else {
            title
        };
        let space = open.space.clone().map(|id| Key { id, ..open.key.clone() });
        let line = beside.map(|beside| match beside {
            Beside::Activity(words) => div()
                .flex_1()
                .min_w_0()
                .truncate()
                .type_ui()
                .text_color(theme::accent_text())
                .child(words)
                .into_any_element(),
            Beside::Person(line) => person_line(&line).into_any_element(),
            Beside::Path(subtitle) => div()
                .id("reveal")
                .flex_1()
                .min_w_0()
                .truncate()
                .type_ui()
                .text_color(theme::text_faint())
                .hover(|style| style.text_color(theme::text_soft()))
                .cursor_pointer()
                .tooltip(tip("Reveal in sidebar", None))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                // CONTRACT: the sidebar has no "reveal this row" hook (slice B); its Space is shown.
                .on_click(cx.listener(move |this, _, _, cx| match space.clone() {
                    Some(space) => this.go(crate::gui::sidebar::Mode::Space(space), cx),
                    None => this.go(crate::gui::sidebar::Mode::Inbox, cx),
                }))
                .child(subtitle)
                .into_any_element(),
        });
        drags_window(div())
            .h(px(TOP_BAR))
            .flex_none()
            .overflow_hidden()
            .pl(px(theme::PANE_PAD))
            .pr(px(end_pad))
            .flex()
            .items_center()
            .border_b_1()
            .border_color(theme::hairline())
            .child(controls)
            .child(
                div()
                    .id("conversation-face")
                    .flex_none()
                    .ml(px(12.))
                    .mr(px(8.))
                    .when_some(self.presence_words(open), |face, words| face.tooltip(tip(words, None)))
                    .child(face),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    // Nothing in this row draws over the call or the buttons, however narrow.
                    .overflow_hidden()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .children(mixed.then(|| div().flex_none().self_center().mr(px(-2.)).child(network_icon(open.key.network, 14.))))
                    .child(title)
                    .children(self.discord_account_label(&open.key).map(|name| div().flex_none().type_small().text_color(theme::text_faint()).child(name)))
                    .children(mark(open))
                    .children(role.map(|role| role_chip(&role.name, role.color)))
                    .children(self.encryption_chip(open, usable, cx))
                    .children(self.disappearing_chip(open, usable < 520., cx))
                    .children(self.meeting_chip(open, usable < 540., cx))
                    .children(line)
                    .children(trouble.map(|words| div().flex_none().type_small().text_color(theme::text_faint()).child(words))),
            )
            .children(live_hint.map(|hint| div().flex_none().ml(px(8.)).child(hint)))
            .children(self.follow_button(open, compact, cx).map(|follow| div().flex_none().ml(px(8.)).child(follow)))
            .children(call.map(|call| div().flex_none().ml(px(8.)).child(call)))
            .children(self.meeting_button(open, link, compact, cx))
            .children(stage_pill.map(|pill| div().flex_none().ml(px(8.)).child(pill)))
            .children(thread_membership.map(|btn| div().flex_none().ml(px(8.)).child(btn)))
            .child(
                div()
                    .flex_none()
                    .ml(px(8.))
                    .flex()
                    .gap(px(4.))
                    .children(setup)
                    .children(thread_settings)
                    .children(threads_btn)
                    .children(add_people)
                    .child(search)
                    .children(members)
                    .children(details)
                    .children(windowed)
                    .children(overflow_btn),
            )
            .children(overflow_menu)
            .when(window_buttons, |bar| bar.child(div().flex_none().ml(px(8.)).w(px(1.)).h(px(16.)).bg(theme::hairline())))
    }

    #[allow(clippy::too_many_arguments)]
    fn header_overflow_menu(
        &self,
        open: &Conversation,
        can_setup: bool,
        can_thread_settings: bool,
        can_threads: bool,
        can_add: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let anchor = self.conversation_state.header_overflow_anchor.get()?;
        let max_w = (f32::from(window.viewport_size().width) - 16.).max(180.);
        let mut items = menu().w(px(220.0_f32.min(max_w)));
        if can_setup {
            let space = Key { id: open.space.clone().unwrap_or_default(), ..open.key.clone() };
            items = items.child(menu_item("header-ov-setup", Some("icons/list.svg"), "Server setup", None).on_click(
                cx.listener(move |this, _, _, cx| {
                    this.conversation_state.header_overflow.set(false);
                    this.open_server_gate(space.clone(), cx);
                }),
            ));
        }
        if can_thread_settings {
            items = items.child(menu_item("header-ov-thread-settings", Some("icons/pencil.svg"), "Thread settings", None).on_click(
                cx.listener(|this, _, _, cx| {
                    this.conversation_state.header_overflow.set(false);
                    this.composer_toggle_thread_settings(cx);
                }),
            ));
        }
        if can_threads {
            items = items.child(menu_item("header-ov-threads", Some("icons/messages-square.svg"), "Threads", None).on_click(
                cx.listener(|this, _, _, cx| {
                    this.conversation_state.header_overflow.set(false);
                    this.toggle_channel_threads(cx);
                }),
            ));
        }
        if can_add {
            let conversation = open.clone();
            items = items.child(menu_item("header-ov-add", Some("icons/user-plus.svg"), "Add people", None).on_click(
                cx.listener(move |this, _, window, cx| {
                    this.conversation_state.header_overflow.set(false);
                    this.open_add_people(&conversation, window, cx);
                }),
            ));
        }
        let pop_key = open.key.clone();
        items = items.child(menu_item("header-ov-popout", Some("icons/picture-in-picture-2.svg"), "Open in New Window", None).on_click(
            cx.listener(move |this, _, _, cx| {
                this.conversation_state.header_overflow.set(false);
                this.pop_out_conversation(pop_key.clone(), cx);
            }),
        ));
        let items = items
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.conversation_state.header_overflow.set(false);
                cx.notify();
            }));
        Some(placed("header-overflow-menu", Place::Below(anchor), OverlayLayer::Menu, window, items))
    }

    /// The Discord account's name, when two Discord accounts are signed in.
    fn discord_account_label(&self, key: &Key) -> Option<String> {
        if key.network != Network::Discord {
            return None;
        }
        let snapshot = self.snapshot.as_ref()?;
        let signed: Vec<_> = snapshot.accounts.iter().filter(|account| account.key.network == Network::Discord && account.link != Link::SignedOut).collect();
        if signed.len() < 2 {
            return None;
        }
        signed.iter().find(|account| account.key == key.account()).and_then(|account| (!account.name.is_empty()).then(|| account.name.clone()))
    }

    /// Follow button in the header strip for Discord announcement channels.
    fn follow_button(&self, open: &Conversation, compact: bool, cx: &mut Context<Self>) -> Option<gpui::Stateful<Div>> {
        if open.kind != ConversationKind::Announcement {
            return None;
        }
        let allowed = self
            .snapshot
            .as_ref()
            .and_then(|s| s.account(&open.key.account()))
            .is_some_and(|a| a.can.announcement_follow);
        if !allowed {
            return None;
        }
        let conv = open.clone();
        Some(
            div()
                .id("announcement-follow")
                .role(gpui::Role::Button)
                .aria_label("Follow")
                .tab_index(0)
                .focus_visible(|style| style.shadow(theme::focus_ring()))
                .flex_none()
                .h(px(theme::CONTROL))
                .when(compact, |el| el.w(px(theme::CONTROL)).justify_center())
                .when(!compact, |el| el.px(px(10.)).gap(px(5.)))
                .rounded(px(theme::R_CONTROL))
                .bg(theme::field())
                .border_1()
                .border_color(theme::border())
                .hover(|style| style.bg(theme::selected()).border_color(theme::border_strong()))
                .active(|style| style.bg(theme::pressed()))
                .flex()
                .items_center()
                .cursor_pointer()
                .type_small()
                .font_weight(theme::MEDIUM)
                .text_color(theme::text())
                .tooltip(tip("Follow this announcement channel into one of your servers", None))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, window, cx| this.open_follow(&conv, window, cx)))
                .child(glyph("icons/megaphone.svg", 13., theme::text_soft()))
                .when(!compact, |el| el.child("Follow")),
        )
    }

    /// Whether the conversation is end-to-end encrypted, where its network says: a lock and the
    /// words, or the plain words that the server can read it. A plaintext conversation keeps that
    /// label for good; it is never called encrypted. Opens Encryption, where the safety numbers are.
    fn encryption_chip(&self, open: &Conversation, usable: f32, cx: &mut Context<Self>) -> Option<gpui::Stateful<Div>> {
        if open.encryption.is_unknown() {
            return None;
        }
        let sealed = open.encryption.is_sealed();
        // Green when only the members' devices can read it. Amber when the server can.
        // A plaintext conversation is never drawn with the lock.
        let (icon, tone) = if sealed { ("icons/lock.svg", theme::online()) } else { ("icons/triangle-alert.svg", theme::warning()) };
        let show_words = if sealed { usable >= 760. } else { usable >= 480. };
        let short_words = if sealed { "Encrypted" } else { "Not encrypted" };
        let conversation = open.key.clone();
        Some(
            div()
                .id("conversation-encryption")
                .flex_none()
                .self_center()
                .h(px(20.))
                .when(!show_words, |el| el.w(px(20.)).justify_center())
                .when(show_words, |el| el.px(px(5.)).gap(px(4.)))
                .rounded(px(theme::R_CONTROL))
                .flex()
                .items_center()
                .cursor_pointer()
                .type_micro()
                .font_weight(theme::MEDIUM)
                .text_color(tone)
                .hover(|style| style.bg(theme::hover()).text_color(theme::text()))
                .tooltip(tip(open.encryption.explained(), None))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, window, cx| this.tiny_open_encryption(&conversation, sealed, window, cx)))
                .child(glyph(icon, 12., tone))
                .when(show_words, |el| el.child(short_words)),
        )
    }

    /// Shows the conversation's active disappearing-messages timer in the header.
    fn disappearing_chip(&self, open: &Conversation, compact: bool, cx: &mut Context<Self>) -> Option<gpui::Stateful<Div>> {
        let secs = open.disappearing.filter(|&s| s > 0)?;
        let label = protocol::format_disappearing(secs);
        let tip_words = if open.key.network == Network::Telegram {
            format!("Auto-delete: {label} · Click to manage")
        } else {
            format!("Disappearing messages: {label} · Click to manage")
        };
        Some(
            div()
                .id("conversation-disappearing")
                .flex_none()
                .self_center()
                .h(px(20.))
                .when(compact, |el| el.w(px(20.)).justify_center())
                .when(!compact, |el| el.px(px(7.)).gap(px(4.)))
                .rounded_full()
                .bg(theme::hover())
                .border_1()
                .border_color(theme::hairline())
                .flex()
                .items_center()
                .cursor_pointer()
                .type_micro()
                .font_weight(theme::MEDIUM)
                .text_color(theme::text_soft())
                .hover(|style| style.bg(theme::selected()).border_color(theme::border()).text_color(theme::text()))
                .tooltip(tip(tip_words, None))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(|this, _, _, cx| this.toggle_details(cx)))
                .child(glyph("icons/clock.svg", 12., theme::text_soft()))
                .when(!compact, |el| el.child(label)),
        )
    }

    /// Today's meeting for this chat: the time, and Join when the link is https.
    fn meeting_chip(&self, open: &Conversation, compact: bool, cx: &mut Context<Self>) -> Option<gpui::Stateful<Div>> {
        if open.key.network != Network::Teams {
            return None;
        }
        let event = self.sidebar_state.home.upcoming.iter().find(|event| {
            event.conversation == open.key.id && (self.sidebar_state.home.upcoming_account.is_empty() || self.sidebar_state.home.upcoming_account == open.key.account)
        })?;
        let base_label = crate::gui::time::meeting_display(event, crate::view::now());
        let join = event.join_url.clone();
        let can_join = join.starts_with("https://");
        let full_label = if can_join { format!("{base_label} · Join") } else { base_label.clone() };
        let shown_label = if compact { base_label } else { full_label.clone() };
        Some(
            div()
                .id("conversation-meeting")
                .flex_none()
                .self_center()
                .h(px(20.))
                .px(px(7.))
                .rounded_full()
                .bg(theme::raised())
                .flex()
                .items_center()
                .gap(px(4.))
                .type_micro()
                .font_weight(theme::MEDIUM)
                .text_color(theme::text_soft())
                .tooltip(tip(full_label, None))
                .when(can_join, |chip| {
                    chip.cursor_pointer()
                        .hover(|style| style.text_color(theme::text()))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let pane = this.pane(cx);
                            pane.update(cx, |pane, cx| pane.open_external_link(&join, window, cx));
                        }))
                })
                .child(glyph("icons/calendar.svg", 12., theme::text_soft()))
                .child(shown_label),
        )
    }

    /// The line after the title (07 §1): active typing/recording status first, then beside a direct
    /// conversation the person's status or presence; elsewhere where it is (`Space › group`) and its topic.
    fn beside(&self, open: &Conversation) -> Option<Beside> {
        let typists = self.people_state.hub().typists_in(&open.key);
        if let Some(words) = crate::gui::composer::typing_preview_for(&typists, open.kind != ConversationKind::Direct) {
            return Some(Beside::Activity(words));
        }
        if open.kind == ConversationKind::Direct {
            if let Some(mut line) = self.peer_line(&open.key) {
                // The avatar's dot says it; the word is its tooltip. A call and Telegram's
                // "last seen" have no dot of their own here, so they keep their words — unless
                // the header's own Join-call pill or our active call already shows it.
                let call_already_shown = (open.call == protocol::CallSupport::Native
                    && open.occupants.as_ref().is_some_and(|o| o.total > 0 || o.live))
                    || self.snapshot.as_ref().and_then(|s| s.call.as_ref()).is_some_and(|c| {
                        c.state == protocol::CallState::Joined
                            && open.peer.as_deref().is_some_and(|p| c.participants.iter().any(|u| u.id == p))
                    });
                if self.presence_dot(open).is_some_and(|shape| shape != crate::gui::people::presence::Shape::InCall)
                    || (call_already_shown && line.word == "In a call")
                {
                    line.word = "";
                }
                return (!line.word.is_empty() || !line.extra.is_empty()).then_some(Beside::Person(line));
            }
        }
        // A Tiny group sits with the direct messages, which is no place to name: it says how many
        // people are in it (its topic, as the adapter writes it).
        if open.kind == ConversationKind::Group && open.key.network == Network::Tiny && !open.topic.is_empty() {
            return Some(Beside::Path(open.topic.clone()));
        }
        let path = path_line(&open.subtitle());
        (!path.is_empty()).then(|| Beside::Path(path))
    }
}

/// The most a title holds on to as the row narrows: `cap`, or all of it when it is shorter.
fn title_floor(title: &str, window: &Window, cap: f32) -> gpui::Pixels {
    let mut style = window.text_style();
    style.font_weight = theme::SEMIBOLD;
    let run = style.to_run(title.len());
    let width = window.text_system().shape_line(title.to_owned().into(), theme::sized(16.), &[run]).width;
    width.ceil().min(px(cap))
}

/// What follows the title. A person's presence is not a place, so it does not reveal the sidebar.
enum Beside {
    Activity(String),
    Person(crate::gui::people::PeerLine),
    Path(String),
}

/// The presence word in its color, then what they say. Online is green.
fn person_line(line: &crate::gui::people::PeerLine) -> Div {
    let mut row = div().flex_1().min_w_0().flex().items_center().gap(px(6.)).overflow_hidden();
    if !line.word.is_empty() {
        row = row.child(div().min_w_0().truncate().type_ui().text_color(line.tone).child(line.word));
    }
    if !line.extra.is_empty() {
        if !line.word.is_empty() {
            row = row.child(div().flex_none().type_ui().text_color(theme::text_faint()).child("·"));
        }
        row = row.child(div().min_w_0().truncate().type_ui().text_color(theme::text_faint()).child(line.extra.clone()));
    }
    row
}

/// The other person's role on this server. One chip, the most senior. No email.
fn role_chip(name: &str, color: Option<u32>) -> Div {
    let tone = color.map(theme::role_color).unwrap_or(theme::text_soft());
    div()
        .flex_none()
        .self_center()
        .h(px(20.))
        .px(px(7.))
        .rounded_full()
        .bg(theme::hover())
        .flex()
        .items_center()
        .gap(px(4.))
        .child(div().size(px(6.)).flex_none().rounded_full().bg(tone))
        .child(div().min_w_0().max_w(px(84.)).truncate().type_micro().font_weight(theme::MEDIUM).text_color(theme::text()).child(name.to_owned()))
}

/// One path glyph everywhere: `›`.
// CONTRACT: `Conversation::subtitle` joins with " > "; the protocol could say "›" itself.
pub(super) fn path_line(subtitle: &str) -> String {
    subtitle.replace(" > ", " › ")
}

/// What the network says the other side is, as a tag after the title. A verified account is
/// a badge-check; the rest are words, red where they warn. A bot with no mark of its own gets the
/// row's word for one (`chat::name_mark`).
fn mark(c: &Conversation) -> Option<Div> {
    let (words, color) = match c.mark {
        Some(Mark::Verified) => return Some(div().flex_none().self_center().child(glyph("icons/badge-check.svg", 14., theme::accent_text()))),
        Some(Mark::Scam) => ("Scam", theme::danger()),
        Some(Mark::Fake) => ("Fake", theme::danger()),
        Some(Mark::Guest) => ("Guest", theme::text_soft()),
        Some(Mark::External) => ("External", theme::text_soft()),
        Some(Mark::Deactivated) => ("Deactivated", theme::text_faint()),
        None if c.bot => (if c.key.network == Network::Telegram { "bot" } else { "App" }, theme::text_soft()),
        None => return None,
    };
    Some(
        div()
            .flex_none()
            .self_center()
            .h(px(20.))
            .px(px(6.))
            .rounded(px(theme::R_CHIP))
            .border_1()
            .border_color(color)
            .flex()
            .items_center()
            .type_micro()
            .font_weight(theme::MEDIUM)
            .text_color(color)
            .child(words),
    )
}

#[cfg(test)]
mod tests {
    use super::path_line;

    #[test]
    fn the_path_reads_with_one_glyph() {
        assert_eq!(path_line("Moss & Lichen > Trail · Maps and routes"), "Moss & Lichen › Trail · Maps and routes");
        assert_eq!(path_line("a>b"), "a>b");
    }
}
