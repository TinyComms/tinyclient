//! Server Settings: a full-window overlay, as Discord's, over the rail and every pane. A left
//! column of pages, the page on the right, Escape or the close button to leave. A page that edits
//! keeps its changes in a draft until Save in the unsaved-changes bar; Reset puts them back.
//!
//! Each page is one function in its own file, `settings/<page>.rs`, with the same signature:
//!
//! ```ignore
//! pub(in crate::gui::tiny) fn <page>(shell: &Shell, page: &Page, window: &mut Window, cx: &mut Context<Shell>) -> AnyElement
//! ```
//!
//! A page that edits also gives `dirty`, `save` and `reset` (see `overview.rs`), which the frame
//! calls from [`Shell::tiny_settings_dirty`], [`Shell::tiny_settings_save`] and
//! [`Shell::tiny_settings_reset`]. Its draft lives in [`Overlay`] (and text fields, which need a
//! window context to make, in [`Fields`]). A page is listed only where `visible` says this
//! account may see it.

mod audit;
mod bans;
mod channels;
mod danger;
mod invites;
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
    Danger,
}

impl Page {
    pub const ALL: [Page; 10] = [Page::Overview, Page::Roles, Page::Members, Page::Invites, Page::Bans, Page::Channels, Page::Voice, Page::Audit, Page::SignIn, Page::Danger];

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
            Page::Danger => "icons/triangle-alert.svg",
        }
    }

    /// The column's group the page is under.
    fn group(self) -> &'static str {
        match self {
            Page::Overview | Page::Roles => "Server",
            Page::Members | Page::Invites | Page::Bans => "People",
            Page::Channels | Page::Voice => "Voice",
            Page::Audit | Page::SignIn | Page::Danger => "Security",
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
            Page::Danger => may.owner,
        }
    }
}

/// Server Settings while it is open: the page on show and each page's draft. The drafts of the
/// pages still to be drawn are read by nobody yet.
#[allow(dead_code)]
pub(in crate::gui) struct Overlay {
    pub page: Page,
    /// The channel the Channels page opens on, when it was opened from one.
    pub channel: Option<String>,
    pub overview: overview::Draft,
    pub roles: roles::Draft,
    pub members: members::Draft,
    pub invites: invites::Draft,
    pub bans: bans::Draft,
    pub channels: channels::Draft,
    pub voice: voice::Draft,
    pub audit: audit::Draft,
    pub signin: signin::Draft,
    pub danger: danger::Draft,
    /// A save is with the server.
    pub saving: bool,
    /// What the server said no to, in the unsaved-changes bar.
    pub error: Option<String>,
    /// Shown for a moment after a save went through.
    pub saved: bool,
}

impl Overlay {
    fn new(page: Page, channel: Option<String>) -> Overlay {
        Overlay {
            page,
            channel,
            overview: Default::default(),
            roles: Default::default(),
            members: Default::default(),
            invites: Default::default(),
            bans: Default::default(),
            channels: Default::default(),
            voice: Default::default(),
            audit: Default::default(),
            signin: Default::default(),
            danger: Default::default(),
            saving: false,
            error: None,
            saved: false,
        }
    }
}

/// The text fields the pages edit with. Made once, with the shell.
pub(in crate::gui) struct Fields {
    pub name: Entity<TextInput>,
    pub description: Entity<TextInput>,
    pub welcome: Entity<TextInput>,
}

impl Fields {
    pub fn new(cx: &mut Context<Shell>) -> Fields {
        let name = cx.new(|cx| TextInput::new("Server name", cx));
        let description = cx.new(|cx| TextInput::multiline("What this server is, in a sentence or two", cx));
        let welcome = cx.new(|cx| TextInput::multiline("What new members read on the server's home", cx));
        for input in [&name, &description, &welcome] {
            cx.subscribe(input, |_, _, _: &crate::gui::input::InputEvent, cx| cx.notify()).detach();
            // Escape in a field leaves Server Settings, as it does anywhere else in it.
            cx.subscribe(input, |this, _, key: &crate::gui::input::InputKey, cx| {
                if *key == crate::gui::input::InputKey::Escape {
                    this.tiny_escape(cx);
                }
            })
            .detach();
        }
        Fields { name, description, welcome }
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

/// What a page that is still to be built says. Its function is in place for whoever builds it.
pub(in crate::gui::tiny) fn coming(page: Page, what: &'static str) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(24.))
        .child(page_head(page.label(), what))
        .child(
            div()
                .p(px(20.))
                .rounded(px(theme::R_CARD))
                .border_1()
                .border_color(theme::border())
                .border_dashed()
                .flex()
                .items_center()
                .gap(px(12.))
                .child(glyph(page.icon(), 20., theme::text_faint()))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(div().type_ui().font_weight(theme::SEMIBOLD).text_color(theme::text()).child("Coming in this branch"))
                        .child(div().type_small().text_color(theme::text_soft()).child("This page is drawn next; its place and its requests are ready.")),
                ),
        )
        .into_any_element()
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
        self.tiny_overview_reset(cx);
        cx.notify();
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
        }
        cx.notify();
    }

    /// The page on show has changes not yet saved.
    pub(in crate::gui) fn tiny_settings_dirty(&self, cx: &gpui::App) -> bool {
        let Some(overlay) = self.tiny_state.settings.as_ref() else {
            return false;
        };
        match overlay.page {
            Page::Overview => self.tiny_overview_dirty(cx),
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
        if page == Page::Overview {
            self.tiny_overview_save(cx);
        }
    }

    pub(in crate::gui) fn tiny_settings_reset(&mut self, cx: &mut Context<Self>) {
        let Some(page) = self.tiny_state.settings.as_ref().map(|overlay| overlay.page) else {
            return;
        };
        if let Some(overlay) = self.tiny_state.settings.as_mut() {
            overlay.error = None;
        }
        if page == Page::Overview {
            self.tiny_overview_reset(cx);
        }
        cx.notify();
    }

    /// The first read of a server: the fields start from what it holds.
    pub(in crate::gui::tiny) fn tiny_settings_loaded(&mut self, cx: &mut Context<Self>) {
        if self.tiny_state.settings.is_some() {
            self.tiny_overview_reset(cx);
        }
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
            .child(div().id("tiny-settings-scroll").size_full().overflow_y_scroll().child(div().w_full().max_w(px(740.)).px(px(40.)).pt(px(48.)).pb(px(120.)).child(body)))
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
