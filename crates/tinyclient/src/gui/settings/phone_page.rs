//! Settings on a phone: not a second window but a page of the one window, pushed over the tabs.
//! First the list of sections (the desktop's list, with a search field on top), then one section
//! at a time with a way back. The sections and their rows are the desktop's own
//! (`pages.rs`); on a phone each row is 52 pt or taller, wide controls go under their label, and
//! a question rises from the bottom instead of floating in the middle.
//!
//! The layout draws it with [`phone_page`] and hands the back gesture to [`phone_back`] first.

use super::{on_phone, Dismiss, Find, Host, Section, SettingsWindow};
use crate::gui::components::{button, glyph, ButtonKind};
use crate::gui::phone::tokens::{self, PhoneType};
use crate::gui::theme;
use gpui::{div, prelude::*, px, AnyElement, App, Context, Div, Focusable, SharedString, Stateful, Window};

/// What the page was asked to show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Wanted {
    List,
    Section(Section),
}

/// Asks for the page; it is made or moved the next time the window draws.
pub(super) fn want(wanted: Wanted, cx: &mut App) {
    cx.default_global::<Host>().wanted = Some(wanted);
    cx.refresh_windows();
}

/// On a phone: Settings on its list of sections. (On the desktop, the window on the section last
/// used.)
pub fn open_list(cx: &mut App) {
    if crate::gui::phone::active() {
        want(Wanted::List, cx);
    } else {
        super::open(Section::last(), cx);
    }
}

/// Whether the Settings page is up on a phone.
pub fn phone_shown(cx: &App) -> bool {
    cx.try_global::<Host>().and_then(|host| host.page.as_ref()).is_some_and(|page| page.read(cx).shown)
}

/// The back gesture or button, on a phone: a section goes back to the list, the list closes the
/// page. `false` when there is no page up, so the layout goes back itself.
pub fn phone_back(cx: &mut App) -> bool {
    if !phone_shown(cx) {
        return false;
    }
    let page = cx.default_global::<Host>().page.clone();
    if let Some(page) = page {
        page.update(cx, |settings, cx| settings.phone_up(cx));
    }
    true
}

/// The Settings page, full screen over whatever the phone shows, while it is up; `None` on the
/// desktop and while it is closed. The layout adds it last to its root:
/// `root.children(settings::phone_page(window, cx))`.
pub fn phone_page(window: &mut Window, cx: &mut App) -> Option<AnyElement> {
    if !crate::gui::phone::active() {
        return None;
    }
    let host = cx.default_global::<Host>();
    // Made or moved after this frame: the layout calls this from the shell's own draw, and the
    // page reads the shell as it is made.
    if let Some(wanted) = host.wanted.take() {
        window.defer(cx, move |window, cx| apply(wanted, window, cx));
    }
    let page = cx.default_global::<Host>().page.clone()?;
    if !page.read(cx).shown {
        return None;
    }
    Some(div().absolute().top_0().left_0().size_full().occlude().child(page).into_any_element())
}

/// Makes the page the first time, then shows what was asked for.
fn apply(wanted: Wanted, window: &mut Window, cx: &mut App) {
    let host = cx.default_global::<Host>();
    let (shell, page) = (host.shell.clone(), host.page.clone());
    let page = match page {
        Some(page) => page,
        None => {
            let Some(shell) = shell else {
                return;
            };
            let section = match wanted {
                Wanted::Section(section) => section,
                Wanted::List => Section::last(),
            };
            let made = cx.new(|cx| SettingsWindow::new(shell, section, true, window, cx));
            cx.default_global::<Host>().page = Some(made.clone());
            made
        }
    };
    page.update(cx, |settings, cx| {
        settings.shown = true;
        match wanted {
            Wanted::Section(section) if on_phone(section) => settings.show(section, window, cx),
            _ => {
                settings.listing = true;
                cx.notify();
            }
        }
    });
    window.refresh();
}

