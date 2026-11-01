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
/// other; Slack has two.
pub fn states(network: Network) -> &'static [Presence] {
    match network {
        Network::Discord => &[Presence::Online, Presence::Idle, Presence::Dnd, Presence::Offline],
        Network::Teams => &[Presence::Online, Presence::Idle, Presence::Busy, Presence::Dnd, Presence::Offline],
        Network::Slack => &[Presence::Online, Presence::Idle],
        // Signal shows no presence at all.
        Network::Telegram | Network::Signal | Network::Messages | Network::Tiny => &[],
    }
}

fn has_status_text(network: Network) -> bool {
    !matches!(network, Network::Telegram | Network::Signal | Network::Messages | Network::Tiny)
}

/// The word for a state of our own: choosing to look offline has the network's name for it.
pub fn state_word(presence: Presence, network: Network) -> &'static str {
    match (presence, network) {
        (Presence::Offline, Network::Discord) => "Invisible",
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

pub struct StatusMenu {
    shell: WeakEntity<Shell>,
    hub: Hub,
    scene: Scene,
    /// The account whose states and status text are showing.
    open: Option<AccountKey>,
    field: Entity<TextInput>,
    refused: Option<String>,
    /// The head is editing our name and picture on this device.
    editing: bool,
    name: Entity<TextInput>,
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
        StatusMenu { shell, hub, scene: Scene::default(), open: None, field, refused: None, editing: false, name }
    }

    /// The menu opened again: it starts as it was first seen, nothing half edited.
    pub fn opened(&mut self, cx: &mut Context<Self>) {
        self.editing = false;
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
        if self.open.as_ref() == Some(account) {
            self.open = None;
        } else {
            let said = self.own(account).map(|own| own.status.clone()).unwrap_or_default();
            self.field.update(cx, |field, cx| field.set_text(&said, cx));
            self.open = Some(account.clone());
        }
        cx.notify();
    }

    fn send(&mut self, account: AccountKey, presence: Presence, status: Option<String>, cx: &mut Context<Self>) {
        self.refused = None;
        cx.notify();
        if only_draws() {
            return;
        }
        let this = cx.weak_entity();
        // chatd reports the new state as a `Status` change, which is what redraws the rows.
        super::ask_later(cx, Request::SetPresence { account, presence, status }, move |answer, cx| {
            let refused = match answer {
                Ok(Response::Error { detail, .. }) => Some(detail),
                Ok(_) => None,
                Err(err) => Some(crate::client::trouble_words("people", &err)),
            };
            let _ = this.update(cx, |this, cx| {
                this.refused = refused;
                cx.notify();
            });
        });
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
        self.send(own.key, own.presence, Some(text), cx);
    }

    fn account_row(&self, ix: usize, own: &Own, cx: &mut Context<Self>) -> Stateful<Div> {
        let network = own.key.network;
        let settable = own.link == Link::Connected && !states(network).is_empty();
        let open = self.open.as_ref() == Some(&own.key);
        let (word, color) = link_word(own);
        let shape = Shape::of(own.presence).filter(|_| own.link == Link::Connected);
        let account = own.key.clone();
        div()
            .id(("own-account", ix))
            .h(px(if crate::gui::phone::active() { theme::row_height(theme::ROW_ONE) } else { theme::ROW_ONE }))
            .px(px(8.))
            .rounded(px(theme::R_CONTROL))
            .flex()
            .items_center()
            .gap(px(8.))
            .child(network_icon(network, 16.))
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
        for (at, state) in states(network).iter().copied().enumerate() {
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
                    .on_click(cx.listener(move |this, _, _, cx| this.send(account.clone(), state, None, cx))),
            );
        }
        if !has_status_text(network) {
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
                    this.say(cx);
                }))
                .child(glyph("icons/x.svg", 14., theme::text_soft()))
        });
        let unsent = self.field.read(cx).text().trim() != own.status.trim();
        choices.child(
            div().mt(px(4.)).px(px(8.)).flex().flex_col().gap(px(4.)).child(
                field(focused)
                    .line_height(px(18.))
                    .child(glyph("icons/smile.svg", 14., theme::text_faint()))
                    .child(div().flex_1().min_w_0().child(self.field.clone()))
                    .children(clear),
            )
            .when(unsent, |status| {
                status.child(div().type_small().text_color(theme::text_faint()).child(crate::gui::shortcuts::label("Return sets it.")))
            }),
        )
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
        let (name, avatar) = self.identity(32., cx);
        let ring = presence::Ring::on(theme::overlay());
        let best = most_available(connected(&self.scene).map(|own| own.presence));
        div()
            .h(px(theme::ROW_TWO))
            .pl(px(8.))
            .pr(px(4.))
            .flex()
            .items_center()
            .gap(px(10.))
            .child(avatar.build().children(Shape::of(best).and_then(|shape| presence::dot(shape, 32., &ring))))
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
        let first = connected(&self.scene).find(|own| has_status_text(own.key.network) && !states(own.key.network).is_empty());
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
        assert_eq!(states(Network::Slack), [Presence::Online, Presence::Idle]);
        assert_eq!(states(Network::Discord).len(), 4);
        assert!(states(Network::Telegram).is_empty());
        assert_eq!(state_word(Presence::Idle, Network::Slack), "Away");
        assert_eq!(state_word(Presence::Offline, Network::Discord), "Invisible");
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
}
