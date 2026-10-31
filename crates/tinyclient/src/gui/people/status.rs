//! Our own status, in the popover of the rail's avatar: each account's link, how others see us
//! there, and setting it. One account is open at a time, showing the states its network has and
//! the status text.

use super::presence::{self, Shape};
use super::{only_draws, Hub, Own, Scene};
use crate::gui::components::{
    field, glyph, menu_item, menu_label, menu_separator, network_icon, Avatar,
};
use crate::gui::input::{InputEvent, InputKey, TextInput};
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use gpui::{div, prelude::*, px, Context, Div, Entity, Focusable, Stateful, WeakEntity, Window};
use protocol::{AccountKey, Key, Link, Network, Presence, Request, Response};

pub const SOURCE: &str = "own";
const WIDTH: f32 = 288.;

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
        StatusMenu { shell, hub, scene: Scene::default(), open: None, field, refused: None }
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
        let (word, color) = match (own.link, network) {
            (Link::Connected, Network::Telegram) => ("Online while open", theme::text_soft()),
            (Link::Connected, _) => (state_word(own.presence, network), theme::text_soft()),
            (Link::Offline, _) => ("Offline", theme::warning()),
            (Link::Expired, _) => ("Signed out by the network", theme::danger()),
            (Link::SignedOut, _) => ("Signed out", theme::text_soft()),
        };
        let shape = Shape::of(own.presence).filter(|_| own.link == Link::Connected);
        let account = own.key.clone();
        div()
            .id(("own-account", ix))
            .h(px(theme::ROW_ONE))
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
                    .h(px(theme::ROW_ONE))
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

impl Render for StatusMenu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (scene, hub) = (&self.scene, &self.hub);
        // Our name and picture come from our own person, who is watched while the menu is up.
        hub.watch(SOURCE, own_keys(scene), cx);
        let me = own_keys(scene).iter().find_map(|own| hub.person(own));
        // An account that is a person names us; a workspace's name is not ours.
        let personal = scene
            .accounts
            .iter()
            .find(|own| matches!(own.key.network, Network::Discord | Network::Telegram) && !own.name.is_empty());
        let name = match (&me, personal) {
            (Some(me), _) => me.name.clone(),
            (None, Some(own)) => own.name.clone(),
            (None, None) => "You".to_owned(),
        };
        let pictured = connected(scene).find(|own| own.avatar.is_some() && !own.me.is_empty());
        let avatar = match (&me, pictured) {
            (Some(me), _) if me.avatar.is_some() => {
                let picture = hub.avatar(&me.key.account(), me.avatar.as_ref(), cx);
                Avatar::new(&me.key, &me.name, 32.).image(picture.as_ref())
            }
            (_, Some(own)) => {
                let picture = hub.avatar(&own.key, own.avatar.as_ref(), cx);
                Avatar::new(&Key::new(&own.key, own.me.clone()), &name, 32.).image(picture.as_ref())
            }
            (Some(me), None) => Avatar::new(&me.key, &me.name, 32.),
            (None, None) => Avatar::named(&name, 32.),
        };
        let ring = presence::Ring::on(theme::overlay());
        let best = most_available(connected(scene).map(|own| own.presence));
        let mut list = div()
            .w(px(WIDTH))
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(theme::ROW_TWO))
                    .px(px(8.))
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
                            .child(div().truncate().type_small().text_color(theme::text_soft()).child(summary(scene))),
                    ),
            )
            .child(menu_label("Accounts"));
        let accounts = self.scene.accounts.clone();
        for (ix, own) in accounts.iter().enumerate() {
            list = list.child(self.account_row(ix, own, cx));
            if self.open.as_ref() == Some(&own.key) {
                list = list.child(self.choices(ix, own, window, cx));
            }
        }
        list.children(self.refused.clone().map(|words| {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn own(network: Network, account: &str, name: &str, link: Link, presence: Presence) -> Own {
        Own {
            key: AccountKey { network, account: account.into() },
            name: name.into(),
            link,
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
