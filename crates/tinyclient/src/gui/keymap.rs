//! Shortcut chords and the command list behind `>` in the quick switcher.
//!
//! A stock map names every default. Settings stores a change in the support directory and the
//! shell binds that chord. Message letters apply only while a message row has focus. `Up` on an
//! empty composer is `EditLast` here; the composer file is owned by other pull requests, so
//! nothing there reads it yet. Pin and mark-unread-from-here have no request, so the row key
//! resolves the message and does not call out.

use gpui::{KeyBinding, Keystroke};
use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Message,
    Text,
    /// Empty-composer Up. Nothing in the composer reads this until that file is free to change.
    #[allow(dead_code)]
    Composer {
        empty: bool,
    },
    #[allow(dead_code)]
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageAct {
    Edit,
    Reply,
    React,
    Thread,
    Pin,
    Unread,
    Delete,
    EditLast,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindError {
    Conflict(&'static str),
    Types,
    Unknown,
}

impl BindError {
    pub fn words(self) -> String {
        match self {
            BindError::Conflict(name) => format!("{name} already uses that key"),
            BindError::Types => {
                "That key would be typed as text. Hold Ctrl, Alt, or the shortcut key.".to_owned()
            }
            BindError::Unknown => "That shortcut cannot be changed.".to_owned(),
        }
    }
}

/// What picking a `>` row does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandRun {
    /// Dispatch the action this shortcut already has.
    Binding(&'static str),
    /// `online`, `idle`, `dnd`, or `clear`.
    Status(&'static str),
    /// `hour`, `morning`, `resume`, or `clear`.
    Pause(&'static str),
    Mute,
    SignIn(protocol::Network),
    /// A settings section's own name.
    OpenSettings(&'static str),
}

pub struct Command {
    pub id: &'static str,
    pub title: String,
    pub detail: &'static str,
    pub run: CommandRun,
}

pub struct ShownRow {
    pub id: &'static str,
    pub title: &'static str,
    pub glyph: String,
}

struct Spec {
    id: &'static str,
    command: &'static str,
    keys: &'static [&'static str],
    glyph: &'static str,
    context: Option<&'static str>,
    act: Option<MessageAct>,
    mac_only: bool,
    /// Listed after `>` . Message letters and list navigation stay out.
    palette: bool,
}

#[allow(clippy::too_many_arguments)]
const fn spec(
    id: &'static str,
    command: &'static str,
    keys: &'static [&'static str],
    glyph: &'static str,
    context: Option<&'static str>,
    act: Option<MessageAct>,
    mac_only: bool,
    palette: bool,
) -> Spec {
    Spec {
        id,
        command,
        keys,
        glyph,
        context,
        act,
        mac_only,
        palette,
    }
}

const CATALOG: &[Spec] = &[
    spec(
        "quit",
        "Quit tinyclient",
        &["secondary-q"],
        "⌘Q",
        None,
        None,
        false,
        true,
    ),
    spec(
        "quick-switcher",
        "Quick Switcher",
        &["secondary-k"],
        "⌘K",
        None,
        None,
        false,
        true,
    ),
    spec(
        "back",
        "Back",
        &["secondary-["],
        "⌘[",
        None,
        None,
        false,
        true,
    ),
    spec(
        "forward",
        "Forward",
        &["secondary-]"],
        "⌘]",
        None,
        None,
        false,
        true,
    ),
    spec(
        "up-level",
        "Up a Level",
        &["secondary-up"],
        "⌘↑",
        Some("Shell"),
        None,
        false,
        true,
    ),
    spec(
        "hide-sidebar",
        "Hide Sidebar",
        &["secondary-\\"],
        "⌘\\",
        None,
        None,
        false,
        true,
    ),
    spec(
        "close-right",
        "Close Right Pane",
        &["secondary-."],
        "⌘.",
        None,
        None,
        false,
        true,
    ),
    spec(
        "close-top",
        "",
        &["escape"],
        "",
        Some("Shell"),
        None,
        false,
        false,
    ),
    spec(
        "mark-read",
        "Mark as Read",
        &["shift-escape"],
        "⇧Esc",
        Some("Shell"),
        None,
        false,
        true,
    ),
    spec(
        "focus-next",
        "",
        &["tab"],
        "",
        Some("Shell"),
        None,
        false,
        false,
    ),
    spec(
        "focus-previous",
        "",
        &["shift-tab"],
        "",
        Some("Shell"),
        None,
        false,
        false,
    ),
    spec("next", "Next", &["alt-down"], "⌥↓", None, None, false, true),
    spec(
        "previous",
        "Previous",
        &["alt-up"],
        "⌥↑",
        None,
        None,
        false,
        true,
    ),
    spec(
        "next-unread",
        "Next Unread",
        &["alt-shift-down"],
        "⌥⇧↓",
        None,
        None,
        false,
        true,
    ),
    spec(
        "previous-unread",
        "Previous Unread",
        &["alt-shift-up"],
        "⌥⇧↑",
        None,
        None,
        false,
        true,
    ),
    spec(
        "list-down",
        "",
        &["down"],
        "",
        Some("Shell"),
        None,
        false,
        false,
    ),
    spec(
        "list-up",
        "",
        &["up"],
        "",
        Some("Shell"),
        None,
        false,
        false,
    ),
    spec(
        "list-open",
        "",
        &["enter"],
        "",
        Some("Shell"),
        None,
        false,
        false,
    ),
    spec(
        "list-fold",
        "",
        &["left"],
        "",
        Some("Shell"),
        None,
        false,
        false,
    ),
    spec(
        "list-unfold",
        "",
        &["right"],
        "",
        Some("Shell"),
        None,
        false,
        false,
    ),
    spec(
        "place-1",
        "Inbox",
        &["secondary-1"],
        "⌘1",
        None,
        None,
        false,
        true,
    ),
    spec(
        "place-2",
        "People",
        &["secondary-2"],
        "⌘2",
        None,
        None,
        false,
        true,
    ),
    spec(
        "place-3",
        "",
        &["secondary-3"],
        "⌘3",
        None,
        None,
        false,
        false,
    ),
    spec(
        "place-4",
        "",
        &["secondary-4"],
        "⌘4",
        None,
        None,
        false,
        false,
    ),
    spec(
        "place-5",
        "",
        &["secondary-5"],
        "⌘5",
        None,
        None,
        false,
        false,
    ),
    spec(
        "place-6",
        "",
        &["secondary-6"],
        "⌘6",
        None,
        None,
        false,
        false,
    ),
    spec(
        "place-7",
        "",
        &["secondary-7"],
        "⌘7",
        None,
        None,
        false,
        false,
    ),
    spec(
        "place-8",
        "",
        &["secondary-8"],
        "⌘8",
        None,
        None,
        false,
        false,
    ),
    spec(
        "place-9",
        "",
        &["secondary-9"],
        "⌘9",
        None,
        None,
        false,
        false,
    ),
    spec(
        "settings",
        "Settings…",
        &["secondary-,"],
        "⌘,",
        None,
        None,
        false,
        true,
    ),
    spec(
        "new-message",
        "New Message",
        &["secondary-n"],
        "⌘N",
        None,
        None,
        false,
        true,
    ),
    spec(
        "attach",
        "Attach File…",
        &["secondary-u"],
        "⌘U",
        None,
        None,
        false,
        true,
    ),
    spec(
        "close-window",
        "Close Window",
        &["secondary-w"],
        "⌘W",
        None,
        None,
        false,
        true,
    ),
    spec(
        "search-here",
        "Search in Conversation",
        &["secondary-f"],
        "⌘F",
        None,
        None,
        false,
        true,
    ),
    spec(
        "search-all",
        "Search Everywhere",
        &["secondary-shift-f"],
        "⇧⌘F",
        None,
        None,
        false,
        true,
    ),
    spec(
        "bigger-text",
        "Bigger Text",
        &["secondary-+", "secondary-="],
        "⌘+",
        None,
        None,
        false,
        true,
    ),
    spec(
        "smaller-text",
        "Smaller Text",
        &["secondary--"],
        "⌘−",
        None,
        None,
        false,
        true,
    ),
    spec(
        "actual-size",
        "Actual Size",
        &["secondary-0"],
        "⌘0",
        None,
        None,
        false,
        true,
    ),
    spec(
        "full-screen",
        "Enter Full Screen",
        &[],
        "⌃⌘F",
        None,
        None,
        false,
        true,
    ),
    spec(
        "details",
        "Show Details",
        &["alt-secondary-i"],
        "⌥⌘I",
        None,
        None,
        false,
        true,
    ),
    spec(
        "start-call",
        "Start Call",
        &["secondary-shift-c"],
        "⇧⌘C",
        None,
        None,
        false,
        true,
    ),
    spec(
        "open-in",
        "Open in App",
        &["alt-secondary-o"],
        "⌥⌘O",
        None,
        None,
        false,
        true,
    ),
    spec(
        "minimize",
        "Minimize",
        &["secondary-m"],
        "⌘M",
        None,
        None,
        false,
        true,
    ),
    spec(
        "shortcuts-page",
        "Keyboard Shortcuts",
        &["secondary-/"],
        "⌘/",
        None,
        None,
        false,
        true,
    ),
    spec(
        "show-logs",
        "Logs",
        &["secondary-shift-l"],
        "⇧⌘L",
        None,
        None,
        false,
        true,
    ),
    spec(
        "hide",
        "Hide tinyclient",
        &["secondary-h"],
        "⌘H",
        None,
        None,
        true,
        true,
    ),
    spec(
        "hide-others",
        "Hide Others",
        &["alt-secondary-h"],
        "⌥⌘H",
        None,
        None,
        true,
        true,
    ),
    spec(
        "mute",
        "Mute or unmute",
        &["secondary-shift-m"],
        "⇧⌘M",
        None,
        None,
        false,
        false,
    ),
    spec(
        "deafen",
        "Deafen or undeafen",
        &["secondary-shift-d"],
        "⇧⌘D",
        None,
        None,
        false,
        true,
    ),
    spec(
        "leave-call",
        "Leave the call",
        &["secondary-shift-h"],
        "⇧⌘H",
        None,
        None,
        false,
        true,
    ),
    spec(
        "accept",
        "Accept a call",
        &["secondary-shift-a"],
        "⇧⌘A",
        None,
        None,
        false,
        true,
    ),
    spec(
        "decline",
        "Decline a call",
        &["secondary-shift-r"],
        "⇧⌘R",
        None,
        None,
        false,
        true,
    ),
    spec(
        "go-to-call",
        "Go to the call",
        &["secondary-shift-j"],
        "⇧⌘J",
        None,
        None,
        false,
        true,
    ),
    spec(
        "popout-call",
        "Pop out call video",
        &["secondary-shift-o"],
        "⇧⌘O",
        None,
        None,
        false,
        true,
    ),
    spec(
        "msg-edit",
        "Edit",
        &["e"],
        "E",
        Some("MessageRow"),
        Some(MessageAct::Edit),
        false,
        false,
    ),
    spec(
        "msg-reply",
        "Reply",
        &["r"],
        "R",
        Some("MessageRow"),
        Some(MessageAct::Reply),
        false,
        false,
    ),
    spec(
        "msg-react",
        "React",
        &["+", "shift-equals"],
        "+",
        Some("MessageRow"),
        Some(MessageAct::React),
        false,
        false,
    ),
    spec(
        "msg-thread",
        "Thread",
        &["t"],
        "T",
        Some("MessageRow"),
        Some(MessageAct::Thread),
        false,
        false,
    ),
    spec(
        "msg-pin",
        "Pin",
        &["p"],
        "P",
        Some("MessageRow"),
        Some(MessageAct::Pin),
        false,
        false,
    ),
    spec(
        "msg-unread",
        "Mark unread from here",
        &["u"],
        "U",
        Some("MessageRow"),
        Some(MessageAct::Unread),
        false,
        false,
    ),
    spec(
        "msg-delete",
        "Delete",
        &["delete"],
        "Delete",
        Some("MessageRow"),
        Some(MessageAct::Delete),
        false,
        false,
    ),
    spec(
        "msg-edit-last",
        "Edit last message",
        &["up"],
        "↑",
        Some("Composer"),
        Some(MessageAct::EditLast),
        false,
        false,
    ),
];

fn find(id: &str) -> Option<&'static Spec> {
    CATALOG.iter().find(|spec| spec.id == id)
}

fn on_this_os(spec: &Spec) -> bool {
    !spec.mac_only || cfg!(target_os = "macos")
}

fn full_screen_key() -> &'static str {
    if cfg!(target_os = "macos") {
        "ctrl-cmd-f"
    } else {
        "ctrl-alt-f"
    }
}

fn default_keys(spec: &Spec) -> Vec<String> {
    if spec.id == "full-screen" {
        return vec![full_screen_key().to_owned()];
    }
    spec.keys.iter().map(|key| (*key).to_owned()).collect()
}

fn named_nav(key: &str) -> bool {
    matches!(
        key,
        "escape"
            | "enter"
            | "tab"
            | "backspace"
            | "delete"
            | "up"
            | "down"
            | "left"
            | "right"
            | "home"
            | "end"
            | "pageup"
            | "pagedown"
    ) || key
        .strip_prefix('f')
        .and_then(|rest| rest.parse::<u8>().ok())
        .is_some_and(|n| (1..=24).contains(&n))
}

fn steals_typing(stroke: &Keystroke) -> bool {
    if stroke.modifiers.control
        || stroke.modifiers.alt
        || stroke.modifiers.platform
        || stroke.modifiers.function
    {
        return false;
    }
    !named_nav(&stroke.key)
}

fn same_stroke(a: &Keystroke, b: &Keystroke) -> bool {
    a.key.eq_ignore_ascii_case(&b.key)
        && a.modifiers.control == b.modifiers.control
        && a.modifiers.alt == b.modifiers.alt
        && a.modifiers.shift == b.modifiers.shift
        && a.modifiers.platform == b.modifiers.platform
        && a.modifiers.function == b.modifiers.function
}

fn overlaps(a: Option<&str>, b: Option<&str>) -> bool {
    match (a, b) {
        (None, _) | (_, None) => true,
        (Some(left), Some(right)) => left == right,
    }
}

fn glyph_key(key: &str) -> String {
    match key {
        "up" => "↑".to_owned(),
        "down" => "↓".to_owned(),
        "left" => "←".to_owned(),
        "right" => "→".to_owned(),
        "escape" => "Esc".to_owned(),
        "enter" | "return" => "Return".to_owned(),
        "tab" => "Tab".to_owned(),
        "backspace" => "⌫".to_owned(),
        "delete" => "Delete".to_owned(),
        "space" => "Space".to_owned(),
        "pageup" => "Page Up".to_owned(),
        "pagedown" => "Page Down".to_owned(),
        "-" | "minus" => "−".to_owned(),
        other if other.chars().count() == 1 => other.to_uppercase(),
        other => other.to_owned(),
    }
}

fn glyph_from_gpui(source: &str) -> String {
    let Ok(stroke) = Keystroke::parse(source) else {
        return source.to_owned();
    };
    let mut out = String::new();
    if stroke.modifiers.control {
        out.push('⌃');
    }
    if stroke.modifiers.alt {
        out.push('⌥');
    }
    if stroke.modifiers.shift {
        out.push('⇧');
    }
    if stroke.modifiers.platform {
        out.push('⌘');
    }
    out.push_str(&glyph_key(&stroke.key));
    out
}

fn sign_in_id(network: protocol::Network) -> &'static str {
    match network {
        protocol::Network::Telegram => "sign-in-telegram",
        protocol::Network::Discord => "sign-in-discord",
        protocol::Network::Slack => "sign-in-slack",
        protocol::Network::Teams => "sign-in-teams",
        protocol::Network::Signal => "sign-in-signal",
        protocol::Network::Messages => "sign-in-messages",
        protocol::Network::Tiny => "sign-in-tiny",
    }
}

