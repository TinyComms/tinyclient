//! Our own status, in the popover of the rail's avatar: each account's link, how others see us
//! there, and setting it. One account is open at a time, showing the states its network has and
//! the status text. The head is how Tiny shows us on this device: a name and picture of our own,
//! kept in the window's prefs, else what the accounts say.

use super::presence::{self, Shape};
use super::{only_draws, Hub, Own, Scene};
use crate::gui::app::prefs::{self, Me};
use crate::gui::components::{
    button, field, glyph, icon_button, menu_item, menu_label, menu_separator, network_icon, tip, Avatar, ButtonKind,
};
use crate::gui::input::{InputEvent, InputKey, TextInput};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use gpui::{div, prelude::*, px, Context, Div, Entity, Focusable, PathPromptOptions, Rgba, Stateful, WeakEntity, Window};
use protocol::{AccountKey, Key, Link, Network, Presence, Request, Response};
use std::path::Path;

pub const SOURCE: &str = "own";
const WIDTH: f32 = 288.;
/// The picture formats the avatar draws, and the most a chosen one may weigh.
const PICTURES: [&str; 5] = ["png", "jpg", "jpeg", "gif", "webp"];
const PICTURE_MAX: u64 = 10 * 1024 * 1024;

/// A row of the phone's status sheet: the height every phone menu row has (`menu_item`).
fn phone_row() -> f32 {
    (crate::gui::phone::tokens::scaled(22.) + 26.).max(48.)
}

/// How available a presence is: the rail shows the most available one among our accounts.
fn rank(presence: Presence) -> u8 {
    match presence {
        Presence::Online => 4,
        Presence::Idle | Presence::Busy => 3,
        Presence::Dnd => 2,
        Presence::Offline => 1,
        Presence::Unknown => 0,
    }
}

pub fn most_available(states: impl IntoIterator<Item = Presence>) -> Presence {
    states.into_iter().max_by_key(|presence| rank(*presence)).unwrap_or(Presence::Unknown)
}

fn connected(scene: &Scene) -> impl Iterator<Item = &Own> {
    scene.accounts.iter().filter(|own| own.link == Link::Connected)
}

/// Our own person on each account that says who we are.
fn own_keys(scene: &Scene) -> Vec<Key> {
    connected(scene).filter(|own| !own.me.is_empty()).map(|own| Key::new(&own.key, own.me.clone())).collect()
}

/// The states a network lets us choose. Telegram is online while a client is open and has no
/// other; Slack has two. A Tiny server offers states only when it advertises `presence_status`.
pub fn states(network: Network, can: &protocol::Capabilities) -> &'static [Presence] {
    match network {
        Network::Discord => &[Presence::Online, Presence::Idle, Presence::Dnd, Presence::Offline],
        Network::Teams => &[Presence::Online, Presence::Idle, Presence::Busy, Presence::Dnd, Presence::Offline],
        Network::Slack => &[Presence::Online, Presence::Idle],
        Network::Tiny if can.presence_status => &[Presence::Online, Presence::Idle, Presence::Dnd, Presence::Offline],
        // Signal shows no presence at all.
        Network::Telegram | Network::Signal | Network::Messages | Network::Tiny => &[],
    }
}

fn has_status_text(network: Network, can: &protocol::Capabilities) -> bool {
    match network {
        Network::Tiny => can.presence_status,
        Network::Telegram | Network::Signal | Network::Messages => false,
        _ => true,
    }
}

/// The word for a state of our own: choosing to look offline has the network's name for it.
pub fn state_word(presence: Presence, network: Network) -> &'static str {
    match (presence, network) {
        (Presence::Offline, Network::Discord | Network::Tiny) => "Invisible",
        (Presence::Offline, Network::Teams) => "Appear offline",
        _ => presence::word(presence, network),
    }
}

/// What an account's row is called: the network, or the workspace where the network has
/// several.
pub fn account_label(accounts: &[Own], own: &Own) -> String {
    let several = accounts.iter().filter(|other| other.key.network == own.key.network).count() > 1;
    if several && !own.name.is_empty() {
        own.name.clone()
    } else {
        own.key.network.label().to_owned()
    }
}

/// The head's one line of truth.
fn summary(scene: &Scene) -> String {
    let states: Vec<Presence> = connected(scene).map(|own| own.presence).collect();
    let best = most_available(states.iter().copied());
    let word = presence::word(best, Network::Discord);
    match (states.len(), best) {
        (0, _) => "Not connected".to_owned(),
        (1, Presence::Unknown) => "Connected".to_owned(),
        (count, Presence::Unknown) => format!("Connected on {count} accounts"),
        (1, _) => word.to_owned(),
        (count, _) if states.iter().all(|state| *state == best) => format!("{word} on {count} accounts"),
        _ => word.to_owned(),
    }
}

/// Quick-pick status emojis offered in the rich status editor.
const STATUS_EMOJIS: [&str; 6] = ["📅", "🌴", "🤒", "🏠", "🚌", "💬"];

/// Preset expiration choices for "Clear after" in the rich status editor.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ClearAfter {
    #[default]
    Never,
    Minutes30,
    Hour1,
    Today,
    ThisWeek,
}

impl ClearAfter {
    const ALL: [ClearAfter; 5] = [
        ClearAfter::Never,
        ClearAfter::Minutes30,
        ClearAfter::Hour1,
        ClearAfter::Today,
        ClearAfter::ThisWeek,
    ];

