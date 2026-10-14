//! What floats over the People views: a person's profile card, the menu of a row's More
//! button, and the question asked before a friend is removed or someone is blocked. One at a
//! time; Escape or a click outside closes it.

use super::list::bot_tag;
use super::presence::{self, Ring, Shape};
use super::{can_call, in_call, only_draws, person_avatar, shown_presence, Hub, Scene};
use crate::gui::components::{
    button, button_with_icon, fade_in, glyph, icon_button, menu, menu_item, menu_item_danger, menu_separator,
    modal_body, modal_buttons, network_icon, popover, tip, ButtonKind,
};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use gpui::{
    anchored, deferred, div, point, prelude::*, px, ClipboardItem, Context, Div, FocusHandle, KeyDownEvent,
    Pixels, Point, WeakEntity, Window,
};
use protocol::{Key, Network, Person, Profile, RelateAction, Relation, Request, Response};

const CARD_WIDTH: f32 = 320.;
const CARD_HEIGHT: f32 = 480.;
/// Shared Spaces named on the card before "+n".
const SPACES_SHOWN: usize = 5;
/// A bio longer than this starts folded to four lines.
const BIO_FOLDED: usize = 180;

#[derive(Clone, PartialEq)]
pub enum Float {
    Card(Key),
    /// A row's More menu.
    Menu(Key),
    /// The card's own More menu, anchored where the card was.
    More(Key),
    /// Asked once before a friend is removed or someone is blocked.
    Confirm(Key, RelateAction),
}

impl Float {
    fn person(&self) -> &Key {
        match self {
            Float::Card(person) | Float::Menu(person) | Float::More(person) | Float::Confirm(person, _) => person,
        }
    }
}

pub struct Floating {
    shell: WeakEntity<Shell>,
    hub: Hub,
    scene: Scene,
    open: Option<(Float, Point<Pixels>)>,
    /// Counts openings, so each one fades in.
    opened: usize,
    focus: Option<FocusHandle>,
    /// What the network said of the person whose card is up; the card's body.
    profile: Option<Profile>,
    profile_asked: u64,
    bio_unfolded: bool,
}

/// Their wall clock now and how far it is from ours: "12:48 PM · 3 h behind you". Offsets are
/// seconds east of UTC.
pub fn local_time(now: i64, theirs: i32, ours: i32) -> String {
    let seconds = (now + i64::from(theirs)).rem_euclid(86_400);
    let (hour, minute) = (seconds / 3600, seconds % 3600 / 60);
    let (hour, half) = match hour {
        0 => (12, "AM"),
        1..=11 => (hour, "AM"),
        12 => (12, "PM"),
        _ => (hour - 12, "PM"),
    };
    let apart = (i64::from(theirs) - i64::from(ours)) / 60;
    let far = match (apart.abs() / 60, apart.abs() % 60) {
        (hours, 0) => format!("{hours} h"),
        (0, minutes) => format!("{minutes} min"),
        (hours, minutes) => format!("{hours} h {minutes} min"),
    };
    let where_ = match apart.signum() {
        0 => "same time as you".to_owned(),
        1 => format!("{far} ahead of you"),
        _ => format!("{far} behind you"),
    };
    format!("{hour}:{minute:02} {half} · {where_}")
}

/// This machine's offset from UTC now, in seconds.
pub(super) fn own_offset(now: i64) -> i32 {
    let seconds = now as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `localtime_r` / `localtime_s` write a full `tm` into the buffer we own.
    unsafe {
        #[cfg(unix)]
        libc::localtime_r(&seconds, &mut tm);
        #[cfg(windows)]
        libc::localtime_s(&mut tm, &seconds);
    }
    #[cfg(unix)]
    {
        return tm.tm_gmtoff as i32;
    }
    #[cfg(windows)]
    {
        return utc_offset_seconds(seconds);
    }
}

#[cfg(windows)]
fn utc_offset_seconds(seconds: libc::time_t) -> i32 {
    let mut local: libc::tm = unsafe { std::mem::zeroed() };
    let mut utc: libc::tm = unsafe { std::mem::zeroed() };
    // The libc crate does not expose these on Windows. They are in the CRT.
    extern "C" {
        fn mktime(tm: *mut libc::tm) -> libc::time_t;
        fn _mkgmtime(tm: *mut libc::tm) -> libc::time_t;
    }
    // SAFETY: `gmtime_s`, `localtime_s`, `mktime`, and `_mkgmtime` fill or read the buffers we own.
    unsafe {
        libc::localtime_s(&mut local, &seconds);
        libc::gmtime_s(&mut utc, &seconds);
        local.tm_isdst = -1;
        let local_sec = mktime(&mut local);
        let utc_sec = _mkgmtime(&mut utc);
        (local_sec - utc_sec) as i32
    }
}

