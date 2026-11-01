//! Server Settings: a full-window overlay, as Discord's, over the rail and every pane. A left
//! column of pages, the page on the right, Escape or the close button to leave. A page that edits
//! keeps its changes in a draft until Save in the unsaved-changes bar; Reset puts them back.
//!
//! Each page is one function in its own file, `settings/<page>.rs`, with the same signature:
//!
//! ```ignore
//! pub(in crate::gui::tiny) fn <page>(shell: &Shell, facts: &Facts, window: &mut Window, cx: &mut Context<Shell>) -> AnyElement
//! ```
//!
//! A page that edits also gives `dirty`, `save` and `reset` (Overview, Roles, Channels), which the
//! frame calls from [`Shell::tiny_settings_dirty`], [`Shell::tiny_settings_save`] and
//! [`Shell::tiny_settings_reset`]. Its draft lives in [`Overlay`] (and text fields, which need a
//! window context to make, in [`Fields`]). What goes at once (a role made, a new order, a revoke)
//! is sent from the page. A page is listed only where `visible` says this account may see it.
//! The pieces the pages share are in `kit`.

mod audit;
mod bans;
mod channels;
mod danger;
mod encryption;
mod invites;
mod kit;
mod members;
mod overview;
mod roles;
mod signin;
mod voice;

use super::Page as Facts;
use crate::gui::chrome;
use crate::gui::components::{button, button_disabled, glyph, ButtonKind};
use crate::gui::input::TextInput;
use crate::gui::state::Shell;
use crate::gui::theme::{self, Type};
use gpui::{div, prelude::*, px, AnyElement, Context, Div, Entity, SharedString, Window};

pub(in crate::gui) use bans::banned_name;
#[cfg(feature = "gui-shot")]
pub(in crate::gui) use channels::Sel;
pub(in crate::gui) use danger::confirmed;
pub(in crate::gui) use invites::revocable;
#[cfg(feature = "gui-shot")]
pub(in crate::gui) use members::selectable as tickable;

/// The roles' ids, most senior first: what `--tiny-pick` counts in.
#[cfg(feature = "gui-shot")]
pub(in crate::gui) fn roles_in_order(community: &protocol::TinyCommunity) -> Vec<String> {
    roles::ordered(community).into_iter().map(|role| role.id.clone()).collect()
}

/// The pages, in the order the column lists them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::gui) enum Page {
    Overview,
    Roles,
    Members,
    Invites,
    Bans,
    Channels,
    Voice,
    Audit,
    SignIn,
    Encryption,
    Danger,
}

impl Page {
    pub const ALL: [Page; 11] = [Page::Overview, Page::Roles, Page::Members, Page::Invites, Page::Bans, Page::Channels, Page::Voice, Page::Audit, Page::SignIn, Page::Encryption, Page::Danger];

    pub fn label(self) -> &'static str {
        match self {
            Page::Overview => "Overview",
            Page::Roles => "Roles",
            Page::Members => "Members",
            Page::Invites => "Invites",
            Page::Bans => "Bans",
            Page::Channels => "Channels",
            Page::Voice => "Voice & media",
            Page::Audit => "Audit log",
            Page::SignIn => "Sign-in",
            Page::Encryption => "Encryption",
            Page::Danger => "Danger zone",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Page::Overview => "icons/server.svg",
            Page::Roles => "icons/shield.svg",
            Page::Members => "icons/users.svg",
            Page::Invites => "icons/user-plus.svg",
            Page::Bans => "icons/ban.svg",
            Page::Channels => "icons/volume-2.svg",
            Page::Voice => "icons/signal.svg",
            Page::Audit => "icons/list.svg",
            Page::SignIn => "icons/key.svg",
            Page::Encryption => "icons/lock.svg",
            Page::Danger => "icons/triangle-alert.svg",
        }
    }

    /// The column's group the page is under.
    fn group(self) -> &'static str {
        match self {
            Page::Overview | Page::Roles => "Server",
            Page::Members | Page::Invites | Page::Bans => "People",
            Page::Channels | Page::Voice => "Voice",
            Page::Audit | Page::SignIn | Page::Encryption | Page::Danger => "Security",
        }
    }

    /// Whether this account sees the page. Overview is for everyone, read-only to those who
    /// can't change it; the others need what they manage, and a server new enough for them.
    pub fn visible(self, facts: &Facts) -> bool {
        let community = facts.community;
        let may = facts.may;
        let can = |id: &str| community.allowed(id);
        match self {
            Page::Overview => true,
            Page::Roles => community.has(protocol::TINY_FEATURE_ROLES) && may.manage_roles,
            Page::Members => may.manage_roles || can("community.members.remove") || can("community.members.timeout") || can("community.members.ban"),
            Page::Invites => may.read_invites,
            Page::Bans => may.read_bans,
            Page::Channels => may.make_room || may.edit_room || may.delete_room,
            Page::Voice => may.edit_server,
            Page::Audit => may.read_audit,
            Page::SignIn => may.edit_server || may.owner,
            // Everyone's own devices and the people they talk to: not a power over the server.
            Page::Encryption => community.has(protocol::TINY_FEATURE_E2EE_TEXT),
            Page::Danger => may.owner,
        }
    }
}

