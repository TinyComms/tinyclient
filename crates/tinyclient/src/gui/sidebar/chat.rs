//! The two-line conversation row (`design/polish/01-system.md` §2, `04-unified.md` §2): the one
//! row the Inbox, the Discord DMs and the Telegram and Teams chat lists share. Avatar 32, line
//! one the name, its context and the time, line two what happened last and one badge.

use super::row_id;
use crate::gui::components::{glyph, list_row, pair_or, row_badge, row_tone, Avatar, Shape};
use crate::gui::layout::Menu;
use crate::gui::people::presence;
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use crate::gui::time;
use crate::view::now;
use gpui::{div, prelude::*, px, Context, Div, MouseButton, Rgba, SharedString, Stateful};
use protocol::{Badge, Conversation, ConversationKind, Key, Mark, Network, Special};
use std::rc::Rc;

/// How a list draws its two-line rows.
#[derive(Clone, Copy, Default)]
pub struct Look {
    /// The list mixes networks and two or more are signed in: a monochrome mark leads the name.
    pub mark: bool,
    /// Say the Space after the name (` · Harbor Labs`).
    pub context: bool,
    /// The time column is drawn at rest (Discord's DM list shows it only on hover).
    pub time: bool,
    /// The network's read ticks on my last message (Telegram, Teams).
    pub ticks: bool,
    /// A DM's line two says what the person is doing before what they said (Discord).
    pub status: bool,
}

/// The time on a row, in the one date table (`polish/01-system.md` §2): the clock today, then
/// "Yesterday", the short weekday ("Mon") this week, "Oct 2" this year, else the short date.
pub fn row_time(now: i64, at: i64) -> String {
    if at <= 0 {
        return String::new();
    }
    time::stamp(now, at)
}

/// Sidebar text occupies one visual line, even when a message or draft has paragraphs.
pub(super) fn one_line(text: &str) -> std::borrow::Cow<'_, str> {
    if text.chars().any(|c| c.is_whitespace() && c != ' ') {
        std::borrow::Cow::Owned(text.split_whitespace().collect::<Vec<_>>().join(" "))
    } else {
        std::borrow::Cow::Borrowed(text)
    }
}

/// A preview that starts by naming me says it to me, so the name is dropped (C12).
pub fn strip_me<'a>(preview: &'a str, me: &str) -> &'a str {
    if me.is_empty() {
        return preview;
    }
    let rest = preview.strip_prefix('@').and_then(|rest| rest.strip_prefix(me));
    match rest {
        Some(rest) if rest.is_empty() || rest.starts_with([' ', ',', ':']) => rest.trim_start_matches([',', ':']).trim_start(),
        _ => preview,
    }
}

/// The words a stored preview uses for an attachment, and the glyph that goes before them.
pub fn attachment_glyph(preview: &str) -> Option<&'static str> {
    let words = preview.trim();
    let counted = |one: &str, many: &str| {
        words == one || words.split_once(' ').is_some_and(|(n, rest)| rest == many && n.parse::<u32>().is_ok())
    };
    if counted("Photo", "photos") || counted("GIF", "GIFs") {
        Some("icons/image.svg")
    } else if counted("Video", "videos") {
        Some("icons/video.svg")
    } else if words == "Voice message" {
        Some("icons/mic.svg")
    } else if words == "Sticker" {
        Some("icons/smile.svg")
    } else if counted("File", "files") {
        Some("icons/paperclip.svg")
    } else {
        None
    }
}

/// `Sender: ` before a group's or channel's preview, by first name; `You: ` for mine.
pub fn sender(c: &Conversation) -> Option<String> {
    if c.preview_mine {
        return Some("You".to_owned());
    }
    let group = !matches!(c.kind, ConversationKind::Direct) && !matches!(c.kind, ConversationKind::Announcement);
    let from = c.preview_from.split_whitespace().next().unwrap_or_default();
    (group && !from.is_empty()).then(|| from.to_owned())
}

/// The tag after a name: a bot or app, a guest, another organization, a warning.
pub fn tag(words: &'static str, danger: bool) -> Div {
    div()
        .flex_none()
        .h(px(16.))
        .px(px(4.))
        .rounded(px(theme::R_CHIP))
        .bg(if danger { theme::danger_soft() } else { theme::quiet_fill() })
        .flex()
        .items_center()
        .text_size(px(11.))
        .line_height(px(14.))
        .font_weight(if danger { theme::SEMIBOLD } else { theme::MEDIUM })
        .text_color(if danger { theme::danger() } else { theme::text_soft() })
        .child(words)
}