/// The shared Spaces a card names, and how many more there are.
pub fn spaces_shown(spaces: &[String]) -> (&[String], usize) {
    let shown = &spaces[..spaces.len().min(SPACES_SHOWN)];
    (shown, spaces.len() - shown.len())
}

/// Whether the More menus offer Remove friend and Block for this person.
// CONTRACT: nothing says which networks can block or unfriend. Friends exist only where a
// network reports one; Block is offered on Discord, the one network with a Blocked list.
pub fn offers(person: &Person) -> (bool, bool) {
    let remove = person.relation == Relation::Friend;
    let block = person.key.network == Network::Discord && person.relation != Relation::Blocked;
    (remove, block)
}

pub(super) fn relation_word(relation: Relation) -> Option<&'static str> {
    match relation {
        Relation::Friend => Some("Friend"),
        Relation::Incoming => Some("Wants to be friends"),
        Relation::Outgoing => Some("Friend request sent"),
        Relation::Blocked => Some("Blocked"),
        Relation::Contact | Relation::None => None,
    }
}

impl Floating {
    pub fn new(shell: WeakEntity<Shell>, hub: Hub) -> Floating {
        Floating {
            shell,
            hub,
            scene: Scene::default(),
            open: None,
            opened: 0,
            focus: None,
            profile: None,
            profile_asked: 0,
            bio_unfolded: false,
        }
    }

    pub fn see(&mut self, scene: Scene, _: &mut Context<Self>) {
        self.scene = scene;
    }

    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    pub fn open(&mut self, float: Float, anchor: Point<Pixels>, cx: &mut Context<Self>) {
        let person = float.person().clone();
        // Their presence is drawn, so it is watched for as long as this is up.
        self.hub.watch("profile", [person.clone()], cx);
        if matches!(float, Float::Card(_)) {
            self.ask_profile(person, cx);
        }
        self.open = Some((float, anchor));
        self.opened += 1;
        cx.notify();
    }