/// Server Settings while it is open: the page on show and each page's draft.
pub(in crate::gui) struct Overlay {
    pub page: Page,
    /// The channel the Channels page opens on, when it was opened from one.
    pub channel: Option<String>,
    pub overview: overview::Draft,
    pub roles: roles::Draft,
    pub members: members::Draft,
    pub channels: channels::Draft,
    pub audit: audit::Draft,
    pub danger: danger::Draft,
    pub encryption: encryption::Draft,
    /// A save is with the server.
    pub saving: bool,
    /// What the server said no to, in the unsaved-changes bar.
    pub error: Option<String>,
    /// Shown for a moment after a save went through.
    pub saved: bool,
    /// The page's scroll: each page opens at its top.
    pub scroll: gpui::ScrollHandle,
}

impl Overlay {
    fn new(page: Page, channel: Option<String>) -> Overlay {
        Overlay {
            page,
            channel,
            overview: Default::default(),
            roles: Default::default(),
            members: Default::default(),
            channels: Default::default(),
            audit: Default::default(),
            danger: Default::default(),
            encryption: Default::default(),
            saving: false,
            error: None,
            saved: false,
            scroll: gpui::ScrollHandle::new(),
        }
    }
}

/// The text fields the pages edit with. Made once, with the shell.
pub(in crate::gui) struct Fields {
    pub name: Entity<TextInput>,
    pub description: Entity<TextInput>,
    pub welcome: Entity<TextInput>,
    /// Images and GIFs, as mebibytes, or the current byte count when that is not a whole mebibyte.
    pub image_max: Entity<TextInput>,
    /// Video and every other file, in the same form as `image_max`.
    pub file_max: Entity<TextInput>,
    pub role_name: Entity<TextInput>,
    pub role_color: Entity<TextInput>,
    pub member_search: Entity<TextInput>,
    pub ban_search: Entity<TextInput>,
    pub room_name: Entity<TextInput>,
    pub room_topic: Entity<TextInput>,
    pub category_name: Entity<TextInput>,
    /// What a dangerous modal asks to be typed.
    pub confirm: Entity<TextInput>,
    /// A new device's link code, on the Encryption page.
    pub link_code: Entity<TextInput>,
    /// A recovery key typed to check it.
    pub recovery_check: Entity<TextInput>,
}