    fn label(self) -> &'static str {
        match self {
            ClearAfter::Never => "Don't clear",
            ClearAfter::Minutes30 => "30m",
            ClearAfter::Hour1 => "1h",
            ClearAfter::Today => "Today",
            ClearAfter::ThisWeek => "This week",
        }
    }

    pub fn until(self, now: i64) -> Option<i64> {
        match self {
            ClearAfter::Never => None,
            ClearAfter::Minutes30 => Some(now + 30 * 60),
            ClearAfter::Hour1 => Some(now + 3600),
            ClearAfter::Today => Some(now + 24 * 3600 - (now.rem_euclid(24 * 3600))),
            ClearAfter::ThisWeek => Some(now + 7 * 24 * 3600),
        }
    }
}

/// Preset durations for Slack's "Pause notifications" (`dnd.setSnooze`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DndPreset {
    Minutes30,
    Hour1,
    Hours2,
    UntilTomorrow,
}

impl DndPreset {
    const ALL: [DndPreset; 4] = [
        DndPreset::Minutes30,
        DndPreset::Hour1,
        DndPreset::Hours2,
        DndPreset::UntilTomorrow,
    ];

    pub fn label(self) -> &'static str {
        match self {
            DndPreset::Minutes30 => "30m",
            DndPreset::Hour1 => "1h",
            DndPreset::Hours2 => "2h",
            DndPreset::UntilTomorrow => "Until tomorrow",
        }
    }

    pub fn until(self, now: i64) -> i64 {
        match self {
            DndPreset::Minutes30 => now + 30 * 60,
            DndPreset::Hour1 => now + 3600,
            DndPreset::Hours2 => now + 2 * 3600,
            // Next day at 09:00 UTC (at least 1 hour ahead, capped at 24h).
            DndPreset::UntilTomorrow => {
                let day = 86_400;
                let midnight = now - now.rem_euclid(day);
                let nine_am = midnight + day + 9 * 3600;
                (nine_am - now).clamp(3600, day) + now
            }
        }
    }
}

pub struct StatusMenu {
    shell: WeakEntity<Shell>,
    hub: Hub,
    scene: Scene,
    /// The account whose states and status text are showing.
    open: Option<AccountKey>,
    field: Entity<TextInput>,
    /// Selected status emoji (empty means none / default) and clear-after preset for rich status.
    emoji: String,
    clear_after: ClearAfter,
    refused: Option<String>,
    /// The head is editing our name and picture on this device.
    editing: bool,
    name: Entity<TextInput>,
    /// Out-of-office / automatic replies inline form (for accounts with `can.out_of_office`).
    ooo_open: Option<AccountKey>,
    ooo_loading: bool,
    ooo_saving: bool,
    ooo_enabled: bool,
    ooo_external: bool,
    ooo_days: u32,
    ooo_message: Entity<TextInput>,
    ooo_external_message: Entity<TextInput>,
}

/// What an account's row says of it: how others see us there while it is connected, else why
/// it is not. Every row says something.
fn link_word(own: &Own) -> (&'static str, Rgba) {
    let network = own.key.network;
    match own.link {
        Link::Connected => match (network, own.presence) {
            (Network::Telegram, _) => ("Online while open", theme::text_soft()),
            // Signal and Google Messages show no presence; the others until they say one.
            (_, Presence::Unknown) => ("Connected", theme::text_soft()),
            (_, presence) => (state_word(presence, network), theme::text_soft()),
        },
        Link::Offline if own.reconnecting => ("Reconnecting…", theme::warning()),
        Link::Offline => ("Offline", theme::warning()),
        Link::Expired => ("Signed out by the network", theme::danger()),
        Link::SignedOut => ("Signed out", theme::text_soft()),
    }
}

/// Copies a chosen picture beside the prefs under a new name, so the avatar's cache never shows
/// the one it replaces. Answers the file name, or why not in words.
fn keep_picture(from: &Path) -> Result<String, String> {
    let extension = from.extension().and_then(|ext| ext.to_str()).unwrap_or("").to_ascii_lowercase();
    if !PICTURES.contains(&extension.as_str()) {
        return Err("Choose a PNG, JPEG, GIF or WebP picture.".to_owned());
    }
    let size = std::fs::metadata(from).map_err(|_| "That picture could not be read.".to_owned())?.len();
    if size > PICTURE_MAX {
        return Err("Choose a picture under 10 MB.".to_owned());
    }
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_millis());
    let file = format!("me-{stamp}.{extension}");
    std::fs::copy(from, protocol::support_dir().join(&file)).map_err(|_| "That picture could not be copied.".to_owned())?;
    Ok(file)
}

impl StatusMenu {
    pub fn new(shell: WeakEntity<Shell>, hub: Hub, cx: &mut Context<Self>) -> StatusMenu {
        let field = cx.new(|cx| TextInput::new("Set a status", cx));
        cx.subscribe(&field, |this, _, event: &InputEvent, cx| match event {
            InputEvent::Changed => cx.notify(),
            InputEvent::Submit => this.say(cx),
        })
        .detach();
        cx.subscribe(&field, |this, _, key: &InputKey, cx| {
            if *key == InputKey::Escape {
                let _ = this.shell.update(cx, |shell, cx| shell.close_menu(cx));
            }
        })
        .detach();
        let name = cx.new(|cx| TextInput::new("Your name", cx));
        cx.subscribe(&name, |this, _, event: &InputEvent, cx| match event {
            InputEvent::Changed => cx.notify(),
            InputEvent::Submit => this.finish(true, cx),
        })
        .detach();
        cx.subscribe(&name, |this, _, key: &InputKey, cx| {
            if *key == InputKey::Escape {
                this.finish(false, cx);
            }
        })
        .detach();
        let ooo_message = cx.new(|cx| TextInput::new("Internal automatic reply", cx));
        let ooo_external_message = cx.new(|cx| TextInput::new("External reply (defaults to internal)", cx));
        StatusMenu {
            shell,
            hub,
            scene: Scene::default(),
            open: None,
            field,
            emoji: String::new(),
            clear_after: ClearAfter::Never,
            refused: None,
            editing: false,
            name,
            ooo_open: None,
            ooo_loading: false,
            ooo_saving: false,
            ooo_enabled: false,
            ooo_external: false,
            ooo_days: 0,
            ooo_message,
            ooo_external_message,
        }
    }