    /// The card's body, asked of the network each time the card opens. The head does not wait.
    fn ask_profile(&mut self, person: Key, cx: &mut Context<Self>) {
        self.profile_asked += 1;
        self.bio_unfolded = false;
        if self.profile.as_ref().is_some_and(|profile| profile.person.key != person) {
            self.profile = None;
        }
        if only_draws() {
            return;
        }
        let (asked, this) = (self.profile_asked, cx.weak_entity());
        super::ask_later(cx, Request::Profile { person, space: None }, move |answer, cx| {
            let Ok(Response::Profile { profile }) = answer else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.profile_asked == asked {
                    this.hub.learn(std::slice::from_ref(&profile.person));
                    this.profile = Some(*profile);
                    cx.notify();
                }
            });
        });
    }

    /// Swaps what floats for something else in the same place.
    fn turn_to(&mut self, float: Float, cx: &mut Context<Self>) {
        let anchor = self.open.as_ref().map(|(_, anchor)| *anchor).unwrap_or_default();
        self.open(float, anchor, cx);
    }

    fn relate(&mut self, person: &Person, action: RelateAction, cx: &mut Context<Self>) {
        self.close(cx);
        let _ = self.shell.update(cx, |shell, cx| shell.relate(person, action, cx));
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        if self.open.take().is_some() {
            self.hub.watch("profile", [], cx);
            let _ = self.shell.update(cx, |_, cx| cx.notify());
            cx.notify();
        }
    }

    fn message(&mut self, person: &Person, call: bool, cx: &mut Context<Self>) {
        self.close(cx);
        let _ = self.shell.update(cx, |shell, cx| {
            if call {
                shell.call_person(person, cx);
            } else {
                shell.open_person(person, cx);
            }
        });
    }

    /// What the network said beyond the row: their words, the Spaces we share, their clock.
    fn body(&self, profile: &Profile, cx: &mut Context<Self>) -> Option<Div> {
        let label = |words: &'static str| {
            div().h(px(20.)).flex().items_center().type_micro().text_color(theme::text_faint()).child(words)
        };
        let mut body = div().flex().flex_col();
        let mut any = false;
        if !profile.bio.is_empty() {
            any = true;
            let long = profile.bio.chars().count() > BIO_FOLDED || profile.bio.lines().count() > 4;
            let folded = long && !self.bio_unfolded;
            body = body
                .child(label("About"))
                .child(
                    div()
                        .type_ui()
                        .text_color(theme::text_soft())
                        .when(folded, |bio| bio.line_clamp(4))
                        .child(profile.bio.clone()),
                )
                .when(long, |body| {
                    body.child(
                        div()
                            .id("card-bio-more")
                            .mt(px(2.))
                            .type_small()
                            .text_color(theme::accent_text())
                            .cursor_pointer()
                            .child(if folded { "more" } else { "less" })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.bio_unfolded = !this.bio_unfolded;
                                cx.notify();
                            })),
                    )
                })
                .child(div().h(px(8.)));
        }
        if !profile.mutual_spaces.is_empty() {
            any = true;
            // CONTRACT: `Profile::mutual_spaces` is titles only, so they are named in words where
            // the design shows icons.
            let (shown, more) = spaces_shown(&profile.mutual_spaces);
            let chip = |words: String| {
                div()
                    .h(px(20.))
                    .max_w(px(180.))
                    .px(px(6.))
                    .rounded(px(theme::R_CHIP))
                    .bg(theme::hover())
                    .flex()
                    .items_center()
                    .type_small()
                    .text_color(theme::text_soft())
                    .child(div().truncate().child(words))
            };
            let words = if profile.person.key.network == Network::Discord { "Mutual servers" } else { "In common" };
            body = body.child(label(words)).child(
                div()
                    .pb(px(8.))
                    .flex()
                    .flex_wrap()
                    .gap(px(4.))
                    .children(shown.iter().cloned().map(chip))
                    .children((more > 0).then(|| chip(format!("+{more}")))),
            );
        }
        if let Some(offset) = profile.utc_offset {
            any = true;
            let now = crate::view::now();
            body = body.child(
                div()
                    .h(px(theme::ROW_ONE))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .type_ui()
                    .child(div().flex_none().text_color(theme::text_soft()).child("Local time"))
                    .child(div().min_w_0().truncate().child(local_time(now, offset, own_offset(now)))),
            );
        }
        // CONTRACT: a `Profile` has no title, email, phone, roles, pronouns or "friends since",
        // so the card's body is the bio, the clock and the shared Spaces.
        any.then_some(body)
    }

    fn card(&self, person: &Person, cx: &mut Context<Self>) -> Div {
        let network = person.key.network;
        let known = shown_presence(&self.hub, person);
        let in_call = in_call(&self.hub, &self.scene, &person.key);
        let status = self.hub.status_of(&person.key);
        let status = if status.is_empty() { person.status.clone() } else { status };
        let word = if in_call { "In a call" } else { presence::word(known, network) };
        let shape = if in_call { Some(Shape::InCall) } else { Shape::of(known) };
        let ring = Ring::on(theme::overlay());
        let said = match (word.is_empty(), status.is_empty()) {
            (true, true) => "No status shared".to_owned(),
            (true, false) => status,
            (false, true) => word.to_owned(),
            (false, false) => format!("{word} · {status}"),
        };
        let handle = person.handle.trim_start_matches('@');
        let (message, call) = (person.clone(), person.clone());
        let link = (!person.link.is_empty()).then(|| person.link.clone());
        let (more, docked) = (person.key.clone(), person.key.clone());
        let body = self.profile.as_ref().filter(|profile| profile.person.key == person.key);
        let body = body.and_then(|profile| self.body(profile, cx));
        popover()
            .w(px(CARD_WIDTH))
            .max_h(px(CARD_HEIGHT))
            .p(px(16.))
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_start()
                    .justify_between()
                    .child(person_avatar(&self.hub, person, 80., &ring, in_call, cx))
                    .child(
                        div()
                            .h(px(24.))
                            .px(px(8.))
                            .rounded(px(theme::R_CONTROL))
                            .bg(theme::hover())
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .type_small()
                            .text_color(theme::text_soft())
                            .child(network_icon(network, 14.))
                            .child(network.label()),
                    ),
            )
            .child(
                div()
                    .mt(px(12.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(div().min_w_0().truncate().type_title().child(person.name.clone()))
                    .when(person.bot, |name| name.child(bot_tag())),
            )
            .when(!handle.is_empty(), |card| {
                card.child(div().truncate().type_ui().text_color(theme::text_soft()).child(format!("@{handle}")))
            })
            .child(
                div()
                    .mt(px(8.))
                    .flex()
                    .items_start()
                    .gap(px(6.))
                    .type_ui()
                    .text_color(theme::text_soft())
                    .children(shape.map(|shape| div().pt(px(4.)).child(presence::inline(shape, 10., theme::overlay()))))
                    .child(div().min_w_0().line_clamp(2).child(said)),
            )
            .children(relation_word(person.relation).map(|word| {
                div().mt(px(4.)).type_small().text_color(theme::text_soft()).child(word)
            }))
            .child(
                div()
                    .mt(px(16.))
                    .flex()
                    .gap(px(8.))
                    .child(
                        button_with_icon("card-message", ButtonKind::Primary, "icons/message-circle.svg", "Message")
                            .flex_1()
                            .on_click(cx.listener(move |this, _, _, cx| this.message(&message, false, cx))),
                    )
                    .when(can_call(network) && !person.bot, |actions| {
                        actions.child(
                            button_with_icon("card-call", ButtonKind::Secondary, "icons/phone.svg", "Call")
                                .flex_1()
                                .on_click(cx.listener(move |this, _, _, cx| this.message(&call, true, cx))),
                        )
                    })
                    .child(
                        icon_button("card-dock", "icons/info.svg", false)
                            .tooltip(tip("Keep in the side pane", None))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.close(cx);
                                let _ = this.shell.update(cx, |shell, cx| shell.show_info(docked.clone(), cx));
                            })),
                    )
                    .child(
                        icon_button("card-more", "icons/ellipsis.svg", false)
                            .on_click(cx.listener(move |this, _, _, cx| this.turn_to(Float::More(more.clone()), cx))),
                    ),
            )
            // The body only ever extends the card downward, and scrolls once the card is full.
            .children(body.map(|body| {
                div()
                    .id("card-body")
                    .mt(px(12.))
                    .pt(px(8.))
                    .border_t_1()
                    .border_color(theme::hairline())
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(body)
            }))
            .children(link.map(|url| {
                div().mt(px(12.)).pt(px(8.)).border_t_1().border_color(theme::hairline()).child(
                    button("card-open", ButtonKind::Ghost, format!("Open in {}", network.label()))
                        .h(px(24.))
                        .px(px(6.))
                        .type_small()
                        .child(glyph("icons/external-link.svg", 14., theme::text_soft()))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.open_url(&url);
                            this.close(cx);
                        })),
                )
            }))
    }

    /// A row's More menu, or the card's: the card's has no way back to itself.
    fn row_menu(&self, person: &Person, from_card: bool, cx: &mut Context<Self>) -> Div {
        let profile = person.key.clone();
        let handle = person.handle.trim_start_matches('@').to_owned();
        let name = if handle.is_empty() { person.name.clone() } else { handle };
        let link = (!person.link.is_empty() && !from_card).then(|| person.link.clone());
        let label = format!("Open in {}", person.key.network.label());
        let (remove, block) = offers(person);
        let (removed, blocked) = (person.key.clone(), person.key.clone());
        menu()
            .w(px(200.))
            .when(!from_card, |menu| {
                menu.child(menu_item("menu-profile", Some("icons/user.svg"), "Profile", None).on_click(cx.listener(
                    move |this, _, _, cx| this.turn_to(Float::Card(profile.clone()), cx),
                )))
            })
            .child(menu_item("menu-copy", Some("icons/copy.svg"), "Copy username", None).on_click(cx.listener(
                move |this, _, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(name.clone()));
                    this.close(cx);
                },
            )))
            .children(link.map(|url| {
                menu_item("menu-open", Some("icons/external-link.svg"), label, None).on_click(cx.listener(
                    move |this, _, _, cx| {
                        cx.open_url(&url);
                        this.close(cx);
                    },
                ))
            }))
            .when(remove || block, |menu| menu.child(menu_separator()))
            .when(remove, |menu| {
                menu.child(
                    menu_item_danger("menu-remove", Some("icons/x.svg"), "Remove friend")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.turn_to(Float::Confirm(removed.clone(), RelateAction::Remove), cx)
                        })),
                )
            })
            .when(block, |menu| {
                menu.child(
                    menu_item_danger("menu-block", Some("icons/circle-alert.svg"), "Block")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.turn_to(Float::Confirm(blocked.clone(), RelateAction::Block), cx)
                        })),
                )
            })
    }

    /// The one question before a friendship ends or someone is blocked. Nothing is sent until
    /// its second button is clicked.
    fn confirm(&self, person: &Person, action: RelateAction, cx: &mut Context<Self>) -> Div {
        let network = person.key.network.label();
        let (title, words, verb) = match action {
            RelateAction::Block => (
                format!("Block {}?", person.name),
                format!("They will not be able to message you on {network}. They are not told."),
                "Block",
            ),
            _ => (
                format!("Remove {}?", person.name),
                format!("You will no longer be friends on {network}. They are not told."),
                "Remove friend",
            ),
        };
        let target = person.clone();
        popover()
            .w(px(300.))
            .p(px(16.))
            .flex()
            .flex_col()
            .child(div().type_ui().font_weight(theme::SEMIBOLD).child(title))
            .child(modal_body(words))
            .child(
                modal_buttons()
                    .mt(px(16.))
                    .child(
                        button("confirm-cancel", ButtonKind::Secondary, "Cancel")
                            .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
                    )
                    .child(
                        button("confirm-do", ButtonKind::Danger, verb)
                            .on_click(cx.listener(move |this, _, _, cx| this.relate(&target, action, cx))),
                    ),
            )
    }
}