fn listed() -> Vec<Command> {
    let mut out = Vec::new();
    for spec in CATALOG.iter().filter(|spec| {
        spec.palette && spec.act.is_none() && !spec.command.is_empty() && on_this_os(spec)
    }) {
        let run = if spec.id == "mute" {
            CommandRun::Mute
        } else {
            CommandRun::Binding(spec.id)
        };
        out.push(Command {
            id: spec.id,
            title: spec.command.to_owned(),
            detail: "Shortcut",
            run,
        });
    }
    let status = [
        ("status-online", "Set status: Online", "online"),
        ("status-idle", "Set status: Idle", "idle"),
        ("status-dnd", "Set status: Do not disturb", "dnd"),
        ("status-clear", "Set status: Clear", "clear"),
    ];
    for (id, title, word) in status {
        out.push(Command {
            id,
            title: title.to_owned(),
            detail: "Status",
            run: CommandRun::Status(word),
        });
    }
    let pause = [
        ("pause-hour", "Pause notifications for 1 hour", "hour"),
        (
            "pause-morning",
            "Pause notifications until tomorrow morning",
            "morning",
        ),
        (
            "pause-resume",
            "Pause notifications until I resume",
            "resume",
        ),
        ("pause-clear", "Resume notifications", "clear"),
    ];
    for (id, title, word) in pause {
        out.push(Command {
            id,
            title: title.to_owned(),
            detail: "Notifications",
            run: CommandRun::Pause(word),
        });
    }
    out.push(Command {
        id: "toggle-mute",
        title: "Toggle mute".to_owned(),
        detail: "Call",
        run: CommandRun::Mute,
    });
    for network in protocol::Network::ALL {
        out.push(Command {
            id: sign_in_id(network),
            title: format!("Sign in to {}", network.label()),
            detail: "Account",
            run: CommandRun::SignIn(network),
        });
    }
    for name in [
        "Accounts",
        "General",
        "Notifications",
        "Sounds",
        "Appearance",
        "Messages",
        "Privacy",
        "Audio & video",
        "Voice",
        "Storage",
        "Shortcuts",
        "About",
    ] {
        out.push(Command {
            id: match name {
                "Accounts" => "settings-accounts",
                "General" => "settings-general",
                "Notifications" => "settings-notifications",
                "Sounds" => "settings-sounds",
                "Appearance" => "settings-appearance",
                "Messages" => "settings-messages",
                "Privacy" => "settings-privacy",
                "Audio & video" => "settings-audio",
                "Voice" => "settings-voice",
                "Storage" => "settings-storage",
                "Shortcuts" => "settings-shortcuts",
                _ => "settings-about",
            },
            title: format!("Open settings: {name}"),
            detail: "Settings",
            run: CommandRun::OpenSettings(name),
        });
    }
    let mut menu = vec![
        ("about", "About tinyclient", CommandRun::Binding("about")),
        (
            "check-updates",
            "Check for Updates…",
            CommandRun::Binding("check-updates"),
        ),
        (
            "add-account",
            "Add Account…",
            CommandRun::Binding("add-account"),
        ),
        (
            "copy-diagnostics",
            "Copy Diagnostics",
            CommandRun::Binding("copy-diagnostics"),
        ),
        ("star", "Star", CommandRun::Binding("star")),
        (
            "conversation-mute",
            "Mute…",
            CommandRun::Binding("conversation-mute"),
        ),
    ];
    if cfg!(target_os = "macos") {
        menu.push(("show-all", "Show All", CommandRun::Binding("show-all")));
        menu.push(("zoom", "Zoom", CommandRun::Binding("zoom")));
    }
    for (id, title, run) in menu {
        out.push(Command {
            id,
            title: title.to_owned(),
            detail: "Menu",
            run,
        });
    }
    out
}