impl Fields {
    pub fn new(cx: &mut Context<Shell>) -> Fields {
        let name = cx.new(|cx| TextInput::new("Server name", cx));
        let description = cx.new(|cx| TextInput::multiline("What this server is, in a sentence or two", cx));
        let welcome = cx.new(|cx| TextInput::multiline("What new members read on the server's home", cx));
        let image_max = cx.new(|cx| TextInput::new("MiB", cx));
        let file_max = cx.new(|cx| TextInput::new("MiB", cx));
        let role_name = cx.new(|cx| TextInput::new("Role name", cx));
        let role_color = cx.new(|cx| TextInput::new("#RRGGBB", cx));
        let member_search = cx.new(|cx| TextInput::new("Search by name or email", cx));
        let ban_search = cx.new(|cx| TextInput::new("Search bans", cx));
        let room_name = cx.new(|cx| TextInput::new("Channel name", cx));
        let room_topic = cx.new(|cx| TextInput::new("What the channel is for", cx));
        let category_name = cx.new(|cx| TextInput::new("Category name", cx));
        let confirm = cx.new(|cx| TextInput::new("Type the server's name", cx));
        let link_code = cx.new(|cx| TextInput::new("tiny-device-…", cx));
        let recovery_check = cx.new(|cx| TextInput::new("Type your recovery key to check it", cx));
        for input in [&name, &description, &welcome, &image_max, &file_max, &role_name, &role_color, &member_search, &ban_search, &room_name, &room_topic, &category_name, &confirm, &link_code, &recovery_check] {
            cx.subscribe(input, |_, _, _: &crate::gui::input::InputEvent, cx| cx.notify()).detach();
            // Escape in a field leaves Server Settings, as it does anywhere else in it.
            cx.subscribe(input, |this, _, key: &crate::gui::input::InputKey, cx| {
                if *key == crate::gui::input::InputKey::Escape {
                    this.tiny_escape(cx);
                }
            })
            .detach();
        }
        Fields { name, description, welcome, image_max, file_max, role_name, role_color, member_search, ban_search, room_name, room_topic, category_name, confirm, link_code, recovery_check }
    }
}

/// A page's title and the sentence under it.
pub(in crate::gui::tiny) fn page_head(title: &'static str, about: &'static str) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(div().type_headline().text_color(theme::text()).child(title))
        .when(!about.is_empty(), |head| head.child(div().whitespace_normal().type_ui().text_color(theme::text_soft()).child(about)))
}

/// A labelled section of a page.
pub(in crate::gui::tiny) fn section(label: &'static str) -> Div {
    div().flex().flex_col().gap(px(8.)).child(div().type_micro().text_color(theme::text_faint()).child(label.to_uppercase()))
}

/// The unsaved-changes bar at the foot of an edited page.
fn unsaved_bar(saving: bool, error: Option<String>, cx: &mut Context<Shell>) -> Div {
    let save = if saving {
        button_disabled("Saving…").into_any_element()
    } else {
        button("tiny-settings-save", ButtonKind::Primary, "Save changes").on_click(cx.listener(|this, _, _, cx| this.tiny_settings_save(cx))).into_any_element()
    };
    let (words, tone): (SharedString, _) = match error {
        Some(detail) => (detail.into(), theme::danger()),
        None => ("You have unsaved changes.".into(), theme::text()),
    };
    div()
        .w_full()
        .max_w(px(680.))
        .h(px(52.))
        .pl(px(16.))
        .pr(px(10.))
        .rounded(px(theme::R_FLOAT))
        .bg(theme::overlay())
        .border_1()
        .border_color(theme::overlay_edge())
        .shadow(theme::shadow_modal())
        .flex()
        .items_center()
        .gap(px(8.))
        .child(div().flex_1().min_w_0().truncate().type_ui().font_weight(theme::MEDIUM).text_color(tone).child(words))
        .child(button("tiny-settings-reset", ButtonKind::Ghost, "Reset").on_click(cx.listener(|this, _, _, cx| this.tiny_settings_reset(cx))))
        .child(save)
}

impl Shell {
    /// Opens Server Settings at `page`, from the header menu or a channel's Edit.
    pub(in crate::gui) fn tiny_open_settings(&mut self, page: Page, channel: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let _ = window;
        self.tiny_state.float = None;
        self.tiny_state.sub = None;
        self.tiny_state.modal = None;
        self.tiny_state.card = None;
        self.tiny_state.settings = Some(Overlay::new(page, channel));
        self.tiny_settings_reset_all(cx);
        self.tiny_settings_refreshed(cx);
        self.tiny_settings_enter(page, cx);
        cx.notify();
    }