    /// The menu opened again: it starts as it was first seen, nothing half edited.
    pub fn opened(&mut self, cx: &mut Context<Self>) {
        self.editing = false;
        self.ooo_open = None;
        self.refused = None;
        cx.notify();
    }

    fn edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mine = prefs::me().name;
        self.name.update(cx, |name, cx| name.set_text(&mine, cx));
        self.editing = true;
        self.refused = None;
        window.focus(&self.name.focus_handle(cx), cx);
        cx.notify();
    }

    /// Keeps the typed name (empty goes back to the accounts' one), or leaves it as it was.
    fn finish(&mut self, keep: bool, cx: &mut Context<Self>) {
        if keep {
            let name: String = self.name.read(cx).text().trim().chars().take(64).collect();
            self.set_me(Me { name, ..prefs::me() }, cx);
        }
        self.editing = false;
        self.refused = None;
        cx.notify();
    }

    /// Stores how we are shown here; the prefs file is written with the window's next frame.
    /// A picture no longer used is deleted.
    fn set_me(&mut self, me: Me, cx: &mut Context<Self>) {
        let before = prefs::me();
        if before.picture != me.picture {
            if let Some(old) = before.picture_path() {
                let _ = std::fs::remove_file(old);
            }
        }
        prefs::set_me(me);
        let _ = self.shell.update(cx, |_, cx| cx.notify());
        cx.notify();
    }

    fn pick_picture(&mut self, cx: &mut Context<Self>) {
        let asked = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some("Use as your picture".into()) });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = asked.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let kept = cx.background_executor().spawn(async move { keep_picture(&path) }).await;
            let _ = this.update(cx, |this, cx| match kept {
                Ok(picture) => {
                    this.refused = None;
                    this.set_me(Me { picture, ..prefs::me() }, cx);
                }
                Err(why) => {
                    this.refused = Some(why);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub fn see(&mut self, scene: Scene, cx: &mut Context<Self>) {
        if self.scene == scene {
            return;
        }
        // An account that dropped is not settable, so it closes.
        if self.open.as_ref().is_some_and(|open| !connected(&scene).any(|own| &own.key == open)) {
            self.open = None;
        }
        self.scene = scene;
        cx.notify();
    }

    fn own(&self, account: &AccountKey) -> Option<&Own> {
        self.scene.accounts.iter().find(|own| &own.key == account)
    }

    fn toggle(&mut self, account: &AccountKey, cx: &mut Context<Self>) {
        self.refused = None;
        self.ooo_open = None;
        if self.open.as_ref() == Some(account) {
            self.open = None;
        } else {
            let said = self.own(account).map(|own| own.status.clone()).unwrap_or_default();
            self.field.update(cx, |field, cx| field.set_text(&said, cx));
            self.emoji.clear();
            self.clear_after = ClearAfter::Never;
            self.open = Some(account.clone());
        }
        cx.notify();
    }

    fn send(
        &mut self,
        account: AccountKey,
        presence: Presence,
        status: Option<String>,
        status_emoji: Option<String>,
        clear_at: Option<i64>,
        cx: &mut Context<Self>,
    ) {
        self.refused = None;
        cx.notify();
        if only_draws() {
            return;
        }
        let this = cx.weak_entity();
        // chatd reports the new state as a `Status` change, which is what redraws the rows.
        super::ask_later(
            cx,
            Request::SetPresence { account, presence, status, status_emoji, clear_at },
            move |answer, cx| {
                let refused = match answer {
                    Ok(Response::Error { detail, .. }) => Some(detail),
                    Ok(_) => None,
                    Err(err) => Some(crate::client::trouble_words("people", &err)),
                };
                let _ = this.update(cx, |this, cx| {
                    this.refused = refused;
                    cx.notify();
                });
            },
        );
    }

    /// The status text of the open account: what is in the field, and empty clears it.
    fn say(&mut self, cx: &mut Context<Self>) {
        let Some(own) = self.open.as_ref().and_then(|open| self.own(open)).cloned() else {
            return;
        };
        // CONTRACT: `SetPresence` cannot change the text and leave the presence alone, so the
        // presence goes back as chatd last reported it; until it has reported one there is
        // nothing safe to send.
        if own.presence == Presence::Unknown {
            self.refused = Some("Choose a state first.".to_owned());
            cx.notify();
            return;
        }
        let text = self.field.read(cx).text().trim().to_owned();
        let (status_emoji, clear_at) = if own.can.rich_status {
            let em = if text.is_empty() && self.emoji.is_empty() {
                Some(String::new())
            } else if !self.emoji.is_empty() {
                Some(self.emoji.clone())
            } else {
                None
            };
            let until = if text.is_empty() && self.emoji.is_empty() {
                None
            } else {
                self.clear_after.until(crate::view::now())
            };
            (em, until)
        } else if own.key.network == Network::Teams {
            let until = if text.is_empty() {
                None
            } else {
                self.clear_after.until(crate::view::now())
            };
            (None, until)
        } else {
            (None, None)
        };
        self.send(own.key, own.presence, Some(text), status_emoji, clear_at, cx);
    }

    fn toggle_ooo(&mut self, account: AccountKey, cx: &mut Context<Self>) {
        self.refused = None;
        if self.ooo_open.as_ref() == Some(&account) {
            self.ooo_open = None;
            cx.notify();
            return;
        }
        self.ooo_open = Some(account.clone());
        self.ooo_loading = true;
        self.ooo_saving = false;
        cx.notify();
        if only_draws() {
            self.ooo_loading = false;
            return;
        }
        let this = cx.weak_entity();
        let req_account = account.clone();
        super::ask_later(cx, Request::GetOutOfOffice { account }, move |answer, cx| {
            let _ = this.update(cx, |this, cx| {
                if this.ooo_open.as_ref() != Some(&req_account) {
                    return;
                }
                this.ooo_loading = false;
                match answer {
                    Ok(Response::OutOfOffice { ooo }) => {
                        this.ooo_enabled = ooo.enabled;
                        this.ooo_external = ooo.send_external;
                        this.ooo_days = match (ooo.start, ooo.end) {
                            (Some(s), Some(e)) if e > s => ((e - s) / 86_400).clamp(1, 7) as u32,
                            _ => 0,
                        };
                        this.ooo_message.update(cx, |f, cx| f.set_text(&ooo.message, cx));
                        this.ooo_external_message.update(cx, |f, cx| f.set_text(&ooo.external_message, cx));
                    }
                    Ok(Response::Error { detail, .. }) => this.refused = Some(detail),
                    Err(err) => this.refused = Some(crate::client::trouble_words("people", &err)),
                    _ => {}
                }
                cx.notify();
            });
        });
    }

    fn save_ooo(&mut self, account: AccountKey, cx: &mut Context<Self>) {
        if self.ooo_saving {
            return;
        }
        self.refused = None;
        self.ooo_saving = true;
        let now = crate::view::now();
        let (start, end) = if self.ooo_enabled && self.ooo_days > 0 {
            (Some(now), Some(now + i64::from(self.ooo_days) * 86_400))
        } else {
            (None, None)
        };
        let ooo = protocol::OutOfOffice {
            enabled: self.ooo_enabled,
            message: self.ooo_message.read(cx).text().trim().to_owned(),
            start,
            end,
            send_external: self.ooo_enabled && self.ooo_external,
            external_message: self.ooo_external_message.read(cx).text().trim().to_owned(),
        };
        cx.notify();
        if only_draws() {
            self.ooo_saving = false;
            self.ooo_open = None;
            return;
        }
        let this = cx.weak_entity();
        super::ask_later(cx, Request::SetOutOfOffice { account, ooo }, move |answer, cx| {
            let _ = this.update(cx, |this, cx| {
                this.ooo_saving = false;
                match answer {
                    Ok(Response::OutOfOffice { .. }) => {
                        this.ooo_open = None;
                    }
                    Ok(Response::Error { detail, .. }) => this.refused = Some(detail),
                    Err(err) => this.refused = Some(crate::client::trouble_words("people", &err)),
                    _ => {}
                }
                cx.notify();
            });
        });
    }

    fn ooo_panel(&self, ix: usize, own: &Own, window: &Window, cx: &mut Context<Self>) -> Div {
        let account = own.key.clone();
        let open = self.ooo_open.as_ref() == Some(&own.key);
        let mut box_div = div().mt(px(4.)).flex().flex_col().gap(px(4.)).child(
            div()
                .id(("own-ooo-toggle", ix))
                .h(px(theme::ROW_ONE))
                .px(px(8.))
                .rounded(px(theme::R_CONTROL))
                .flex()
                .items_center()
                .gap(px(8.))
                .cursor_pointer()
                .hover(|style| style.bg(theme::selected()))
                .child(glyph("icons/calendar.svg", 14., theme::text_soft()))
                .child(div().flex_1().min_w_0().truncate().child("Out of office…"))
                .child(glyph(if open { "icons/chevron-down.svg" } else { "icons/chevron-right.svg" }, 14., theme::text_faint()))
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_ooo(account.clone(), cx))),
        );
        if !open {
            return box_div;
        }
        if self.ooo_loading {
            return box_div.child(div().px(px(8.)).type_small().text_color(theme::text_faint()).child("Loading automatic replies…"));
        }
        let save_acc = own.key.clone();
        let msg_focused = self.ooo_message.focus_handle(cx).is_focused(window);
        let ext_focused = self.ooo_external_message.focus_handle(cx).is_focused(window);
        let durations: [(&str, u32); 4] = [("Until off", 0), ("1 day", 1), ("3 days", 3), ("1 week", 7)];
        let form = div()
            .mx(px(8.))
            .p(px(8.))
            .rounded(px(theme::R_CONTROL))
            .bg(theme::field())
            .border_1()
            .border_color(theme::border())
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(
                div()
                    .id(("ooo-enabled", ix))
                    .flex()
                    .items_center()
                    .justify_between()
                    .cursor_pointer()
                    .child(div().type_small().font_weight(theme::MEDIUM).text_color(theme::text()).child("Send automatic replies"))
                    .child(
                        div()
                            .px(px(6.))
                            .py(px(2.))
                            .rounded(px(theme::R_CHIP))
                            .bg(if self.ooo_enabled { theme::accent() } else { theme::selected() })
                            .type_micro()
                            .text_color(if self.ooo_enabled { theme::on_accent() } else { theme::text_soft() })
                            .child(if self.ooo_enabled { "On" } else { "Off" }),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.ooo_enabled = !this.ooo_enabled;
                        cx.notify();
                    })),
            )
            .when(self.ooo_enabled, |form| {
                form.child(
                    field(msg_focused)
                        .line_height(px(18.))
                        .child(div().flex_1().min_w_0().child(self.ooo_message.clone())),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(4.))
                        .children(durations.into_iter().enumerate().map(|(dix, (label, days))| {
                            let chosen = self.ooo_days == days;
                            div()
                                .id(("ooo-dur", ix * 8 + dix))
                                .px(px(6.))
                                .py(px(2.))
                                .rounded(px(theme::R_CHIP))
                                .type_micro()
                                .text_color(if chosen { theme::text() } else { theme::text_soft() })
                                .when(chosen, |el| el.bg(theme::selected()))
                                .cursor_pointer()
                                .hover(|el| el.bg(theme::hover()))
                                .child(label)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.ooo_days = days;
                                    cx.notify();
                                }))
                        })),
                )
                .child(
                    div()
                        .id(("ooo-external", ix))
                        .flex()
                        .items_center()
                        .justify_between()
                        .cursor_pointer()
                        .child(div().type_small().text_color(theme::text()).child("Reply outside organization"))
                        .child(
                            div()
                                .px(px(6.))
                                .py(px(2.))
                                .rounded(px(theme::R_CHIP))
                                .bg(if self.ooo_external { theme::warning() } else { theme::selected() })
                                .type_micro()
                                .text_color(if self.ooo_external { theme::on_accent() } else { theme::text_soft() })
                                .child(if self.ooo_external { "On" } else { "Off" }),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.ooo_external = !this.ooo_external;
                            cx.notify();
                        })),
                )
                .when(self.ooo_external, |form| {
                    form.child(
                        field(ext_focused)
                            .line_height(px(18.))
                            .child(div().flex_1().min_w_0().child(self.ooo_external_message.clone())),
                    )
                })
            })
            .child(
                div()
                    .type_micro()
                    .text_color(theme::text_faint())
                    .child("Also updates your Outlook automatic replies. Turning on external replies notifies senders outside your organization."),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(4.))
                    .child(
                        button(("ooo-cancel", ix), ButtonKind::Ghost, "Cancel")
                            .h(px(24.))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.ooo_open = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        button(("ooo-save", ix), ButtonKind::Primary, if self.ooo_saving { "Saving…" } else { "Save" })
                            .h(px(24.))
                            .on_click(cx.listener(move |this, _, _, cx| this.save_ooo(save_acc.clone(), cx))),
                    ),
            );
        box_div = box_div.child(form);
        box_div
    }

    fn account_row(&self, ix: usize, own: &Own, cx: &mut Context<Self>) -> Stateful<Div> {
        let network = own.key.network;
        let settable = own.link == Link::Connected && !states(network, &own.can).is_empty();
        let open = self.open.as_ref() == Some(&own.key);
        let (word, color) = link_word(own);
        let shape = Shape::of(own.presence).filter(|_| own.link == Link::Connected);
        let account = own.key.clone();
        div()
            .id(("own-account", ix))
            .h(px(if crate::gui::phone::active() { phone_row() } else { theme::ROW_ONE }))
            .px(px(if crate::gui::phone::active() { 12. } else { 8. }))
            .rounded(px(if crate::gui::phone::active() { theme::R_CARD } else { theme::R_CONTROL }))
            .flex()
            .items_center()
            .gap(px(if crate::gui::phone::active() { 14. } else { 8. }))
            .child(network_icon(network, if crate::gui::phone::active() { 20. } else { 16. }))
            .child(div().flex_1().min_w_0().truncate().child(account_label(&self.scene.accounts, own)))
            .children(shape.map(|shape| presence::inline(shape, 10., theme::overlay())))
            .child(div().flex_none().type_small().text_color(color).child(word))
            .when(settable, |row| {
                let chevron = if open { "icons/chevron-down.svg" } else { "icons/chevron-right.svg" };
                row.cursor_pointer()
                    .hover(|style| style.bg(theme::selected()))
                    .child(glyph(chevron, 14., theme::text_faint()))
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle(&account, cx)))
            })
    }

    /// The open account's states, the current one checked, then its status text.
    fn choices(&self, ix: usize, own: &Own, window: &Window, cx: &mut Context<Self>) -> Div {
        let network = own.key.network;
        let mut choices = div().pl(px(24.)).pb(px(4.)).flex().flex_col();
        for (at, state) in states(network, &own.can).iter().copied().enumerate() {
            let account = own.key.clone();
            choices = choices.child(
                div()
                    .id(("own-state", ix * 8 + at))
                    .h(px(if crate::gui::phone::active() { theme::row_height(theme::ROW_ONE) } else { theme::ROW_ONE }))
                    .px(px(8.))
                    .rounded(px(theme::R_CONTROL))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::selected()))
                    .children(Shape::of(state).map(|shape| presence::inline(shape, 10., theme::overlay())))
                    .child(div().flex_1().min_w_0().truncate().child(state_word(state, network)))
                    .when(own.presence == state, |row| row.child(glyph("icons/check.svg", 14., theme::text_soft())))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let clear_at = (network == Network::Teams)
                            .then(|| this.clear_after.until(crate::view::now()))
                            .flatten();
                        this.send(account.clone(), state, None, None, clear_at, cx);
                    })),
            );
        }
        if network == Network::Teams {
            let reset_acc = own.key.clone();
            choices = choices
                .child(
                    div()
                        .id(("own-reset-status", ix))
                        .h(px(if crate::gui::phone::active() { theme::row_height(theme::ROW_ONE) } else { theme::ROW_ONE }))
                        .px(px(8.))
                        .rounded(px(theme::R_CONTROL))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .cursor_pointer()
                        .hover(|style| style.bg(theme::selected()))
                        .child(glyph("icons/rotate-ccw.svg", 14., theme::text_soft()))
                        .child(div().flex_1().min_w_0().truncate().child("Reset status"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.clear_after = ClearAfter::Never;
                            this.send(reset_acc.clone(), Presence::Unknown, None, None, None, cx);
                        })),
                )
                .child(
                    div()
                        .mt(px(2.))
                        .px(px(8.))
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .child(div().type_micro().text_color(theme::text_faint()).child("Reset automatically after"))
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap(px(4.))
                                .children(ClearAfter::ALL.iter().enumerate().map(|(cix, &preset)| {
                                    let chosen = self.clear_after == preset;
                                    div()
                                        .id(("own-teams-clear-after", ix * 8 + cix))
                                        .px(px(6.))
                                        .py(px(2.))
                                        .rounded(px(theme::R_CHIP))
                                        .type_micro()
                                        .text_color(if chosen { theme::text() } else { theme::text_soft() })
                                        .when(chosen, |el| el.bg(theme::selected()))
                                        .cursor_pointer()
                                        .hover(|el| el.bg(theme::hover()))
                                        .child(preset.label())
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.clear_after = preset;
                                            cx.notify();
                                        }))
                                })),
                        ),
                );
        }
        if own.can.dnd_snooze {
            if own.presence == Presence::Dnd {
                let account = own.key.clone();
                choices = choices.child(
                    div()
                        .id(("own-dnd-resume", ix))
                        .h(px(if crate::gui::phone::active() { theme::row_height(theme::ROW_ONE) } else { theme::ROW_ONE }))
                        .px(px(8.))
                        .rounded(px(theme::R_CONTROL))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .cursor_pointer()
                        .hover(|style| style.bg(theme::selected()))
                        .child(glyph("icons/bell.svg", 14., theme::text_soft()))
                        .child(div().flex_1().min_w_0().truncate().child("Resume notifications"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.send(account.clone(), Presence::Online, None, None, None, cx);
                        })),
                );
            }
            let dnd_row = div()
                .mt(px(2.))
                .px(px(8.))
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(div().type_micro().text_color(theme::text_faint()).child("Pause notifications"))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(4.))
                        .children(DndPreset::ALL.iter().enumerate().map(|(pix, &preset)| {
                            let account = own.key.clone();
                            div()
                                .id(("own-dnd-preset", ix * 8 + pix))
                                .px(px(6.))
                                .py(px(2.))
                                .rounded(px(theme::R_CHIP))
                                .type_micro()
                                .text_color(theme::text_soft())
                                .cursor_pointer()
                                .hover(|el| el.bg(theme::hover()))
                                .child(preset.label())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let until = preset.until(crate::view::now());
                                    this.send(account.clone(), Presence::Dnd, None, None, Some(until), cx);
                                }))
                        })),
                );
            choices = choices.child(dnd_row);
        }
        if !has_status_text(network, &own.can) {
            return choices;
        }
        let focused = self.field.focus_handle(cx).is_focused(window);
        let typed = !self.field.read(cx).text().is_empty();
        let clear = typed.then(|| {
            div()
                .id("own-status-clear")
                .size(px(20.))
                .flex_none()
                .rounded(px(theme::R_CHIP))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|style| style.bg(theme::hover()))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.field.update(cx, |field, cx| field.set_text("", cx));
                    this.emoji.clear();
                    this.clear_after = ClearAfter::Never;
                    this.say(cx);
                }))
                .child(glyph("icons/x.svg", 14., theme::text_soft()))
        });
        let unsent = self.field.read(cx).text().trim() != own.status.trim() || !self.emoji.is_empty();
        let leading = if !self.emoji.is_empty() {
            div().type_small().child(self.emoji.clone()).into_any_element()
        } else {
            glyph("icons/smile.svg", 14., theme::text_faint()).into_any_element()
        };
        let mut status_box = div().mt(px(4.)).px(px(8.)).flex().flex_col().gap(px(4.)).child(
            field(focused)
                .line_height(px(18.))
                .child(leading)
                .child(div().flex_1().min_w_0().child(self.field.clone()))
                .children(clear),
        );
        if own.can.rich_status {
            let emoji_row = div()
                .flex()
                .items_center()
                .gap(px(4.))
                .children(STATUS_EMOJIS.iter().enumerate().map(|(eix, &em)| {
                    let chosen = self.emoji == em;
                    div()
                        .id(("own-status-emoji", eix))
                        .px(px(5.))
                        .py(px(2.))
                        .rounded(px(theme::R_CHIP))
                        .cursor_pointer()
                        .when(chosen, |el| el.bg(theme::selected()))
                        .hover(|el| el.bg(theme::hover()))
                        .child(em)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if this.emoji == em {
                                this.emoji.clear();
                            } else {
                                this.emoji = em.to_owned();
                            }
                            cx.notify();
                        }))
                }));
            let clear_row = div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(4.))
                .children(ClearAfter::ALL.iter().enumerate().map(|(cix, &preset)| {
                    let chosen = self.clear_after == preset;
                    div()
                        .id(("own-status-clear-after", cix))
                        .px(px(6.))
                        .py(px(2.))
                        .rounded(px(theme::R_CHIP))
                        .type_micro()
                        .text_color(if chosen { theme::text() } else { theme::text_soft() })
                        .when(chosen, |el| el.bg(theme::selected()))
                        .cursor_pointer()
                        .hover(|el| el.bg(theme::hover()))
                        .child(preset.label())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.clear_after = preset;
                            cx.notify();
                        }))
                }));
            status_box = status_box.child(emoji_row).child(clear_row);
        }
        let mut out = choices.child(status_box.when(unsent, |status| {
            status.child(div().type_small().text_color(theme::text_faint()).child(crate::gui::shortcuts::label("Return sets it.")))
        }));
        if own.can.out_of_office {
            out = out.child(self.ooo_panel(ix, own, window, cx));
        }
        out
    }
}