/// How the saved file spells a command with no key.
const NONE: &str = "none";

#[derive(Clone, Debug)]
pub struct Keymap {
    overrides: BTreeMap<String, String>,
}

impl Keymap {
    pub fn stock() -> Self {
        Self {
            overrides: BTreeMap::new(),
        }
    }

    pub fn rebind(&mut self, id: &str, key: &str) -> Result<(), BindError> {
        let spec = find(id)
            .filter(|spec| on_this_os(spec))
            .ok_or(BindError::Unknown)?;
        let parsed = Keystroke::parse(key).map_err(|_| BindError::Types)?;
        if default_keys(spec).iter().any(|default| {
            default == key
                || Keystroke::parse(default).is_ok_and(|have| same_stroke(&parsed, &have))
        }) {
            self.overrides.remove(id);
            return Ok(());
        }
        if spec.act.is_none() && steals_typing(&parsed) {
            return Err(BindError::Types);
        }
        if let Some(name) = self.conflict(id, &parsed, spec.context) {
            return Err(BindError::Conflict(name));
        }
        self.overrides.insert(id.to_owned(), key.to_owned());
        Ok(())
    }

    fn conflict(
        &self,
        id: &str,
        stroke: &Keystroke,
        context: Option<&str>,
    ) -> Option<&'static str> {
        for other in CATALOG
            .iter()
            .filter(|other| other.id != id && on_this_os(other))
        {
            if !overlaps(context, other.context) {
                continue;
            }
            for key in self.keys(other.id) {
                if Keystroke::parse(&key)
                    .ok()
                    .is_some_and(|theirs| same_stroke(stroke, &theirs))
                {
                    return Some(if other.command.is_empty() {
                        other.id
                    } else {
                        other.command
                    });
                }
            }
        }
        None
    }

    /// Leaves `id` with no key at all. A command that never had one has nothing to remove.
    pub fn unbind(&mut self, id: &str) -> Result<(), BindError> {
        let spec = find(id)
            .filter(|spec| on_this_os(spec))
            .ok_or(BindError::Unknown)?;
        if default_keys(spec).is_empty() {
            self.overrides.remove(id);
        } else {
            self.overrides.insert(id.to_owned(), String::new());
        }
        Ok(())
    }

    pub fn is_unbound(&self, id: &str) -> bool {
        self.overrides.get(id).is_some_and(String::is_empty)
    }

    pub fn reset(&mut self) {
        self.overrides.clear();
    }

    pub fn reset_one(&mut self, id: &str) {
        self.overrides.remove(id);
    }

    pub fn chord(&self, id: &str) -> String {
        if let Some(key) = self.overrides.get(id) {
            return key.clone();
        }
        find(id)
            .map(|spec| default_keys(spec).into_iter().next().unwrap_or_default())
            .unwrap_or_default()
    }

    pub fn keys(&self, id: &str) -> Vec<String> {
        if let Some(key) = self.overrides.get(id) {
            return if key.is_empty() { Vec::new() } else { vec![key.clone()] };
        }
        find(id).map(default_keys).unwrap_or_default()
    }

    pub fn glyph(&self, id: &str) -> String {
        if let Some(key) = self.overrides.get(id) {
            return if key.is_empty() { String::new() } else { glyph_from_gpui(key) };
        }
        find(id)
            .map(|spec| spec.glyph.to_owned())
            .unwrap_or_default()
    }

    pub fn menu_label(&self, command: &str) -> String {
        let Some(spec) = CATALOG.iter().find(|spec| spec.command == command) else {
            return String::new();
        };
        super::shortcuts::label(&self.glyph(spec.id))
    }

    pub fn commands(&self, query: &str) -> Vec<Command> {
        let _ = &self.overrides;
        let needle = query.trim().trim_start_matches('>').trim().to_lowercase();
        listed()
            .into_iter()
            .filter(|command| needle.is_empty() || command.title.to_lowercase().contains(&needle))
            .collect()
    }

    pub fn message_action(
        &self,
        stroke: &Keystroke,
        focus: Focus,
        composing: bool,
    ) -> Option<MessageAct> {
        if composing || matches!(focus, Focus::Text | Focus::Other) {
            return None;
        }
        for spec in CATALOG.iter().filter(|spec| spec.act.is_some()) {
            let act = spec.act.unwrap();
            let fits = match (act, focus) {
                (MessageAct::EditLast, Focus::Composer { empty: true }) => true,
                (MessageAct::EditLast, _) => false,
                (_, Focus::Message) => true,
                _ => false,
            };
            if !fits {
                continue;
            }
            for key in self.keys(spec.id) {
                if Keystroke::parse(&key)
                    .ok()
                    .is_some_and(|bound| same_stroke(stroke, &bound))
                {
                    return Some(act);
                }
            }
        }
        None
    }

    fn path() -> std::path::PathBuf {
        protocol::support_dir().join("keymap.txt")
    }

    fn save(&self) {
        let path = Self::path();
        if self.overrides.is_empty() {
            let _ = std::fs::remove_file(path);
            return;
        }
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, self.render());
    }

    /// One `id<TAB>key` line per override. No key is written `none`: a bare trailing tab
    /// would be trimmed away when the file is read back.
    fn render(&self) -> String {
        let mut body = String::new();
        for (id, key) in &self.overrides {
            body.push_str(id);
            body.push('\t');
            body.push_str(if key.is_empty() { NONE } else { key });
            body.push('\n');
        }
        body
    }

    fn parse(text: &str) -> Keymap {
        let mut map = Keymap::stock();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((id, key)) = line.split_once('\t') else {
                continue;
            };
            let _ = if key == NONE { map.unbind(id) } else { map.rebind(id, key) };
        }
        map
    }
}