/// A question on a phone: a sheet from the bottom over a scrim, its buttons full width.
pub(super) fn modal_sheet(title: SharedString, words: Vec<String>, action: Stateful<Div>, cx: &mut Context<SettingsWindow>) -> Div {
    let big = |button: Stateful<Div>| button.w_full().h(px(48.)).rounded(px(12.)).text_size(px(tokens::scaled(16.)));
    let panel = div()
        .w_full()
        .p(px(20.))
        .rounded(px(16.))
        .bg(theme::overlay())
        .border_1()
        .border_color(theme::overlay_edge())
        .shadow(tokens::shadow_sheet())
        .flex()
        .flex_col()
        .child(div().phone_title().text_color(theme::text()).child(title))
        .children(words.into_iter().map(|line| div().mt(px(8.)).phone_row().text_color(theme::text_soft()).child(line)))
        .child(
            div()
                .mt(px(20.))
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(big(action))
                .child(big(button("modal-cancel", ButtonKind::Secondary, "Cancel")).on_click(cx.listener(|this, _, _, cx| this.close_modal(cx)))),
        );
    sheet_layer(panel, cx)
}

/// A panel at the foot of the screen, over a scrim that cancels on a tap.
pub(super) fn sheet_layer(panel: Div, cx: &mut Context<SettingsWindow>) -> Div {
    div()
        .absolute()
        .inset_0()
        .child(
            div()
                .id("modal-scrim")
                .absolute()
                .inset_0()
                .bg(theme::scrim())
                .occlude()
                .on_click(cx.listener(|this, _, _, cx| this.close_modal(cx))),
        )
        .child(
            div()
                .absolute()
                .inset_0()
                .flex()
                .flex_col()
                .justify_end()
                .px(px(8.))
                .pb(px(tokens::safe_bottom() + 8.))
                .child(div().id("modal").w_full().occlude().on_click(|_, _, cx| cx.stop_propagation()).child(panel)),
        )
}

impl SettingsWindow {
    /// One step up: a menu or question closes, a search clears, a section goes back to the list,
    /// and the list closes the page.
    pub(super) fn phone_up(&mut self, cx: &mut Context<Self>) {
        if self.picker.take().is_some() {
        } else if self.confirm.is_some() {
            self.close_modal(cx);
        } else if !self.listing {
            self.stop_test(cx);
            self.listing = true;
        } else if !self.query.is_empty() {
            self.query.clear();
            self.search.update(cx, |input, cx| input.set_text("", cx));
        } else {
            self.stop_test(cx);
            self.shown = false;
            // The page is a child of the shell's root: the shell draws without it.
            cx.refresh_windows();
        }
        cx.notify();
    }