impl StatusMenu {
    /// Who the accounts say we are: our own person where one is known, else the name of an
    /// account that is a person (a workspace's name is not ours).
    fn account_name(&self) -> String {
        let scene = &self.scene;
        let me = own_keys(scene).iter().find_map(|own| self.hub.person(own));
        let personal = scene
            .accounts
            .iter()
            .find(|own| matches!(own.key.network, Network::Discord | Network::Telegram) && !own.name.is_empty());
        match (me, personal) {
            (Some(me), _) => me.name,
            (None, Some(own)) => own.name.clone(),
            (None, None) => "You".to_owned(),
        }
    }

    /// Our name and avatar as shown here: this device's own first, then the accounts'.
    fn identity(&self, size: f32, cx: &mut Context<Self>) -> (String, Avatar) {
        let (scene, hub) = (&self.scene, &self.hub);
        let local = prefs::me();
        let name = if local.name.is_empty() { self.account_name() } else { local.name.clone() };
        let me = own_keys(scene).iter().find_map(|own| hub.person(own));
        let pictured = connected(scene).find(|own| own.avatar.is_some() && !own.me.is_empty());
        let avatar = match (&me, pictured) {
            (Some(me), _) if me.avatar.is_some() => {
                let picture = hub.avatar(&me.key.account(), me.avatar.as_ref(), cx);
                Avatar::new(&me.key, &name, size).image(picture.as_ref())
            }
            (_, Some(own)) => {
                let picture = hub.avatar(&own.key, own.avatar.as_ref(), cx);
                Avatar::new(&Key::new(&own.key, own.me.clone()), &name, size).image(picture.as_ref())
            }
            (Some(me), None) => Avatar::new(&me.key, &name, size),
            (None, None) => Avatar::named(&name, size),
        };
        let avatar = match local.picture_path() {
            Some(path) => avatar.image_at(Some(path)),
            None => avatar,
        };
        (name, avatar)
    }

