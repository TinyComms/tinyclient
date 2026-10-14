//! The Settings window (`08-first-run-settings.md` §5 and §7): its own 720 × 560 window, ⌘, and
//! About tinyclient open it. It shows only what works today: Accounts, Appearance (text size) and
//! About. General, Notifications, Privacy, Audio & video and Storage come with what they set;
//! until then they are left out rather than shown as switches that do nothing.
//!
//! It holds nothing of its own: the accounts are the shell's snapshot, read each frame, and
//! every control acts at once through the shell.

use super::components::{button, glyph, link_word, network_mark, segment, segmented, ButtonKind};
use super::layout::NETWORK_ORDER;
use super::state::Shell;
use super::theme::{self, Type};
use gpui::{
    div, point, prelude::*, px, size, App, Bounds, ClipboardItem, Context, Div, Global, SharedString, Subscription,
    TitlebarOptions, WeakEntity, Window, WindowBounds, WindowHandle, WindowOptions,
};
use protocol::{AccountKey, AccountStatus, Link, Network};

/// The window's size: fixed, not resizable.
const SIZE: (f32, f32) = (720., 560.);
const LIST_WIDTH: f32 = 184.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Accounts,
    Appearance,
    About,
}

impl Section {
    fn name(self) -> &'static str {
        match self {
            Section::Accounts => "Accounts",
            Section::Appearance => "Appearance",
            Section::About => "About",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Section::Accounts => "icons/users.svg",
            Section::Appearance => "icons/settings.svg",
            Section::About => "icons/info.svg",
        }
    }
}

/// The shell the window reads, and the window once it is open.
#[derive(Default)]
struct Host {
    shell: Option<WeakEntity<Shell>>,
    window: Option<WindowHandle<SettingsWindow>>,
}

impl Global for Host {}

/// Called by the shell when it starts: Settings reads its accounts from it.
pub fn attach(shell: WeakEntity<Shell>, cx: &mut App) {
    cx.default_global::<Host>().shell = Some(shell);
}

/// Opens Settings on `section`, or brings the open window forward and shows `section` there.
pub fn open(section: Section, cx: &mut App) {
    let host = cx.default_global::<Host>();
    let Some(shell) = host.shell.clone() else {
        return;
    };
    if let Some(window) = host.window {
        let shown = window.update(cx, |settings, window, cx| {
            settings.section = section;
            settings.confirm = None;
            window.activate_window();
            cx.notify();
        });
        if shown.is_ok() {
            return;
        }
    }
    let bounds = Bounds::centered(None, size(px(SIZE.0), px(SIZE.1)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some("Settings".into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(12.), px(16.))),
        }),
        is_resizable: false,
        is_minimizable: false,
        ..Default::default()
    };
    let opened = cx.open_window(options, |_, cx| cx.new(|cx| SettingsWindow::new(shell, section, cx)));
    match opened {
        Ok(window) => cx.default_global::<Host>().window = Some(window),
        Err(err) => eprintln!("tinyclient: the Settings window could not open: {err}"),
    }
}

pub struct SettingsWindow {
    shell: WeakEntity<Shell>,
    section: Section,
    /// The account whose sign-out is being confirmed.
    confirm: Option<AccountStatus>,
    _redraw: Option<Subscription>,
}

impl SettingsWindow {
    fn new(shell: WeakEntity<Shell>, section: Section, cx: &mut Context<Self>) -> SettingsWindow {
        // The shell's snapshot changing is what redraws the accounts.
        let redraw = shell.upgrade().map(|shell| cx.observe(&shell, |_, _, cx| cx.notify()));
        SettingsWindow { shell, section, confirm: None, _redraw: redraw }
    }