/// A network's monochrome mark in a mixed list (C5).
// CONTRACT: the `networks/*-mono.svg` silhouettes are not among the assets yet. Tinting the
// colour logo flattens Telegram's plane into a plain disc, so its plane glyph stands in.
pub fn mark(network: Network, size: f32, color: Rgba) -> gpui::AnyElement {
    match network {
        Network::Telegram => glyph("icons/send-horizontal.svg", size, color).into_any_element(),
        other => crate::gui::components::network_mark(other, size, color).into_any_element(),
    }
}

/// What follows a name, at most one thing (C4).
pub fn name_mark(c: &Conversation) -> Option<Div> {
    match c.mark {
        Some(Mark::Scam) => Some(tag("Scam", true)),
        Some(Mark::Fake) => Some(tag("Fake", true)),
        // Lucide's `badge-check`, so it never reads as the sent tick.
        Some(Mark::Verified) => Some(div().flex_none().child(glyph("icons/badge-check.svg", 14., theme::text_soft()))),
        Some(Mark::Guest) => Some(tag("Guest", false)),
        Some(Mark::External) => Some(tag("External", false)),
        Some(Mark::Deactivated) => Some(tag("Deactivated", false)),
        None if c.bot => Some(tag(if c.key.network == Network::Telegram { "bot" } else { "App" }, false)),
        None if c.kind == ConversationKind::Forum && c.key.network == Network::Telegram => {
            Some(div().flex_none().child(glyph("icons/messages-square.svg", 14., theme::text_faint())))
        }
        None => None,
    }
}

impl Shell {
    /// Whether rows in a list that mixes networks carry the network's mark (C5).
    pub(super) fn marks_shown(&self) -> bool {
        self.snapshot.as_ref().is_some_and(|snapshot| snapshot.signed_in().len() >= 2)
    }

    /// The names I go by: this account's me, and the people the other accounts are (a
    /// workspace account is named for the workspace, so its own name may be unknown).
    fn my_names(&self, c: &Conversation) -> Vec<String> {
        let Some(snapshot) = self.snapshot.as_ref() else {
            return Vec::new();
        };
        let hub = self.people_state.hub();
        let mut names: Vec<String> = Vec::new();
        let ordered = snapshot.accounts.iter().filter(|a| a.key == c.key.account()).chain(snapshot.accounts.iter().filter(|a| a.key != c.key.account()));
        for account in ordered {
            let me = Key::new(&account.key, account.me.clone());
            let name = hub.person(&me).map(|person| person.name).or_else(|| matches!(account.key.network, Network::Discord | Network::Telegram).then(|| account.name.clone()));
            if let Some(name) = name.filter(|name| !name.is_empty() && !names.contains(name)) {
                names.push(name);
            }
        }
        names
    }