    fn head(&self, cx: &mut Context<Self>) -> Div {
        // A phone's sheet heads with the 40 pt face its row sheet has.
        let face = if crate::gui::phone::active() { 40. } else { 32. };
        let (name, avatar) = self.identity(face, cx);
        let ring = presence::Ring::on(theme::overlay());
        let best = most_available(connected(&self.scene).map(|own| own.presence));
        div()
            .h(px(if crate::gui::phone::active() { 60. } else { theme::ROW_TWO }))
            .pl(px(if crate::gui::phone::active() { 12. } else { 8. }))
            .pr(px(4.))
            .flex()
            .items_center()
            .gap(px(10.))
            .child(avatar.build().children(Shape::of(best).and_then(|shape| presence::dot(shape, face, &ring))))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(div().truncate().font_weight(theme::MEDIUM).child(name))
                    .child(div().truncate().type_small().text_color(theme::text_soft()).child(summary(&self.scene))),
            )
            .child(
                icon_button("own-edit", "icons/pencil.svg", false)
                    .size(px(24.))
                    .tooltip(tip("Edit your name and picture on this device", None))
                    .on_click(cx.listener(|this, _, window, cx| this.edit(window, cx))),
            )
    }

    /// The head while editing: the picture and the name, and that both stay on this device.
    fn editor(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let (_, avatar) = self.identity(48., cx);
        let chosen = !prefs::me().picture.is_empty();
        let focused = self.name.focus_handle(cx).is_focused(window);
        let theirs = self.account_name();
        self.name.update(cx, |name, _| name.set_placeholder(theirs));
        let picture = if chosen { "Change picture…" } else { "Choose picture…" };
        div()
            .px(px(8.))
            .pt(px(8.))
            .pb(px(4.))
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(avatar.build())
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .items_start()
                            .gap(px(4.))
                            .child(
                                button("own-picture", ButtonKind::Secondary, picture)
                                    .h(px(24.))
                                    .on_click(cx.listener(|this, _, _, cx| this.pick_picture(cx))),
                            )
                            .when(chosen, |column| {
                                column.child(button("own-picture-remove", ButtonKind::Ghost, "Use the accounts' picture").h(px(24.)).on_click(
                                    cx.listener(|this, _, _, cx| this.set_me(Me { picture: String::new(), ..prefs::me() }, cx)),
                                ))
                            }),
                    ),
            )
            .child(field(focused).child(div().flex_1().min_w_0().child(self.name.clone())))
            .child(
                div()
                    .type_small()
                    .text_color(theme::text_faint())
                    .child("Only on this device. Each network still shows the name and picture you have there."),
            )
            .children(self.refused.clone().map(|words| div().type_small().text_color(theme::danger()).child(words)))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(4.))
                    .child(
                        button("own-edit-cancel", ButtonKind::Ghost, "Cancel")
                            .h(px(24.))
                            .on_click(cx.listener(|this, _, _, cx| this.finish(false, cx))),
                    )
                    .child(
                        button("own-edit-done", ButtonKind::Primary, "Done")
                            .h(px(24.))
                            .on_click(cx.listener(|this, _, _, cx| this.finish(true, cx))),
                    ),
            )
    }
}