fn slot() -> &'static Mutex<Keymap> {
    static LIVE: OnceLock<Mutex<Keymap>> = OnceLock::new();
    LIVE.get_or_init(|| Mutex::new(Keymap::stock()))
}

fn lock() -> std::sync::MutexGuard<'static, Keymap> {
    slot().lock().unwrap_or_else(|poison| poison.into_inner())
}

pub(crate) fn with_live<T>(f: impl FnOnce(&Keymap) -> T) -> T {
    f(&lock())
}

/// The chords to bind for `id` on this computer. Empty when the command is not on this OS.
pub(crate) fn chords(id: &str) -> Vec<String> {
    if !find(id).is_some_and(on_this_os) {
        return Vec::new();
    }
    with_live(|map| {
        map.keys(id)
            .into_iter()
            .filter(|key| !key.is_empty())
            .collect()
    })
}

pub(crate) fn load() {
    let Ok(text) = std::fs::read_to_string(Keymap::path()) else {
        return;
    };
    *lock() = Keymap::parse(&text);
}

pub(crate) fn dirty() -> bool {
    with_live(|map| !map.overrides.is_empty())
}

pub(crate) fn live_rebind(id: &str, key: &str) -> Result<(), BindError> {
    let mut map = lock();
    let result = map.rebind(id, key);
    if result.is_ok() {
        map.save();
    }
    result
}