    fn list(&self, cx: &mut Context<Self>) -> Div {
        let item = |section: Section, selected: bool, cx: &mut Context<Self>| {
            div()
                .id(section.name())
                .h(px(32.))
                .px(px(10.))
                .rounded(px(8.))
                .flex()
                .items_center()
                .gap(px(8.))
                .cursor_pointer()
                .type_ui()
                .text_color(if selected { theme::text() } else { theme::text_soft() })
                .when(selected, |item| item.bg(theme::selected()))
                .when(!selected, |item| item.hover(|style| style.bg(theme::hover())))
                .child(glyph(section.icon(), 16., if selected { theme::text() } else { theme::text_faint() }))
                .child(section.name())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.section = section;
                    this.confirm = None;
                    cx.notify();
                }))
        };
        div()
            .w(px(LIST_WIDTH))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(2.))
            .px(px(8.))
            .pt(px(theme::STRIP))
            .pb(px(12.))
            .bg(theme::sidebar())
            .border_r_1()
            .border_color(theme::hairline())
            .child(item(Section::Accounts, self.section == Section::Accounts, cx))
            .child(item(Section::Appearance, self.section == Section::Appearance, cx))
            .child(div().flex_1())
            .child(item(Section::About, self.section == Section::About, cx))
    }

    fn accounts(&self, cx: &mut Context<Self>) -> Div {
        let Some(shell) = self.shell.upgrade() else {
            return div();
        };
        let shell = shell.read(cx);
        let accounts = shell.snapshot.as_ref().map(|s| s.accounts.clone()).unwrap_or_default();
        let (signing_in, failed) = (shell.signing_in, shell.sign_in_failed.clone());
        let mut pane = div().flex().flex_col().gap(px(20.));
        for network in NETWORK_ORDER {
            let mine: Vec<&AccountStatus> = accounts.iter().filter(|a| a.key.network == network && a.link != Link::SignedOut).collect();
            let mut group = group();
            if mine.is_empty() {
                let waiting = signing_in == Some(network);
                let line = if waiting { "Finish signing in in the other window." } else { "Not signed in" };
                let action = if waiting {
                    div().into_any_element()
                } else {
                    button(("sign-in", network as usize), ButtonKind::Secondary, "Sign in")
                        .on_click(cx.listener(move |this, _, _, cx| this.sign_in(network, cx)))
                        .into_any_element()
                };
                group = group.child(row(network, network_name(network).into(), line.into(), None, action));
            }
            for (ix, account) in mine.iter().enumerate() {
                let state = account_state(account);
                let action = if account.link == Link::Expired {
                    button(("sign-in-again", ix + 10 * network as usize), ButtonKind::Primary, "Sign in again")
                        .on_click(cx.listener(move |this, _, _, cx| this.sign_in(network, cx)))
                        .into_any_element()
                } else {
                    let account = (*account).clone();
                    button(("sign-out", ix + 10 * network as usize), ButtonKind::Ghost, "Sign out…")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.confirm = Some(account.clone());
                            cx.notify();
                        }))
                        .into_any_element()
                };
                let name: SharedString = if account.name.is_empty() { network_name(network).into() } else { account.name.clone().into() };
                let line = format!("{} · {}", network_name(network), state);
                group = group.when(ix > 0, |group| group.child(hairline())).child(row(network, name, line.into(), Some(dot(account)), action));
            }
            pane = pane.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(caption(network_name(network)))
                    .child(group)
                    .children(failed.as_ref().filter(|(at, _)| *at == network).map(|(_, words)| {
                        div().type_small().text_color(theme::danger()).child(words.clone())
                    })),
            );
        }
        pane.child(caption("Signing out removes this account's sign-in and everything tinyclient saved for it from this Mac."))
    }

    fn appearance(&self, cx: &mut Context<Self>) -> Div {
        let step = theme::text_step();
        let choice = |id: &'static str, label: &'static str, value: i32, cx: &mut Context<Self>| {
            segment(id, label, step == value).on_click(cx.listener(move |this, _, window, cx| this.text_size(value, window, cx)))
        };
        let control = segmented()
            .child(choice("text-small", "Small", -1, cx))
            .child(choice("text-default", "Default", 0, cx))
            .child(choice("text-large", "Large", 2, cx));
        div().flex().flex_col().gap(px(6.)).child(
            group().child(
                div()
                    .h(px(44.))
                    .px(px(14.))
                    .flex()
                    .items_center()
                    .child(div().flex_1().type_ui().text_color(theme::text()).child("Text size"))
                    .child(control),
            ),
        )
        .child(caption("Also ⌘+, ⌘− and ⌘0 from the View menu."))
    }

    fn about(&self, cx: &mut Context<Self>) -> Div {
        let version = format!("Version {}", env!("CARGO_PKG_VERSION"));
        let copied = version.clone();
        let plate = div()
            .size(px(96.))
            .rounded(px(22.))
            .bg(gpui::rgb(0x1a1c2a))
            .border_1()
            .border_color(theme::hairline())
            .flex()
            .items_center()
            .justify_center()
            .child(glyph("icons/messages-square.svg", 52., gpui::rgb(0xa3acff)));
        let credit = |words: &'static str| div().px(px(14.)).py(px(12.)).type_small().text_color(theme::text_soft()).child(words);
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(6.))
            .pt(px(8.))
            .child(plate)
            .child(div().pt(px(10.)).text_size(px(22.)).font_weight(theme::SEMIBOLD).text_color(theme::text()).child("tinyclient"))
            .child(div().type_small().text_color(theme::text_faint()).child(version))
            .child(div().type_ui().text_color(theme::text_soft()).child("One small app for Slack, Discord, Telegram and Teams."))
            .child(
                div().pt(px(10.)).child(button("copy-version", ButtonKind::Secondary, "Copy version").on_click(cx.listener(
                    move |_, _, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(format!("tinyclient {copied}"))),
                ))),
            )
            .child(
                div()
                    .w_full()
                    .pt(px(18.))
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(caption("Acknowledgements"))
                    .child(
                        group()
                            .child(credit("Network icons: Dashboard Icons (homarr-labs/dashboard-icons), Apache-2.0. The logos are trademarks of their owners."))
                            .child(hairline())
                            .child(credit("Interface icons: Lucide, ISC.")),
                    ),
            )
    }

    /// The sign-out sheet (§6), over the whole window.
    fn sheet(&self, account: &AccountStatus, cx: &mut Context<Self>) -> gpui::Stateful<Div> {
        let network = network_name(account.key.network);
        let name = if account.name.is_empty() { network.to_owned() } else { account.name.clone() };
        let key = account.key.clone();
        let panel = div()
            .w(px(420.))
            .p(px(24.))
            .rounded(px(theme::R_MODAL))
            .bg(theme::overlay())
            .border_1()
            .border_color(theme::overlay_edge())
            .shadow(theme::shadow_modal())
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(network_mark(account.key.network, 24., theme::text_soft()))
                    .child(div().type_title().text_color(theme::text()).child(format!("Sign out of {name}?"))),
            )
            .child(div().type_ui().text_color(theme::text_soft()).child(format!(
                "This removes from this Mac the saved sign-in and the saved messages, images and files of this account, and ends this session on {network}."
            )))
            .child(div().type_ui().text_color(theme::text_soft()).child(format!(
                "Nothing is deleted on {network}. Your other accounts are not touched."
            )))
            .child(
                div()
                    .pt(px(8.))
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(button("sign-out-cancel", ButtonKind::Secondary, "Cancel").on_click(cx.listener(|this, _, _, cx| {
                        this.confirm = None;
                        cx.notify();
                    })))
                    .child(button("sign-out-confirm", ButtonKind::Danger, "Sign out").on_click(cx.listener(move |this, _, _, cx| {
                        this.confirm = None;
                        this.sign_out(key.clone(), cx);
                    }))),
            );
        div()
            .id("sheet-scrim")
            .absolute()
            .inset_0()
            .bg(theme::scrim())
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .on_click(cx.listener(|this, _, _, cx| {
                this.confirm = None;
                cx.notify();
            }))
            .child(div().id("sheet").on_click(|_, _, cx| cx.stop_propagation()).child(panel))
    }

    fn sign_in(&mut self, network: Network, cx: &mut Context<Self>) {
        let _ = self.shell.update(cx, |shell, cx| shell.sign_in(network, cx));
        cx.notify();
    }

    fn sign_out(&mut self, account: AccountKey, cx: &mut Context<Self>) {
        let _ = self.shell.update(cx, |shell, cx| shell.sign_out(account, cx));
        cx.notify();
    }

    fn text_size(&mut self, step: i32, window: &mut Window, cx: &mut Context<Self>) {
        theme::set_text_step(step);
        let _ = self.shell.update(cx, |shell, cx| {
            shell.save_prefs(cx);
            cx.notify();
        });
        window.refresh();
        cx.refresh_windows();
    }
}