impl Render for StatusMenu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Our name and picture come from our own person, who is watched while the menu is up.
        self.hub.watch(SOURCE, own_keys(&self.scene), cx);
        let head = if self.editing { self.editor(window, cx) } else { self.head(cx) };
        let phone = crate::gui::phone::active();
        let mut list = div().when(!phone, |list| list.w(px(WIDTH))).when(phone, |list| list.w_full()).flex().flex_col().child(head).child(menu_label("Accounts"));
        let accounts = self.scene.accounts.clone();
        for (ix, own) in accounts.iter().enumerate() {
            list = list.child(self.account_row(ix, own, cx));
            if self.open.as_ref() == Some(&own.key) {
                list = list.child(self.choices(ix, own, window, cx));
            }
        }
        // While editing, a refusal is shown in the editor.
        let refused = self.refused.clone().filter(|_| !self.editing);
        list.children(refused.map(|words| {
            div().px(px(8.)).pt(px(4.)).pb(px(4.)).type_small().text_color(theme::danger()).child(words)
        }))
        .child(menu_separator())
        .child(
            menu_item(
                "own-settings",
                Some("icons/settings.svg"),
                "Settings…",
                Some("⌘,"),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                let _ = this.shell.update(cx, |shell, cx| shell.close_menu(cx));
                crate::gui::settings::open(crate::gui::settings::Section::last(), cx);
            })),
        )
    }
}