pub(crate) fn live_unbind(id: &str) -> Result<(), BindError> {
    let mut map = lock();
    let result = map.unbind(id);
    if result.is_ok() {
        map.save();
    }
    result
}

pub(crate) fn is_unbound(id: &str) -> bool {
    with_live(|map| map.is_unbound(id))
}

pub(crate) fn live_reset() {
    let mut map = lock();
    map.reset();
    map.save();
}

pub(crate) fn live_reset_one(id: &str) {
    let mut map = lock();
    map.reset_one(id);
    map.save();
}

pub(crate) fn id_for(command: &str) -> Option<&'static str> {
    CATALOG
        .iter()
        .find(|spec| spec.command == command && on_this_os(spec))
        .map(|spec| spec.id)
}

pub(crate) fn glyph_for(id: &str) -> String {
    with_live(|map| map.glyph(id))
}

pub(crate) fn place_id(index: usize) -> Option<&'static str> {
    match index {
        0 => Some("place-1"),
        1 => Some("place-2"),
        2 => Some("place-3"),
        3 => Some("place-4"),
        4 => Some("place-5"),
        5 => Some("place-6"),
        6 => Some("place-7"),
        7 => Some("place-8"),
        8 => Some("place-9"),
        _ => None,
    }
}

pub(crate) fn message_rows() -> Vec<ShownRow> {
    with_live(|map| {
        CATALOG
            .iter()
            .filter(|spec| spec.act.is_some())
            .map(|spec| ShownRow {
                id: spec.id,
                title: spec.command,
                glyph: map.glyph(spec.id),
            })
            .collect()
    })
}