    /// The picture of a conversation: a person round with their presence, a group as two of its
    /// people, a channel as its Space's icon, a Telegram place as a rounded square.
    pub(in crate::gui) fn conversation_avatar(&self, c: &Conversation, size: f32, surface: Rgba, cx: &mut Context<Self>) -> Div {
        let hub = self.people_state.hub();
        let picture = hub.avatar(&c.key.account(), c.avatar.as_ref(), cx).and_then(|file| file.path).map(std::path::PathBuf::from);
        match c.special {
            // My own notes: Telegram's Saved Messages is its bookmark; Teams' Notes and Slack's
            // DM to myself are my picture, the bookmark until it lands.
            Special::Saved => {
                let mine = (c.key.network != Network::Telegram).then(|| self.own_avatar(&c.key.account())).flatten();
                return match mine {
                    Some(path) => Avatar::new(&c.key, &c.title, size).image_at(Some(path)).build(),
                    None => Avatar::channel("icons/bookmark.svg", size).shape(Shape::Person).build(),
                };
            }
            Special::Service if picture.is_none() => {
                return div().size(px(size)).flex_none().flex().items_center().justify_center().child(crate::gui::components::network_icon(c.key.network, size));
            }
            _ => {}
        }
        match c.kind {
            ConversationKind::Direct => {
                let avatar = Avatar::new(&c.key, &c.title, size).image_at(picture).shape(if c.bot && c.key.network != Network::Telegram { Shape::Place } else { Shape::Person }).build();
                if c.bot || c.special != Special::None {
                    return avatar;
                }
                let shape = c.peer.as_ref().and_then(|peer| {
                    let person = Key { id: peer.clone(), ..c.key.clone() };
                    let presence = self.presence_of(&person);
                    if self.in_call(&person) {
                        Some(presence::Shape::InCall)
                    } else if c.key.network == Network::Telegram && presence != protocol::Presence::Online {
                        // Telegram's "last seen" is words, never a dot (`03` §2).
                        None
                    } else {
                        presence::Shape::of(presence)
                    }
                });
                avatar.children(shape.and_then(|shape| presence::dot(shape, size, &presence::Ring::on(surface))))
            }
            // One look per group at every size: its picture, else two of its people when its
            // title names them, else its two letters (`components::pair`).
            ConversationKind::Group if c.meeting && picture.is_none() => Avatar::channel("icons/calendar.svg", size).build(),
            ConversationKind::Group => {
                let shape = if c.key.network == Network::Telegram { Shape::Place } else { Shape::Person };
                match picture {
                    Some(path) => Avatar::new(&c.key, &c.title, size).shape(shape).image_at(Some(path)).build(),
                    None => pair_or(&c.key, &c.title, &c.title, size, surface, shape),
                }
            }
            kind if picture.is_none() => match c.space.as_ref().and_then(|id| self.spaces.iter().find(|s| s.key.id == *id && s.key.account == c.key.account)) {
                // A channel is drawn as where it is: its Space's icon.
                Some(space) => {
                    let icon = self.media_path(&space.key, space.icon.as_ref());
                    Avatar::new(&space.key, &space.name, size).shape(Shape::Place).image_at(icon).build()
                }
                None if c.key.network == Network::Telegram => Avatar::new(&c.key, &c.title, size).shape(Shape::Place).build(),
                None => Avatar::channel(super::kind_glyph(kind, c.private), size).build(),
            },
            _ => Avatar::new(&c.key, &c.title, size).shape(Shape::Place).image_at(picture).build(),
        }
    }

    /// Line two, first match wins: not sent, typing, a draft when nothing is unread, then the
    /// preview with its sender and my ticks.
    pub(super) fn line_two(&self, c: &Conversation, look: Look, color: Rgba) -> Div {
        let line = div().flex_1().min_w_0().h(px(16. + theme::text_step() as f32)).overflow_hidden().flex().items_center().gap(px(4.)).type_small();
        // A4 off: no words of any message, for screen sharing. Typing and Not sent still say so.
        let previews = crate::gui::app::prefs::choices().previews;
        if self.send_failed_in(&c.key) {
            return line
                .child(glyph("icons/circle-alert.svg", 12., theme::danger()))
                .child(div().flex_none().text_color(theme::danger()).child("Not sent"))
                .when(previews, |line| line.child(div().min_w_0().truncate().text_color(theme::text_faint()).child(format!("· {}", one_line(&c.preview)))));
        }
        let typing = self.slots.typing.and_then(|typing| typing(self, &c.key));
        if let Some(words) = typing {
            return line.child(div().min_w_0().truncate().text_color(theme::accent_text()).child(one_line(&words).into_owned()));
        }
        if !previews {
            return line;
        }
        // A muted row is one grey throughout (C7): `color` is already its faint.
        let quiet = |lit: Rgba| if c.muted { color } else { lit };
        if let Some(doing) = self.doing(c, c.muted.then_some(color)).filter(|_| look.status) {
            return doing;
        }
        let draft = self.draft_of(c);
        if !draft.is_empty() && c.badge() == Badge::None {
            return line
                .child(glyph("icons/pencil.svg", 12., quiet(theme::text_faint())))
                .child(div().flex_none().text_color(quiet(theme::text())).font_weight(if c.muted { theme::REGULAR } else { theme::MEDIUM }).child("Draft:"))
                .child(div().min_w_0().truncate().text_color(quiet(theme::text_soft())).child(one_line(draft).into_owned()));
        }
        let names = self.my_names(c);
        let preview = names.iter().map(|me| strip_me(&c.preview, me)).find(|stripped| stripped.len() != c.preview.len()).unwrap_or(&c.preview);
        let ticks = (look.ticks && c.preview_mine && matches!(c.kind, ConversationKind::Direct | ConversationKind::Group) && c.special == Special::None)
            .then(|| if c.seen { glyph("icons/check-check.svg", 14., quiet(theme::text_soft())) } else { glyph("icons/check.svg", 14., quiet(theme::text_faint())) });
        let who = sender(c).map(|name| div().flex_none().child(format!("{name}:")));
        let attached = attachment_glyph(preview).map(|icon| glyph(icon, 12., quiet(theme::text_faint())));
        line.text_color(color)
            .children(ticks)
            .children(who)
            .children(attached)
            .child(div().min_w_0().truncate().child(one_line(preview).into_owned()))
    }