    /// A page comes on show: the audit reads its first page the first time.
    fn tiny_settings_enter(&mut self, page: Page, cx: &mut Context<Self>) {
        if page == Page::Audit && self.tiny_state.settings.as_ref().is_some_and(|overlay| !overlay.audit.asked) {
            self.tiny_audit_load(false, cx);
        }
        if page == Page::Encryption && self.tiny_state.settings.as_ref().is_some_and(|overlay| !overlay.encryption.asked) {
            self.tiny_e2ee_load(cx);
        }
    }

    pub(in crate::gui) fn tiny_close_settings(&mut self, cx: &mut Context<Self>) {
        self.tiny_state.settings = None;
        cx.notify();
    }

    fn tiny_settings_page(&mut self, page: Page, cx: &mut Context<Self>) {
        if let Some(overlay) = self.tiny_state.settings.as_mut() {
            overlay.page = page;
            overlay.error = None;
            overlay.saved = false;
            overlay.scroll.set_offset(gpui::point(px(0.), px(0.)));
        }
        // What was typed on the page left is put back: a page's changes don't follow to another.
        self.tiny_settings_reset_all(cx);
        self.tiny_settings_enter(page, cx);
        cx.notify();
    }

    fn tiny_settings_reset_all(&mut self, cx: &mut Context<Self>) {
        self.tiny_overview_reset(cx);
        self.tiny_roles_reset(cx);
        self.tiny_channels_reset(cx);
    }

    /// The page on show has changes not yet saved.
    pub(in crate::gui) fn tiny_settings_dirty(&self, cx: &gpui::App) -> bool {
        let Some(overlay) = self.tiny_state.settings.as_ref() else {
            return false;
        };
        match overlay.page {
            Page::Overview => self.tiny_overview_dirty(cx),
            Page::Roles => self.tiny_roles_dirty(cx),
            Page::Channels => self.tiny_channels_dirty(cx),
            _ => false,
        }
    }

    pub(in crate::gui) fn tiny_settings_save(&mut self, cx: &mut Context<Self>) {
        let Some(page) = self.tiny_state.settings.as_ref().map(|overlay| overlay.page) else {
            return;
        };
        if let Some(overlay) = self.tiny_state.settings.as_mut() {
            overlay.error = None;
        }
        match page {
            Page::Overview => self.tiny_overview_save(cx),
            Page::Roles => self.tiny_roles_save(cx),
            Page::Channels => self.tiny_channels_save(cx),
            _ => {}
        }
    }

    pub(in crate::gui) fn tiny_settings_reset(&mut self, cx: &mut Context<Self>) {
        let Some(page) = self.tiny_state.settings.as_ref().map(|overlay| overlay.page) else {
            return;
        };
        if let Some(overlay) = self.tiny_state.settings.as_mut() {
            overlay.error = None;
        }
        match page {
            Page::Overview => self.tiny_overview_reset(cx),
            Page::Roles => self.tiny_roles_reset(cx),
            Page::Channels => self.tiny_channels_reset(cx),
            _ => {}
        }
        cx.notify();
    }

    /// The first read of a server: the fields start from what it holds.
    pub(in crate::gui::tiny) fn tiny_settings_loaded(&mut self, cx: &mut Context<Self>) {
        if self.tiny_state.settings.is_some() {
            self.tiny_settings_reset_all(cx);
        }
    }

    /// Every read of the server: what each page picked is checked against it (a role or a
    /// channel gone is let go of, one just made is picked).
    pub(in crate::gui::tiny) fn tiny_settings_refreshed(&mut self, cx: &mut Context<Self>) {
        if self.tiny_state.settings.is_none() {
            return;
        }
        self.tiny_roles_refreshed(cx);
        self.tiny_channels_refreshed(cx);
        self.tiny_members_refreshed();
        self.tiny_danger_refreshed();
    }

    /// A save went through: the draft is what the server holds now.
    pub(in crate::gui::tiny) fn tiny_settings_saved(&mut self, cx: &mut Context<Self>) {
        let Some(overlay) = self.tiny_state.settings.as_mut() else {
            return;
        };
        if overlay.saving {
            overlay.saving = false;
            overlay.saved = true;
            overlay.overview = Default::default();
            overlay.roles.hoist = None;
            overlay.roles.permissions = None;
            let channels = &mut overlay.channels;
            (channels.limit, channels.bitrate, channels.video, channels.ptt) = (None, None, None, None);
            channels.overrides.clear();
            let _ = cx;
        }
    }

