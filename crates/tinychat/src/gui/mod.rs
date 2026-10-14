//! The window. It holds what is on screen and nothing else; chatd owns the rest.
//!
//! One file per region, so they can change without meeting: `state` (what the shell knows and
//! how it stays current), `layout`, `rail`, `sidebar`, `conversation`, `people`, `composer`,
//! `call`. Each view keeps its own `State` in its own file; `state::Shell` only holds them.

mod assets;
mod call;
mod components;
mod composer;
mod conversation;
mod input;
mod layout;
mod people;
mod rail;
mod sidebar;
mod signin;
mod state;
mod theme;
mod time;

use gpui::{point, prelude::*, px, size, App, Bounds, Context, Div, KeyBinding, TitlebarOptions, WindowBounds, WindowOptions};
use protocol::{support_dir, Key, Network};
use state::Shell;

gpui::actions!(
    tinychat,
    [
        Quit, Switcher, Back, Forward, UpLevel, ToggleSidebar, CloseRight, CloseTop, NextRow,
        PreviousRow, NextUnread, PreviousUnread, ListUp, ListDown, ListOpen, ListFold, ListUnfold,
        MarkRead, FocusNext, FocusPrevious,
        Place1, Place2, Place3, Place4, Place5, Place6, Place7, Place8, Place9
    ]
);

/// Where the window keeps what it remembers between launches: pane widths and rail pins.
// CONTRACT: the protocol has no home for what only the window knows (pane widths, which Spaces
// are pinned to the rail), so they are a small file beside chatd's, read and written off the
// main thread.
const PREFS: &str = "shell.prefs";

fn network_word(network: Network) -> &'static str {
    match network {
        Network::Telegram => "telegram",
        Network::Discord => "discord",
        Network::Slack => "slack",
        Network::Teams => "teams",
    }
}

struct Prefs {
    sidebar: f32,
    right: f32,
    pins: Vec<Key>,
}

fn parse_prefs(text: &str) -> Prefs {
    let mut prefs = Prefs { sidebar: layout::SIDEBAR_WIDTH, right: layout::RIGHT_WIDTH, pins: Vec::new() };
    for line in text.lines() {
        let mut fields = line.split('\t');
        match (fields.next(), fields.next(), fields.next(), fields.next()) {
            (Some("sidebar"), Some(width), ..) => {
                prefs.sidebar = width.parse().map_or(prefs.sidebar, |w: f32| w.clamp(layout::SIDEBAR_RANGE.0, layout::SIDEBAR_RANGE.1));
            }
            (Some("right"), Some(width), ..) => {
                prefs.right = width.parse().map_or(prefs.right, |w: f32| w.clamp(layout::RIGHT_RANGE.0, layout::RIGHT_RANGE.1));
            }
            (Some("pin"), Some(network), Some(account), Some(id)) => {
                if let Some(network) = Network::ALL.into_iter().find(|n| network_word(*n) == network) {
                    prefs.pins.push(Key { network, account: account.to_owned(), id: id.to_owned() });
                }
            }
            _ => {}
        }
    }
    prefs
}

fn write_prefs(prefs: &Prefs) -> String {
    let mut text = format!("sidebar\t{}\nright\t{}\n", prefs.sidebar, prefs.right);
    for pin in &prefs.pins {
        text.push_str(&format!("pin\t{}\t{}\t{}\n", network_word(pin.network), pin.account, pin.id));
    }
    text
}

impl Shell {
    fn load_prefs(&mut self, cx: &mut Context<Self>) {
        let read = || std::fs::read_to_string(support_dir().join(PREFS)).ok();
        self.off_thread(cx, read, |this, text, cx| {
            let Some(prefs) = text.as_deref().map(parse_prefs) else {
                return;
            };
            this.layout_state.sidebar_width = prefs.sidebar;
            this.layout_state.right_width = prefs.right;
            this.sidebar_state.pins = prefs.pins;
            cx.notify();
        });
    }

    fn save_prefs(&mut self, cx: &mut Context<Self>) {
        let text = write_prefs(&Prefs {
            sidebar: self.layout_state.sidebar_width,
            right: self.layout_state.right_width,
            pins: self.sidebar_state.pins.clone(),
        });
        self.off_thread(cx, move || std::fs::write(support_dir().join(PREFS), text), |_, _, _| {});
    }