#[cfg(feature = "gui-shot")]
impl StatusMenu {
    /// The first settable account opened, with words in its field.
    pub fn stage(&mut self, text: &str, cx: &mut Context<Self>) {
        let first = connected(&self.scene).find(|own| has_status_text(own.key.network, &own.can) && !states(own.key.network, &own.can).is_empty());
        self.open = first.map(|own| own.key.clone());
        self.field.update(cx, |field, cx| field.set_text(text, cx));
        cx.notify();
    }

    /// The head editing, with a name typed.
    pub fn stage_edit(&mut self, name: &str, cx: &mut Context<Self>) {
        self.name.update(cx, |field, cx| field.set_text(name, cx));
        self.editing = true;
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn own(network: Network, account: &str, name: &str, link: Link, presence: Presence) -> Own {
        Own {
            key: AccountKey { network, account: account.into() },
            name: name.into(),
            link,
            reconnecting: false,
            me: "me".into(),
            presence,
            status: String::new(),
            avatar: None,
            can: protocol::Capabilities::default(),
        }
    }

    #[test]
    fn the_most_available_state_among_connected_accounts_wins() {
        use Presence::{Dnd, Idle, Offline, Online, Unknown};
        assert_eq!(most_available([Offline, Dnd, Idle, Unknown]), Idle);
        assert_eq!(most_available([Dnd, Online]), Online);
        assert_eq!(most_available([Unknown, Offline]), Offline);
        assert_eq!(most_available([]), Unknown);

        // A dropped account's last state does not count.
        let scene = Scene {
            accounts: vec![
                own(Network::Discord, "1", "ines", Link::Offline, Online),
                own(Network::Slack, "T1", "Northwind", Link::Connected, Idle),
                own(Network::Slack, "T2", "Riverbend", Link::Connected, Dnd),
            ],
            ..Scene::default()
        };
        assert_eq!(most_available(connected(&scene).map(|own| own.presence)), Idle);
        assert_eq!(summary(&scene), "Idle");
    }

    #[test]
    fn every_account_row_says_where_it_stands() {
        let word = |own: Own| link_word(&own).0;
        assert_eq!(word(own(Network::Discord, "1", "", Link::Connected, Presence::Dnd)), "Do not disturb");
        assert_eq!(word(own(Network::Discord, "1", "", Link::Connected, Presence::Offline)), "Invisible");
        // Before the network says our state, and on networks with none, it is still connected.
        assert_eq!(word(own(Network::Discord, "1", "", Link::Connected, Presence::Unknown)), "Connected");
        assert_eq!(word(own(Network::Signal, "1", "", Link::Connected, Presence::Unknown)), "Connected");
        assert_eq!(word(own(Network::Tiny, "1", "", Link::Connected, Presence::Online)), "Online");
        assert_eq!(word(own(Network::Telegram, "1", "", Link::Connected, Presence::Unknown)), "Online while open");
        assert_eq!(word(own(Network::Slack, "T1", "", Link::Connected, Presence::Idle)), "Away");
        assert_eq!(word(own(Network::Slack, "T1", "", Link::Offline, Presence::Online)), "Offline");
        assert_eq!(word(Own { reconnecting: true, ..own(Network::Slack, "T1", "", Link::Offline, Presence::Online) }), "Reconnecting…");
    }

    #[test]
    fn a_network_lists_only_the_states_it_has() {
        let bare = protocol::Capabilities::default();
        let tiny_presence = protocol::Capabilities { presence_status: true, ..protocol::Capabilities::default() };
        assert_eq!(states(Network::Slack, &bare), [Presence::Online, Presence::Idle]);
        assert_eq!(states(Network::Discord, &bare).len(), 4);
        assert!(states(Network::Telegram, &bare).is_empty());
        assert!(states(Network::Tiny, &bare).is_empty());
        assert!(!has_status_text(Network::Tiny, &bare));
        assert_eq!(states(Network::Tiny, &tiny_presence), [Presence::Online, Presence::Idle, Presence::Dnd, Presence::Offline]);
        assert!(has_status_text(Network::Tiny, &tiny_presence));
        assert_eq!(state_word(Presence::Idle, Network::Slack), "Away");
        assert_eq!(state_word(Presence::Offline, Network::Discord), "Invisible");
        assert_eq!(state_word(Presence::Offline, Network::Tiny), "Invisible");
        assert_eq!(state_word(Presence::Offline, Network::Slack), "Offline");
    }

    #[test]
    fn an_account_is_named_by_its_workspace_only_where_a_network_has_several() {
        let accounts = [
            own(Network::Discord, "1", "ines", Link::Connected, Presence::Online),
            own(Network::Slack, "T1", "Northwind", Link::Connected, Presence::Online),
            own(Network::Slack, "T2", "Riverbend", Link::Connected, Presence::Online),
        ];
        assert_eq!(account_label(&accounts, &accounts[0]), "Discord");
        assert_eq!(account_label(&accounts, &accounts[1]), "Northwind");
        assert_eq!(account_label(&accounts[..2], &accounts[1]), "Slack");
    }

    #[test]
    fn dnd_presets_compute_future_snooze_end_times() {
        let now = 1_800_000_000_i64;
        assert_eq!(DndPreset::Minutes30.until(now), now + 1800);
        assert_eq!(DndPreset::Hour1.until(now), now + 3600);
        assert_eq!(DndPreset::Hours2.until(now), now + 7200);
        let tomorrow = DndPreset::UntilTomorrow.until(now);
        assert!(tomorrow >= now + 3600 && tomorrow <= now + 86_400);
    }
}