    pub(in crate::gui::tiny) fn tiny_settings_failed(&mut self, detail: &str) {
        if let Some(overlay) = self.tiny_state.settings.as_mut().filter(|overlay| overlay.saving) {
            overlay.saving = false;
            overlay.error = Some(detail.to_owned());
        }
    }

    pub(in crate::gui::tiny) fn tiny_settings_overlay(&self, facts: &Facts, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(overlay) = self.tiny_state.settings.as_ref() else {
            return div().into_any_element();
        };
        let shown = if overlay.page.visible(facts) { overlay.page } else { Page::Overview };
        let mut nav = div()
            .w(px(232.))
            .h_full()
            .flex_none()
            .bg(theme::sidebar())
            .border_r_1()
            .border_color(theme::hairline())
            .pt(px(48.))
            .px(px(12.))
            .flex()
            .flex_col()
            .gap(px(1.))
            .child(
                div()
                    .px(px(8.))
                    .pb(px(12.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(crate::gui::components::Avatar::named(&facts.name, 24.).shape(crate::gui::components::Shape::Place).image_at(facts.icon()).build())
                    .child(div().min_w_0().truncate().type_ui().font_weight(theme::SEMIBOLD).text_color(theme::text()).child(facts.name.clone())),
            );
        let mut group = "";
        for page in Page::ALL.into_iter().filter(|page| page.visible(facts)) {
            if page.group() != group {
                group = page.group();
                nav = nav.child(div().h(px(28.)).px(px(8.)).pb(px(4.)).flex().items_end().type_micro().text_color(theme::text_faint()).child(group.to_uppercase()));
            }
            let on = page == shown;
            let danger = page == Page::Danger;
            nav = nav.child(
                div()
                    .id(SharedString::from(format!("tiny-settings-nav-{}", page.label())))
                    .h(px(32.))
                    .px(px(8.))
                    .rounded(px(theme::R_CONTROL))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .cursor_pointer()
                    .when(on, |row| row.bg(theme::selected()))
                    .when(!on, |row| row.hover(|style| style.bg(theme::hover())))
                    .on_click(cx.listener(move |this, _, _, cx| this.tiny_settings_page(page, cx)))
                    .child(glyph(page.icon(), 16., if danger { theme::danger() } else if on { theme::text() } else { theme::text_soft() }))
                    .child(div().type_ui().text_color(if danger { theme::danger() } else if on { theme::text() } else { theme::text_soft() }).child(page.label())),
            );
        }
        let body = match shown {
            Page::Overview => overview::overview(self, facts, window, cx),
            Page::Roles => roles::roles(self, facts, window, cx),
            Page::Members => members::members(self, facts, window, cx),
            Page::Invites => invites::invites(self, facts, window, cx),
            Page::Bans => bans::bans(self, facts, window, cx),
            Page::Channels => channels::channels(self, facts, window, cx),
            Page::Voice => voice::voice(self, facts, window, cx),
            Page::Audit => audit::audit(self, facts, window, cx),
            Page::SignIn => signin::signin(self, facts, window, cx),
            Page::Encryption => encryption::encryption(self, facts, window, cx),
            Page::Danger => danger::danger(self, facts, window, cx),
        };
        let dirty = self.tiny_settings_dirty(cx);
        let close = div()
            .id("tiny-settings-close")
            .flex()
            .flex_col()
            .items_center()
            .gap(px(4.))
            .cursor_pointer()
            .on_click(cx.listener(|this, _, _, cx| this.tiny_close_settings(cx)))
            .child(
                div()
                    .size(px(36.))
                    .rounded_full()
                    .border_2()
                    .border_color(theme::border_strong())
                    .hover(|style| style.bg(theme::hover()))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(glyph("icons/x.svg", 18., theme::text_soft())),
            )
            .child(div().type_micro().text_color(theme::text_faint()).child("ESC"));
        let content = div()
            .flex_1()
            .min_w_0()
            .h_full()
            .relative()
            .bg(theme::canvas())
            .child(
                div().id("tiny-settings-scroll").size_full().overflow_y_scroll().track_scroll(&overlay.scroll).child(div().w_full().max_w(px(740.)).px(px(40.)).pt(px(48.)).pb(px(120.)).child(body)),
            )
            .child(div().absolute().top(px(44.)).right(px(chrome::corner_pad(window, 28.))).child(close))
            .when(dirty || overlay.error.is_some(), |content| {
                content.child(div().absolute().bottom(px(20.)).left(px(40.)).right(px(40.)).flex().child(unsaved_bar(overlay.saving, overlay.error.clone(), cx)))
            });
        div()
            .id("tiny-settings")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .bg(theme::canvas())
            .occlude()
            // A slider's thumb follows the pointer anywhere over Server Settings while it is held.
            .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| {
                if event.pressed_button == Some(gpui::MouseButton::Left) {
                    this.tiny_channels_slide(f32::from(event.position.x), cx);
                } else {
                    this.tiny_channels_slide_end();
                }
            }))
            .on_mouse_up(gpui::MouseButton::Left, cx.listener(|this, _, _, _| this.tiny_channels_slide_end()))
            .flex()
            .font_family(theme::FONT)
            .text_color(theme::text())
            .child(nav)
            .child(content)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::Page;
    use crate::gui::tiny::{model, Page as Facts};
    use protocol::{AccountKey, Network, TinyAccess, TinyCommunity, TINY_ALL, TINY_KICK_MEMBERS, TINY_VIEW_AUDIT_LOG};

    fn shown(community: &TinyCommunity) -> Vec<&'static str> {
        let account = AccountKey { network: Network::Tiny, account: "me".into() };
        let facts = Facts { community, account: &account, may: model::may(community), live: true, me: "me", mine: None, audio: false, name: String::new() };
        Page::ALL.into_iter().filter(|page| page.visible(&facts)).map(Page::label).collect()
    }