    /// `cmd-1` to `cmd-9`: the rail's places in order.
    fn visit_place(&mut self, index: usize, window: &mut gpui::Window, cx: &mut Context<Self>) {
        if let Some(mode) = self.places().get(index).cloned() {
            self.visit(mode, window, cx);
        }
    }

    /// The shell itself has the keys: no field, no message, no menu.
    fn in_lists(&self, window: &gpui::Window) -> bool {
        self.focus.is_focused(window) || self.sidebar_state.focus.is_focused(window)
    }

    /// A character typed with no field focused goes to the composer.
    fn typed(&mut self, event: &gpui::KeyDownEvent, window: &mut gpui::Window, cx: &mut Context<Self>) {
        let keys = &event.keystroke;
        let command = keys.modifiers.platform || keys.modifiers.control || keys.modifiers.function;
        let Some(text) = keys.key_char.as_deref().filter(|text| !command && typed_text(text)) else {
            return;
        };
        let layered = self.sidebar_state.switcher.is_some() || self.layout_state.menu.is_some();
        if layered || self.open.is_none() || !self.in_lists(window) {
            return;
        }
        self.composer_type(text, window, cx);
        cx.stop_propagation();
    }

    /// `tab` and `shift-tab`: the keys move between the sidebar's list and the composer. The
    /// region that has them shows it on its row or its field, never around a pane.
    // The messages and the right pane are the conversation view's; they take the keys by click.
    fn focus_region(&mut self, forward: bool, window: &mut gpui::Window, cx: &mut Context<Self>) {
        let _ = forward;
        if self.in_lists(window) && self.open.is_some() {
            self.composer_focus(window, cx);
            return;
        }
        window.focus(&self.sidebar_state.focus, cx);
        self.cursor_to_open();
        cx.notify();
    }