impl Render for SettingsWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match self.section {
            Section::Accounts => self.accounts(cx),
            Section::Appearance => self.appearance(cx),
            Section::About => self.about(cx),
        };
        let heading = (self.section != Section::About).then(|| {
            div().pb(px(16.)).text_size(px(17.)).font_weight(theme::SEMIBOLD).text_color(theme::text()).child(self.section.name())
        });
        let sheet = self.confirm.clone().map(|account| self.sheet(&account, cx));
        div()
            .size_full()
            .relative()
            .flex()
            .bg(theme::canvas())
            .font_family(theme::FONT)
            .text_color(theme::text())
            .child(self.list(cx))
            .child(
                div()
                    .id("settings-pane")
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_y_scroll()
                    .px(px(28.))
                    .pt(px(theme::STRIP - 12.))
                    .pb(px(20.))
                    .children(heading)
                    .child(body),
            )
            .children(sheet)
    }
}

fn network_name(network: Network) -> &'static str {
    match network {
        Network::Teams => "Microsoft Teams",
        other => other.label(),
    }
}

/// An account's state in words: the link, or what stands in its way.
fn account_state(account: &AccountStatus) -> String {
    if !account.required.is_empty() {
        return "Needs attention in its own app".to_owned();
    }
    match account.link {
        Link::Offline if !account.reconnecting => "Offline".to_owned(),
        link => link_word(link).to_owned(),
    }
}