    #[test]
    fn a_page_is_listed_only_where_the_account_may_use_it_and_the_server_has_it() {
        let legacy = |role: &str| -> TinyCommunity { serde_json::from_value(serde_json::json!({"role": role, "members": [], "invites": [], "audit": [], "me": "me"})).unwrap() };
        assert_eq!(shown(&legacy("member")), ["Overview"]);
        let admin = shown(&legacy("admin"));
        assert!(admin.contains(&"Members") && admin.contains(&"Invites") && admin.contains(&"Audit log"), "{admin:?}");
        assert!(!admin.contains(&"Roles") && !admin.contains(&"Bans") && !admin.contains(&"Danger zone"), "an older server has no roles or bans: {admin:?}");
        let mut modern = legacy("member");
        modern.capabilities.features = ["server", "roles", "categories", "bans", "timeouts", "invite_options", "audit_query", "voice_moderation", "e2ee_text"].map(String::from).to_vec();
        modern.access = Some(TinyAccess { permissions: TINY_KICK_MEMBERS | TINY_VIEW_AUDIT_LOG, ..TinyAccess::default() });
        assert_eq!(shown(&modern), ["Overview", "Members", "Audit log", "Encryption"], "a moderator sees what they moderate, and everyone sees Encryption where the server has it");
        modern.access = Some(TinyAccess { permissions: TINY_ALL, rank: 3, ..TinyAccess::default() });
        assert!(!shown(&modern).contains(&"Danger zone"), "only owners see the danger zone");
        modern.access = Some(TinyAccess { owner: true, ..TinyAccess::default() });
        assert_eq!(shown(&modern).len(), Page::ALL.len());
    }

    #[test]
    fn every_page_has_a_label_an_icon_and_a_group_and_each_group_is_together() {
        let mut seen: Vec<&str> = Vec::new();
        for page in Page::ALL {
            assert!(!page.label().is_empty());
            assert!(page.icon().starts_with("icons/"));
            if seen.last() != Some(&page.group()) {
                assert!(!seen.contains(&page.group()), "{} is apart from its group", page.label());
                seen.push(page.group());
            }
        }
    }
}