    /// What the other side of a DM is doing, for a home that shows it over the last message:
    /// in a call, then an activity, then their status. `muted` is the one color of a muted row.
    fn doing(&self, c: &Conversation, muted: Option<Rgba>) -> Option<Div> {
        let peer = Key { id: c.peer.clone()?, ..c.key.clone() };
        let line = |color: Rgba| div().flex_1().min_w_0().h(px(16. + theme::text_step() as f32)).overflow_hidden().flex().items_center().gap(px(4.)).type_small().text_color(muted.unwrap_or(color));
        let glyph = |icon: &'static str, size: f32, color: Rgba| glyph(icon, size, muted.unwrap_or(color));
        let person = self.people_state.hub().person(&peer);
        let voice = person.as_ref().and_then(|p| p.activity.as_ref()).filter(|a| a.kind == protocol::ActivityKind::InVoice && !a.name.is_empty());
        if let Some(voice) = voice {
            return Some(line(theme::accent_text()).child(glyph("icons/volume-2.svg", 12., theme::accent_text())).child(div().min_w_0().truncate().child(format!("In {}", one_line(&voice.name)))));
        }
        if self.in_call(&peer) {
            return Some(line(theme::accent_text()).child(glyph("icons/phone.svg", 12., theme::accent_text())).child("In a call"));
        }
        let person = person?;
        if let Some(activity) = &person.activity {
            use protocol::ActivityKind::*;
            let verb = match activity.kind {
                Playing => "Playing",
                Listening => "Listening to",
                Watching => "Watching",
                Streaming => "Streaming",
                Competing => "Competing in",
                InVoice | InCall => "In",
                Huddle => "In a huddle",
                InMeeting => "In a meeting",
                Presenting => "Presenting",
            };
            let words = if activity.name.is_empty() { verb.to_owned() } else { format!("{verb} {}", activity.name) };
            return Some(line(theme::text_soft()).child(div().min_w_0().truncate().child(one_line(&words).into_owned())));
        }
        let status = self.people_state.hub().status_of(&peer);
        let status = if status.is_empty() { person.status.clone() } else { status };
        (!status.is_empty() || !person.emoji.is_empty()).then(|| {
            line(theme::text_soft()).child(div().min_w_0().truncate().child(one_line(&presence::status_words(&person.emoji, &status)).into_owned()))
        })
    }