fn dot(account: &AccountStatus) -> Div {
    let color = match account.link {
        Link::Connected => theme::positive(),
        Link::Offline => theme::warning(),
        Link::Expired | Link::SignedOut => theme::text_faint(),
    };
    div().size(px(7.)).rounded_full().bg(color).flex_none()
}

fn group() -> Div {
    div().flex().flex_col().rounded(px(12.)).bg(theme::raised()).border_1().border_color(theme::hairline())
}

fn hairline() -> Div {
    div().h(px(1.)).mx(px(14.)).bg(theme::hairline())
}

fn caption(words: impl Into<SharedString>) -> Div {
    div().type_small().text_color(theme::text_faint()).child(words.into())
}

fn row(network: Network, name: SharedString, line: SharedString, dot: Option<Div>, action: gpui::AnyElement) -> Div {
    div()
        .min_h(px(52.))
        .px(px(14.))
        .py(px(8.))
        .flex()
        .items_center()
        .gap(px(12.))
        .child(div().size(px(32.)).flex_none().rounded(px(8.)).bg(theme::hover()).flex().items_center().justify_center().child(network_mark(network, 18., theme::text_soft())))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(div().type_ui().font_weight(theme::MEDIUM).text_color(theme::text()).truncate().child(name))
                .child(div().flex().items_center().gap(px(6.)).children(dot).child(div().type_small().text_color(theme::text_faint()).child(line))),
        )
        .child(action)
}

/// `--settings accounts|appearance|about|sign-out --settings-shot out.png`, with `--shot`: opens
/// Settings once the shell has its first answer and writes that window's frame.
#[cfg(feature = "gui-shot")]
pub fn shot(cx: &mut App) {
    let flag = |name: &str| std::env::args().skip_while(|arg| arg != name).nth(1);
    let (Some(section), Some(path)) = (flag("--settings"), flag("--settings-shot")) else {
        return;
    };
    cx.spawn(async move |cx| {
        cx.background_executor().timer(std::time::Duration::from_millis(900)).await;
        let open_on = match section.as_str() {
            "appearance" => Section::Appearance,
            "about" => Section::About,
            _ => Section::Accounts,
        };
        cx.update(|cx| open(open_on, cx));
        let Some(window) = cx.update(|cx| cx.default_global::<Host>().window) else {
            return;
        };
        if section == "sign-out" {
            let _ = window.update(cx, |settings, _, cx| {
                let first = settings.shell.upgrade().and_then(|shell| shell.read(cx).snapshot.as_ref()?.accounts.first().cloned());
                settings.confirm = first;
                cx.notify();
            });
        }
        // Pictures load on the first frames; draw again once they have.
        for _ in 0..5 {
            cx.background_executor().timer(std::time::Duration::from_millis(300)).await;
            let _ = window.update(cx, |_, window, _| window.refresh());
        }
        cx.background_executor().timer(std::time::Duration::from_millis(200)).await;
        match window.update(cx, |_, window, _| window.render_to_image()) {
            Ok(Ok(image)) => image.save(&path).expect("the Settings shot is written"),
            _ => eprintln!("tinyclient: no Settings shot"),
        }
    })
    .detach();
}