impl Render for Floating {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some((float, anchor)) = self.open.clone() else {
            return div();
        };
        let key = float.person();
        // Someone seen only as a message's sender is still a card: the id is all that is known.
        let person = self.hub.person(key).unwrap_or_else(|| Person::new(key.clone(), key.id.clone()));
        let surface = match float {
            Float::Card(_) => self.card(&person, cx),
            Float::Menu(_) => self.row_menu(&person, false, cx),
            Float::More(_) => self.row_menu(&person, true, cx),
            Float::Confirm(_, action) => self.confirm(&person, action, cx),
        };
        // Escape goes to whatever has the keys, so the card takes them while it is up.
        let focus = self.focus.get_or_insert_with(|| cx.focus_handle()).clone();
        if !focus.is_focused(window) {
            window.focus(&focus, cx);
        }
        let surface = surface
            .track_focus(&focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    this.close(cx);
                }
            }))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close(cx)));
        div().child(
            deferred(
                anchored()
                    .position(anchor + point(px(8.), px(8.)))
                    .snap_to_window_with_margin(px(8.))
                    .child(fade_in(("float", self.opened), 120, surface)),
            )
            .with_priority(2),
        )
    }
}

#[cfg(feature = "gui-shot")]
impl Floating {
    /// A card's body as a network would answer it.
    pub fn stage_profile(&mut self, profile: Profile, cx: &mut Context<Self>) {
        self.hub.learn(std::slice::from_ref(&profile.person));
        self.profile = Some(profile);
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2025-10-13 12:00 UTC.
    const NOON_UTC: i64 = 1_760_356_800;

    #[test]
    fn their_clock_is_read_from_the_offset_and_set_against_ours() {
        // Noon UTC. They are in Lisbon in summer (+1), we are in New York (-4)... and the reverse.
        assert_eq!(local_time(NOON_UTC + 48 * 60, 3600, 4 * 3600), "1:48 PM · 3 h behind you");
        assert_eq!(local_time(NOON_UTC, 3600, -4 * 3600), "1:00 PM · 5 h ahead of you");
        assert_eq!(local_time(NOON_UTC, -4 * 3600, -4 * 3600), "8:00 AM · same time as you");
        // Half-hour zones, and a clock that wraps past midnight.
        assert_eq!(local_time(NOON_UTC, 19_800, 0), "5:30 PM · 5 h 30 min ahead of you");
        assert_eq!(local_time(NOON_UTC, -12 * 3600 - 1800, -12 * 3600), "11:30 PM · 30 min behind you");
        assert_eq!(local_time(NOON_UTC, 12 * 3600, 0), "12:00 AM · 12 h ahead of you");
    }

    #[test]
    fn a_card_names_five_shared_spaces_and_counts_the_rest() {
        let spaces: Vec<String> = (0..8).map(|n| format!("Space {n}")).collect();
        let (shown, more) = spaces_shown(&spaces);
        assert_eq!((shown.len(), more), (5, 3));
        assert_eq!(spaces_shown(&spaces[..2]), (&spaces[..2], 0));
    }

    #[test]
    fn only_a_friend_can_be_removed_and_nobody_is_blocked_twice() {
        let person = |network, relation| Person {
            key: Key { network, account: "1".into(), id: "p".into() },
            name: "P".into(),
            handle: String::new(),
            avatar: None,
            bot: false,
            relation,
            presence: Presence::Unknown,
            status: String::new(),
            link: String::new(),
        };
        assert_eq!(offers(&person(Network::Discord, Relation::Friend)), (true, true));
        assert_eq!(offers(&person(Network::Discord, Relation::None)), (false, true));
        assert_eq!(offers(&person(Network::Discord, Relation::Blocked)), (false, false));
        assert_eq!(offers(&person(Network::Slack, Relation::Contact)), (false, false));
    }
}