    /// The window's keys, on its root so they work wherever focus is. A text field's own keys
    /// win while it has focus.
    fn keys(&self, root: Div, cx: &mut Context<Self>) -> Div {
        macro_rules! places {
            ($root:expr, $($action:ty => $index:expr),*) => {
                $root$(.on_action(cx.listener(|this, _: &$action, window, cx| this.visit_place($index, window, cx))))*
            };
        }
        let root = places!(root, Place1 => 0, Place2 => 1, Place3 => 2, Place4 => 3, Place5 => 4, Place6 => 5, Place7 => 6, Place8 => 7, Place9 => 8);
        root.track_focus(&self.focus)
            .key_context("Shell")
            .on_action(cx.listener(|this, _: &Switcher, window, cx| this.open_switcher("", window, cx)))
            .on_action(cx.listener(|this, _: &Back, _, cx| this.travel(true, cx)))
            .on_action(cx.listener(|this, _: &Forward, _, cx| this.travel(false, cx)))
            .on_action(cx.listener(|this, _: &UpLevel, _, cx| this.up_level(cx)))
            .on_action(cx.listener(|this, _: &ToggleSidebar, window, cx| this.toggle_sidebar(window, cx)))
            .on_action(cx.listener(|this, _: &MarkRead, _, cx| this.mark_open_read(cx)))
            .on_action(cx.listener(|this, _: &FocusNext, window, cx| this.focus_region(true, window, cx)))
            .on_action(cx.listener(|this, _: &FocusPrevious, window, cx| this.focus_region(false, window, cx)))
            .on_key_down(cx.listener(Self::typed))
            .on_action(cx.listener(|this, _: &CloseRight, _, cx| this.close_right(cx)))
            .on_action(cx.listener(|this, _: &CloseTop, window, cx| {
                // With no layer to close, Escape belongs to the conversation.
                if !this.close_top(window, cx) {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &NextRow, _, cx| this.walk(true, cx)))
            .on_action(cx.listener(|this, _: &PreviousRow, _, cx| this.walk(false, cx)))
            .on_action(cx.listener(|this, _: &NextUnread, _, cx| this.walk_unread(true, cx)))
            .on_action(cx.listener(|this, _: &PreviousUnread, _, cx| this.walk_unread(false, cx)))
            .on_action(cx.listener(|this, _: &ListDown, _, cx| {
                if !this.first_run_key("down", cx) {
                    this.move_cursor(true, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ListUp, _, cx| {
                if !this.first_run_key("up", cx) {
                    this.move_cursor(false, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ListOpen, _, cx| {
                if !this.first_run_key("enter", cx) {
                    this.open_cursor(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ListFold, _, cx| this.fold_cursor(false, cx)))
            .on_action(cx.listener(|this, _: &ListUnfold, _, cx| this.fold_cursor(true, cx)))
    }
}

/// Whether a key's text is something to type: printable, and not the keys the lists use.
fn typed_text(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| !c.is_control()) && text != " "
}

fn bind_keys(cx: &mut App) {
    let shell = Some("Shell");
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-k", Switcher, None),
        KeyBinding::new("cmd-[", Back, None),
        KeyBinding::new("cmd-]", Forward, None),
        KeyBinding::new("cmd-up", UpLevel, shell),
        KeyBinding::new("cmd-\\", ToggleSidebar, None),
        KeyBinding::new("cmd-.", CloseRight, None),
        KeyBinding::new("escape", CloseTop, shell),
        KeyBinding::new("shift-escape", MarkRead, shell),
        KeyBinding::new("tab", FocusNext, shell),
        KeyBinding::new("shift-tab", FocusPrevious, shell),
        KeyBinding::new("alt-down", NextRow, None),
        KeyBinding::new("alt-up", PreviousRow, None),
        KeyBinding::new("alt-shift-down", NextUnread, None),
        KeyBinding::new("alt-shift-up", PreviousUnread, None),
        KeyBinding::new("down", ListDown, shell),
        KeyBinding::new("up", ListUp, shell),
        KeyBinding::new("enter", ListOpen, shell),
        KeyBinding::new("left", ListFold, shell),
        KeyBinding::new("right", ListUnfold, shell),
        KeyBinding::new("cmd-1", Place1, None),
        KeyBinding::new("cmd-2", Place2, None),
        KeyBinding::new("cmd-3", Place3, None),
        KeyBinding::new("cmd-4", Place4, None),
        KeyBinding::new("cmd-5", Place5, None),
        KeyBinding::new("cmd-6", Place6, None),
        KeyBinding::new("cmd-7", Place7, None),
        KeyBinding::new("cmd-8", Place8, None),
        KeyBinding::new("cmd-9", Place9, None),
    ]);
}

/// `--shot out.png` draws one state with live data, writes the frame and quits, so every state
/// can be looked at without clicking:
/// `--size WxH` / `--narrow`, `--mode inbox|people|discord|slack|space` (`--space N` picks
/// which), `--filter unread|mentions`, `--open-first` / `--open N` (a row of the list on show),
/// `--cursor N`, `--switcher [letters]`, `--menu add|own|network|space|row`, `--float`,
/// `--hide-sidebar`, `--right thread|members`, `--pin N`, `--toast`, `--components`,
/// `--walk N` (N presses of the unread walk), `--back N` (N steps back in history),
/// `--sign-in-failed closed|unreachable|blocked` (first launch, with nothing signed in).
#[cfg(feature = "gui-shot")]
mod shot {
    use super::layout::{Menu, RightKind, RightPane};
    use super::sidebar::Mode;
    use super::state::Shell;
    use super::{components, flag, theme};
    use gpui::{div, point, prelude::*, px, App, Context, Window};
    use protocol::{InboxFilter, Network};
    use std::time::Duration;

    fn has(name: &str) -> bool {
        std::env::args().any(|arg| arg == name)
    }

    fn demo_pane(_: &Shell, _: &mut Window, _: &mut Context<Shell>) -> gpui::AnyElement {
        components::empty_state("icons/message-square.svg", "No replies yet", "This pane is filled by another view.").into_any_element()
    }

    fn stage(shell: &mut Shell, step: usize, window: &mut Window, cx: &mut Context<Shell>) {
        match step {
            0 => {
                if let Some(how) = flag("--sign-in-failed") {
                    use protocol::ErrorCode;
                    let (code, detail) = match how.as_str() {
                        "unreachable" => (ErrorCode::SignInUnreachable, ""),
                        "blocked" => (ErrorCode::SignInBlocked, ""),
                        _ => (ErrorCode::SignIn, "The sign-in window stopped before it finished."),
                    };
                    shell.sign_in_failed = super::state::sign_in_words(Network::Slack, code, detail).map(|words| (Network::Slack, words));
                }
                if let Some(index) = flag("--pin").and_then(|n| n.parse::<usize>().ok()) {
                    if let Some(space) = shell.spaces.get(index) {
                        shell.pin(space.key.clone(), true, cx);
                    }
                }
                let space = flag("--space").and_then(|n| n.parse::<usize>().ok()).unwrap_or(0);
                match flag("--mode").as_deref() {
                    Some("people") => shell.go(Mode::People, cx),
                    Some("discord") => shell.go(Mode::Network(Network::Discord), cx),
                    Some("slack") => shell.go(Mode::Network(Network::Slack), cx),
                    Some("space") => {
                        if let Some(space) = shell.spaces.get(space) {
                            shell.go(Mode::Space(space.key.clone()), cx);
                        }
                    }
                    _ => {}
                }
                match flag("--filter").as_deref() {
                    Some("unread") => shell.set_filter(InboxFilter::Unread, cx),
                    Some("mentions") => shell.set_filter(InboxFilter::Mentions, cx),
                    _ => {}
                }
            }
            1 => {
                let row = if has("--open-first") { Some(0) } else { flag("--open").and_then(|n| n.parse::<usize>().ok()) };
                if let Some(row) = row {
                    shell.open_listed(row, cx);
                }
            }
            _ => {
                if let Some(cursor) = flag("--cursor").and_then(|n| n.parse::<usize>().ok()) {
                    shell.sidebar_state.cursor = Some(cursor);
                }
                if has("--float") {
                    shell.layout_state.floating = true;
                }
                if has("--hide-sidebar") {
                    shell.layout_state.sidebar_hidden = true;
                }
                if has("--focus-list") {
                    window.focus(&shell.sidebar_state.focus, cx);
                    shell.cursor_to_open();
                }
                match flag("--right").as_deref() {
                    Some("info") => shell.toggle_info(cx),
                    Some("members") => shell.open_right(RightPane { kind: RightKind::Members, title: "Members".into(), count: Some(128), body: demo_pane }, cx),
                    Some(_) => shell.open_right(RightPane { kind: RightKind::Thread, title: "Thread".into(), count: Some(6), body: demo_pane }, cx),
                    None => {}
                }
                match flag("--menu").as_deref() {
                    Some("add") => shell.open_menu(Menu::Add, cx),
                    Some("own") => shell.open_menu(Menu::Own, cx),
                    Some("network") => shell.open_menu(Menu::Network(shell.sidebar_state.mode.network().unwrap_or(Network::Discord)), cx),
                    Some("space") => {
                        if let Mode::Space(space) = shell.sidebar_state.mode.clone() {
                            shell.open_menu(Menu::Space(space), cx);
                        }
                    }
                    Some("row") => {
                        if let Some(row) = shell.sidebar_state.inbox.get(1).cloned() {
                            shell.open_menu(Menu::Row(row, point(px(240.), px(180.))), cx);
                        }
                    }
                    _ => {}
                }
                if has("--toast") {
                    shell.toast(Some(("icons/check.svg", theme::text_soft())), "Marked 14 conversations read", cx);
                }
                if has("--switcher") {
                    let letters = flag("--switcher").filter(|next| !next.starts_with("--")).unwrap_or_default();
                    shell.open_switcher(&letters, window, cx);
                }
                if has("--components") {
                    shell.open_right(
                        RightPane { kind: RightKind::Thread, title: "Components".into(), count: None, body: |_, _, _| sheet().into_any_element() },
                        cx,
                    );
                }
                cx.notify();
            }
        }
    }

    /// The base components in their states, for looking at next to `mockups/01-system.html`.
    fn sheet() -> gpui::Div {
        use components::*;
        use protocol::Presence;
        let line = || div().flex().flex_wrap().items_center().gap(px(8.));
        let presences = [Presence::Online, Presence::Idle, Presence::Dnd, Presence::Offline];
        div()
            .p(px(16.))
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(
                line()
                    .child(button("b1", ButtonKind::Primary, "Send"))
                    .child(button("b2", ButtonKind::Secondary, "Cancel"))
                    .child(button("b3", ButtonKind::Ghost, "Mark read"))
                    .child(button("b4", ButtonKind::Danger, "Sign out")),
            )
            .child(
                line()
                    .child(button_with_icon("b5", ButtonKind::Secondary, "icons/phone.svg", "Call"))
                    .child(button_disabled("Sending…"))
                    .child(icon_button("b6", "icons/smile.svg", false))
                    .child(icon_button("b7", "icons/smile.svg", true)),
            )
            .child(field(false).child(div().text_color(theme::text_faint()).child("Display name")))
            .child(field(true).child("Wren Halloran"))
            .child(field_error().child("wren@"))
            .child(search_field(false).child(div().flex_1().text_color(theme::text_faint()).child("Search")).child(keycap("⌘K")))
            .child(
                line()
                    .children([16., 20., 24., 32., 36., 64.].map(|size| Avatar::named("Wren Halloran", size).build()))
                    .children(["Odalys Brandt", "Ines Varga", "Niko Aldana", "Tobin Reyes"].into_iter().zip(presences).map(
                        |(name, presence)| Avatar::named(name, 32.).presence(presence, theme::sidebar()).build(),
                    ))
                    .child(Avatar::named("Lanternbay", 32.).shape(Shape::Place).build())
                    .child(Avatar::channel("icons/hash.svg", 32.).build()),
            )
            .child(
                line()
                    .child(badge_count(3))
                    .child(badge_count(120))
                    .child(badge_mention(12))
                    .child(badge_quiet(1))
                    .child(badge_dot(8., theme::unread()))
                    .child(badge_dot(8., theme::mention()))
                    .child(badge_dot(6., theme::text_faint())),
            )
            .child(
                segmented()
                    .child(segment("s1", "All", true))
                    .child(segment("s2", "Unread", false))
                    .child(segment("s3", "Mentions", false)),
            )
            .child(banner(Tone::Info, "No internet connection"))
            .child(banner(Tone::Warning, "Reconnecting…"))
            .child(banner(Tone::Danger, "Slack signed you out").child(banner_action("ba", "Sign in again")))
            .child(
                menu()
                    .child(menu_label("Conversation"))
                    .child(menu_item("m1", Some("icons/pin.svg"), "Pin to top", Some("⌘P")))
                    .child(menu_item("m2", Some("icons/bell-off.svg"), "Mute", None))
                    .child(menu_separator())
                    .child(menu_item_danger("m3", Some("icons/log-out.svg"), "Leave")),
            )
            .child(
                modal("Sign out of Slack?")
                    .w(px(300.))
                    .child(modal_body("Your messages stay on this Mac."))
                    .child(modal_buttons().child(button("mb1", ButtonKind::Secondary, "Cancel")).child(button("mb2", ButtonKind::Danger, "Sign out"))),
            )
            .child(toast(Some(("icons/check.svg", theme::text_soft())), "Copied").w(px(300.)))
    }

    pub fn shot(window: gpui::WindowHandle<Shell>, cx: &mut App) {
        let Some(path) = flag("--shot") else {
            return;
        };
        cx.spawn(async move |cx| {
            let settle = Duration::from_millis(500);
            cx.background_executor().timer(settle).await;
            for step in 0..3 {
                let _ = window.update(cx, |shell, window, cx| stage(shell, step, window, cx));
                cx.background_executor().timer(settle).await;
            }
            // Each press waits for what the one before it read.
            for _ in 0..flag("--walk").and_then(|n| n.parse::<usize>().ok()).unwrap_or(0) {
                let _ = window.update(cx, |shell, _, cx| shell.walk_unread(true, cx));
                cx.background_executor().timer(settle).await;
            }
            for _ in 0..flag("--back").and_then(|n| n.parse::<usize>().ok()).unwrap_or(0) {
                let _ = window.update(cx, |shell, _, cx| shell.travel(true, cx));
                cx.background_executor().timer(settle).await;
            }
            let image = window.update(cx, |_, window, _| window.render_to_image());
            match image {
                Ok(Ok(image)) => image.save(&path).expect("the screenshot is written"),
                other => eprintln!("tinychat: no screenshot: {:?}", other.map(|r| r.map(|_| ()))),
            }
            // `--hold 20` keeps the window up so its memory can be measured from outside.
            if let Some(seconds) = flag("--hold").and_then(|n| n.parse::<u64>().ok()) {
                eprintln!("tinychat: holding, pid {}", std::process::id());
                cx.background_executor().timer(Duration::from_secs(seconds)).await;
            }
            cx.update(|cx| cx.quit());
        })
        .detach();
    }
}

fn flag(name: &str) -> Option<String> {
    std::env::args().skip_while(|arg| arg != name).nth(1)
}

/// `--size 1120x720`, for drawing a state at a given width. `--narrow` is 700 wide.
fn window_size() -> (f32, f32) {
    let asked = flag("--size").and_then(|size| {
        let (wide, tall) = size.split_once('x')?;
        Some((wide.parse().ok()?, tall.parse().ok()?))
    });
    match asked {
        Some(size) => size,
        None if std::env::args().any(|arg| arg == "--narrow") => (700., 720.),
        None => (1120., 720.),
    }
}

/// When the process began drawing, for `--shot`'s report of how long the first list took.
#[cfg(feature = "gui-shot")]
static LAUNCHED: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

pub fn run() {
    #[cfg(feature = "gui-shot")]
    LAUNCHED.get_or_init(std::time::Instant::now);
    gpui_platform::application()
        .with_assets(assets::Assets)
        .run(|cx: &mut App| {
            input::bind_keys(cx);
            call::bind_keys(cx);
            bind_keys(cx);
            cx.on_action(|_: &Quit, cx| cx.quit());

            let (wide, tall) = window_size();
            let mut bounds = Bounds::centered(None, size(px(wide), px(tall)), cx);
            // Centering in a visible area of odd size lands on half a point, and the system
            // then grows the frame to whole pixels: one point wider than asked.
            bounds.origin = point(bounds.origin.x.round(), bounds.origin.y.round());
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(layout::WINDOW_MIN.0), px(layout::WINDOW_MIN.1))),
                titlebar: Some(TitlebarOptions {
                    title: Some("tinychat".into()),
                    appears_transparent: true,
                    // Centered on the 52 pt strips, inside the rail.
                    traffic_light_position: Some(point(px(10.), px(20.))),
                }),
                ..Default::default()
            };
            let window = cx
                .open_window(options, |window, cx| {
                    cx.new(|cx| {
                        let shell = Shell::new(window, cx);
                        window.focus(&shell.focus, cx);
                        shell
                    })
                })
                .expect("the window opens");
            #[cfg(feature = "gui-shot")]
            shot::shot(window, cx);
            let _ = window;
            cx.on_window_closed(|cx, _| cx.quit()).detach();
            cx.activate(true);
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_typed_at_the_lists_go_to_the_composer_and_list_keys_do_not() {
        assert!(typed_text("a") && typed_text("É") && typed_text("?"));
        assert!(!typed_text("") && !typed_text("\n") && !typed_text("\t") && !typed_text(" "));
    }

    #[test]
    fn what_the_window_remembers_survives_a_round_trip_and_bad_lines() {
        let key = |network, id: &str| Key { network, account: "T1".into(), id: id.into() };
        let saved = Prefs { sidebar: 300., right: 400., pins: vec![key(Network::Slack, "T1"), key(Network::Discord, "9")] };
        let read = parse_prefs(&write_prefs(&saved));
        assert_eq!((read.sidebar, read.right, &read.pins), (300., 400., &saved.pins));

        // A width outside its range is clamped; a line that means nothing is skipped.
        let odd = parse_prefs("sidebar\t9000\nright\twide\npin\tmyspace\ta\tb\nnonsense\npin\tslack\tT1\tT1\n");
        assert_eq!((odd.sidebar, odd.right, odd.pins), (420., 340., vec![key(Network::Slack, "T1")]));
    }
}