pub(crate) fn run_of(id: &str) -> Option<CommandRun> {
    listed()
        .into_iter()
        .find(|command| command.id == id)
        .map(|command| command.run)
        .or_else(|| find(id).map(|spec| CommandRun::Binding(spec.id)))
}

pub(crate) fn prove() -> Result<(), &'static str> {
    let mut map = Keymap::stock();
    let before = map.menu_label("Quick Switcher");
    map.rebind("quick-switcher", "secondary-shift-p")
        .map_err(|_| "rebinding Quick Switcher was refused")?;
    let after = map.menu_label("Quick Switcher");
    if before == after || map.chord("quick-switcher") != "secondary-shift-p" {
        return Err("rebinding Quick Switcher did not move the binding and the menu label");
    }
    let binding = KeyBinding::new(&map.chord("quick-switcher"), super::Switcher, None);
    let stroke = Keystroke::parse("secondary-shift-p").unwrap();
    if binding.match_keystrokes(&[stroke]) != Some(false) {
        return Err("the Quick Switcher binding does not match the new chord");
    }
    if map.rebind("quick-switcher", &map.chord("quit")).is_ok() {
        return Err("a chord another command uses was accepted");
    }
    map.reset();
    if map.chord("quick-switcher") != "secondary-k" {
        return Err("reset did not put Quick Switcher back");
    }
    if !map
        .commands(">status")
        .iter()
        .any(|command| command.title.to_lowercase().contains("status"))
    {
        return Err("the command list has no status command");
    }
    let letter = Keystroke::parse("e").unwrap();
    if map.message_action(&letter, Focus::Message, false) != Some(MessageAct::Edit) {
        return Err("E on a message does not edit");
    }
    if map.message_action(&letter, Focus::Text, false).is_some()
        || map.message_action(&letter, Focus::Message, true).is_some()
    {
        return Err("E edits while typing or while an input method is composing");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rebinding_quick_switcher_moves_the_binding_and_the_menu_label() {
        prove().unwrap();
    }

    #[test]
    fn an_unbound_command_has_no_key_frees_its_default_and_resets_back() {
        let mut map = Keymap::stock();
        let stock = map.chord("quick-switcher");
        assert!(!stock.is_empty());
        map.unbind("quick-switcher").unwrap();
        assert!(map.is_unbound("quick-switcher"));
        assert!(map.keys("quick-switcher").is_empty());
        assert_eq!(map.glyph("quick-switcher"), "");
        // Its old key is free for another command.
        assert!(map.rebind("msg-edit", &stock).is_ok());
        map.reset_one("quick-switcher");
        assert_eq!(map.chord("quick-switcher"), stock);
        // Binding it to its own default key puts it back too.
        map.unbind("quick-switcher").unwrap();
        assert!(map.rebind("quick-switcher", &stock).is_ok());
        assert!(!map.is_unbound("quick-switcher"));
        assert!(map.unbind("no-such-command").is_err());
    }

    #[test]
    fn no_key_survives_the_saved_file() {
        let mut map = Keymap::stock();
        map.unbind("quick-switcher").unwrap();
        map.rebind("msg-edit", "x").unwrap();
        let text = map.render();
        assert!(text.contains("quick-switcher\tnone\n"), "{text}");
        let back = Keymap::parse(&text);
        assert!(back.is_unbound("quick-switcher"));
        assert_eq!(back.keys("msg-edit"), ["x"]);
        assert_eq!(Keymap::parse("quick-switcher\t\n").overrides.len(), 0);
    }

    #[test]
    fn a_bare_letter_is_refused_on_a_shortcut_and_allowed_on_a_message() {
        let mut map = Keymap::stock();
        assert!(matches!(
            map.rebind("quick-switcher", "e"),
            Err(BindError::Types)
        ));
        assert!(map.rebind("msg-edit", "x").is_ok());
        let stroke = Keystroke::parse("x").unwrap();
        assert_eq!(
            map.message_action(&stroke, Focus::Message, false),
            Some(MessageAct::Edit)
        );
        assert!(map
            .message_action(&stroke, Focus::Composer { empty: true }, false)
            .is_none());
        let up = Keystroke::parse("up").unwrap();
        assert_eq!(
            map.message_action(&up, Focus::Composer { empty: true }, false),
            Some(MessageAct::EditLast)
        );
        assert!(map
            .message_action(&up, Focus::Composer { empty: false }, false)
            .is_none());
    }
}