    /// The top bar: back, and where the page is.
    fn phone_bar(&self, cx: &mut Context<Self>) -> Div {
        let title = if self.listing { "Settings" } else { self.section.name() };
        div()
            .flex_none()
            .pt(px(tokens::safe_top()))
            .bg(theme::sidebar())
            .shadow(tokens::top_highlight())
            .border_b_1()
            .border_color(theme::hairline())
            .child(
                div()
                    .h(px(tokens::NAV))
                    .pl(px(4.))
                    .pr(px(tokens::GUTTER))
                    .flex()
                    .items_center()
                    .gap(px(2.))
                    .child(
                        div()
                            .id("settings-back")
                            .flex_none()
                            .size(px(tokens::TOUCH))
                            .rounded(px(22.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .role(gpui::Role::Button)
                            .aria_label("Back")
                            .active(|style| style.bg(theme::pressed()))
                            .child(glyph("icons/chevron-left.svg", 24., theme::text()))
                            .on_click(cx.listener(|this, _, _, cx| this.phone_up(cx))),
                    )
                    .child(div().flex_1().min_w_0().truncate().phone_title().text_color(theme::text()).child(title)),
            )
    }

    /// The search field at the top of the list.
    fn phone_search(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let focused = self.search.focus_handle(cx).is_focused(window);
        let searching = !self.query.trim().is_empty();
        div()
            .h(px(44.))
            .px(px(12.))
            .rounded(px(10.))
            .bg(theme::field())
            .border_1()
            .border_color(if focused { theme::focus() } else { theme::hairline() })
            .flex()
            .items_center()
            .gap(px(10.))
            .child(glyph("icons/search.svg", 18., theme::text_faint()))
            .child(div().flex_1().min_w_0().text_size(px(tokens::scaled(16.))).line_height(px(tokens::scaled(22.))).child(self.search.clone()))
            .when(searching, |field| {
                field.child(
                    div()
                        .id("search-clear")
                        .size(px(32.))
                        .mr(px(-6.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .child(glyph("icons/x.svg", 16., theme::text_faint()))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.query.clear();
                            this.search.update(cx, |input, cx| input.set_text("", cx));
                            cx.notify();
                        })),
                )
            })
    }

    /// One section in the list: its icon on a tile, its name, what it holds, and the chevron.
    fn phone_section(&self, section: Section, first: bool, detail: Option<String>, cx: &mut Context<Self>) -> Stateful<Div> {
        div()
            .id(section.name())
            .min_h(px(52.))
            .pl(px(tokens::GUTTER))
            .pr(px(12.))
            .flex()
            .items_center()
            .gap(px(14.))
            .cursor_pointer()
            .role(gpui::Role::Button)
            .aria_label(section.name())
            .active(|style| style.bg(theme::pressed()))
            .child(
                div()
                    .flex_none()
                    .size(px(30.))
                    .rounded(px(8.))
                    .bg(theme::selected())
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(glyph(section.icon(), 17., theme::text())),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .min_h(px(52.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .when(!first, |line| line.border_t_1().border_color(theme::hairline()))
                    .child(div().flex_1().min_w_0().truncate().phone_row().text_color(theme::text()).child(section.name()))
                    .children(detail.map(|words| div().flex_none().phone_preview().text_color(theme::text_soft()).child(words)))
                    .child(glyph("icons/chevron-right.svg", 18., theme::text_faint())),
            )
            .on_click(cx.listener(move |this, _, window, cx| this.show(section, window, cx)))
    }

    /// The list of sections, or the search's results while it has words.
    fn phone_list(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let search = self.phone_search(window, cx);
        if !self.query.trim().is_empty() {
            return div().flex().flex_col().child(search).child(div().mt(px(16.)).child(self.results(window, cx)));
        }
        let accounts = self
            .shell
            .upgrade()
            .and_then(|shell| shell.read(cx).snapshot.as_ref().map(|snapshot| snapshot.signed_in().len()))
            .filter(|count| *count > 0)
            .map(|count| count.to_string());
        let mut card = super::controls::card().overflow_hidden();
        for (ix, section) in Section::ALL.into_iter().filter(|s| on_phone(*s) && *s != Section::About).enumerate() {
            let detail = if section == Section::Accounts { accounts.clone() } else { None };
            card = card.child(self.phone_section(section, ix == 0, detail, cx));
        }
        let about = super::controls::card().overflow_hidden().child(self.phone_section(Section::About, true, Some(env!("CARGO_PKG_VERSION").to_owned()), cx));
        div()
            .flex()
            .flex_col()
            .child(search)
            .child(div().mt(px(20.)).child(card))
            .child(div().mt(px(20.)).child(about))
            .child(super::controls::caption("Calls, the window and shortcuts are set in tinyclient on a computer.").pt(px(10.)))
    }

    /// The page: the bar, then the list or one section, scrolling under it.
    pub(super) fn phone_render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        self.width = f32::from(window.viewport_size().width);
        let modal = self.confirm.clone().map(|confirm| self.modal(&confirm, cx));
        let toast = self.copied.filter(|at| at.elapsed() < std::time::Duration::from_secs(4)).map(|_| {
            div().absolute().bottom(px(tokens::safe_bottom() + 16.)).left_0().right_0().flex().justify_center().child(
                crate::gui::components::toast(Some(("icons/check.svg", theme::text_soft())), "Copied").w(px(200.)),
            )
        });
        let bar = self.phone_bar(cx);
        let body = if self.listing { self.phone_list(window, cx) } else { self.page(window, cx) };
        let bottom = tokens::safe_bottom().max(tokens::keyboard()) + 32.;
        let scroll_id = SharedString::from(if self.listing { "phone-settings-list".to_owned() } else { format!("phone-settings-{}", self.section.word()) });
        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(theme::canvas())
            .font_family(theme::FONT)
            .text_color(theme::text())
            .key_context("Settings")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &Find, window, cx| {
                let handle = this.search.focus_handle(cx);
                window.focus(&handle, cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &Dismiss, _, cx| this.phone_up(cx)))
            .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| {
                if let (Some(id), true) = (this.drag, event.dragging()) {
                    this.slide_to(id, event.position.x, cx);
                }
            }))
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if let Some(id) = this.drag.take() {
                        this.commit(id, cx);
                    }
                }),
            )
            .child(bar)
            .child(
                div()
                    .id(scroll_id)
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(div().px(px(tokens::GUTTER)).pt(px(20.)).pb(px(bottom)).child(body)),
            )
            .children(toast)
            .children(modal)
    }
}