    /// One two-line row. `context` is the faint words after the name; `trailing` replaces the
    /// badge (a Join button, a close on hover).
    pub(super) fn chat_row(&self, id: &'static str, c: &Rc<Conversation>, look: Look, focused: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        let selected = self.open.as_ref() == Some(&c.key);
        let tone = row_tone(c, selected, false);
        let surface = theme::row_surface(selected);
        let several = self.snapshot.as_ref().is_some_and(|snapshot| snapshot.accounts_of(c.key.network) > 1);
        let context = (look.context && (!c.kind.is_direct() || several) && !c.space_title.is_empty()).then(|| c.space_title.clone());
        let stamp = if look.time { row_time(now(), c.updated) } else { String::new() };
        let faint = if selected { theme::text_soft() } else { theme::text_faint() };
        let title = match c.kind {
            ConversationKind::Channel | ConversationKind::Announcement | ConversationKind::Voice | ConversationKind::Forum
                if c.key.network != Network::Telegram && c.key.network != Network::Teams =>
            {
                format!("#{}", c.title.trim_start_matches('#'))
            }
            _ => c.title.clone(),
        };
        // The Space gives way first and goes when fewer than six letters of it would show (C11).
        let room = self.sidebar_state.width.get() - 2. * theme::INSET - 16. - 32. - 10. - 12.
            - if look.mark { 20. } else { 0. }
            - stamp.len() as f32 * 6.5
            - title.chars().count().min(24) as f32 * 7.6;
        // Six letters and the ellipsis, or nothing.
        let context = context.filter(|_| room > 6. * 7. + 14.);
        let line_one = div()
            .h(px(18. + theme::text_step() as f32))
            .flex_none()
            .overflow_hidden()
            .flex()
            .items_center()
            .gap(px(6.))
            .children(look.mark.then(|| mark(c.key.network, 14., faint)))
            .child(
                div()
                    .min_w_0()
                    .max_w(px(180.))
                    .truncate()
                    .type_ui()
                    .font_weight(tone.weight)
                    .text_color(tone.title)
                    .child(one_line(&title).into_owned()),
            )
            .children(name_mark(c))
            .map(|line| match context {
                Some(context) => line.child(div().flex_1().min_w_0().truncate().type_ui().text_color(faint).child(one_line(&context).into_owned())),
                None => line.child(div().flex_1()),
            })
            .when(!stamp.is_empty(), |line| line.child(div().flex_none().type_micro().text_color(faint).child(stamp)));
        let badge = row_badge(c);
        let content = div()
            .gap(px(10.))
            .child(self.conversation_avatar(c, theme::row_avatar(), surface, cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(line_one)
                    .child(div().h(px(16. + theme::text_step() as f32)).flex_none().overflow_hidden().flex().items_center().gap(px(6.)).child(self.line_two(c, look, tone.preview)).children(badge)),
            );
        let (picked, menu_row) = (c.clone(), c.clone());
        list_row(row_id(id, &c.key), theme::row_height(theme::ROW_TWO), selected, focused, content)
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| this.pick_row(&picked, cx)))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                    this.open_menu(Menu::Row(menu_row.clone(), event.position), cx);
                }),
            )
    }

    /// A one-line action row (28): Friends, Message requests, Archive, Show n more, folds.
    pub(super) fn action_row(&self, id: impl Into<gpui::ElementId>, icon: Option<&'static str>, words: SharedString, lit: bool, trailing: Option<Div>, focused: bool) -> Stateful<Div> {
        let content = div()
            .gap(px(8.))
            .children(icon.map(|icon| div().size(px(20.)).flex_none().flex().items_center().justify_center().child(glyph(icon, 16., if lit { theme::text() } else { theme::text_soft() }))))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .type_ui()
                    .font_weight(if lit { theme::SEMIBOLD } else { theme::REGULAR })
                    .text_color(if lit { theme::text() } else { theme::text_soft() })
                    .child(words),
            )
            .children(trailing);
        list_row(id, theme::row_height(theme::ROW_ONE), false, focused, content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiline_previews_and_drafts_stay_on_one_sidebar_line() {
        for (text, expected) in [
            ("@everyone\n\nWe will be performing network upgrade…", "@everyone We will be performing network upgrade…"),
            ("UI Updates:\r\n+ RFQ builder now…", "UI Updates: + RFQ builder now…"),
            ("first\u{2028}second\u{2029}third", "first second third"),
            ("draft\titem\nnext", "draft item next"),
        ] {
            assert_eq!(one_line(text), expected);
        }
        assert!(matches!(one_line("You: Nice."), std::borrow::Cow::Borrowed(_)));
        assert_eq!(one_line("two  spaces"), "two  spaces");
    }

    #[test]
    fn a_preview_drops_my_name_and_names_its_attachment() {
        assert_eq!(strip_me("@Robin Vale need a yes or no", "Robin Vale"), "need a yes or no");
        assert_eq!(strip_me("@Robin Vale, are you in?", "Robin Vale"), "are you in?");
        assert_eq!(strip_me("@Robin Valencia hi", "Robin Vale"), "@Robin Valencia hi");
        assert_eq!(strip_me("hi @Robin Vale", "Robin Vale"), "hi @Robin Vale");
        assert_eq!(attachment_glyph("2 photos"), Some("icons/image.svg"));
        assert_eq!(attachment_glyph("Photo"), Some("icons/image.svg"));
        assert_eq!(attachment_glyph("Voice message"), Some("icons/mic.svg"));
        assert_eq!(attachment_glyph("Photos of the trail"), None);
    }

    #[test]
    fn a_group_says_who_spoke_by_first_name_and_mine_says_you() {
        let mut c = crate::gui::sidebar::tests::row("g", ConversationKind::Group);
        c.preview_from = "Dita Novak".into();
        assert_eq!(sender(&c).as_deref(), Some("Dita"));
        c.kind = ConversationKind::Direct;
        assert_eq!(sender(&c), None);
        c.preview_mine = true;
        assert_eq!(sender(&c).as_deref(), Some("You"));
    }
}
