//! What only the window remembers between launches: its frame, pane widths, rail pins, the
//! last conversation and sidebar mode, the two Details switches, the text size, when GIFs
//! play, and the settings only the window acts on (`Choices`, `docs/product/09-settings.md`).
//! One small tab-separated file beside chatd's, read once before the window opens (so the first frame
//! draws where the user was) and written off the main thread. A line this build does not know
//! is kept as it was, so an older build never erases a newer one's choices.
// CONTRACT: the protocol has no home for what only the window knows, so it is a file of its own.

use super::super::anim::Autoplay;
use super::super::layout;
use super::super::settings::Section;
use super::super::sidebar::Mode;
use protocol::settings::Values;
use protocol::{Key, Network};
use std::sync::Mutex;

pub const FILE: &str = "shell.prefs";

/// The settings the window keeps (Appearance, Messages, P3) and the Settings section last open.
/// Only what differs from these defaults is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Choices {
    /// A3: list rows 40 instead of 48.
    pub compact: bool,
    /// A4: list rows show the last message.
    pub previews: bool,
    /// M1: ⌘Return sends and Return adds a line.
    pub command_return: bool,
    /// M2: a conversation opens at its newest message, not the first unread.
    pub newest: bool,
    /// M3: others typing are shown.
    pub typing: bool,
    /// M4: `:)` and friends become emoji when sent.
    pub emoticons: bool,
    /// P3: pictures and link previews load by themselves.
    pub images: bool,
    /// G4: a newer release is downloaded and installed on quit. Off, the notice still appears.
    pub updates: bool,
    /// The Settings section last shown. Written by its name, which stays when sections are
    /// added or moved; a name this build does not know opens Accounts.
    pub section: Section,
}

impl Default for Choices {
    fn default() -> Choices {
        Choices { compact: false, previews: true, command_return: false, newest: false, typing: true, emoticons: false, images: true, updates: true, section: Section::Accounts }
    }
}

// One window, one value each, read while drawing: globals, as the text step is.
static CHOICES: Mutex<Choices> = Mutex::new(Choices {
    compact: false,
    previews: true,
    command_return: false,
    newest: false,
    typing: true,
    emoticons: false,
    images: true,
    updates: true,
    section: Section::Accounts,
});
static VALUES: Mutex<Option<Values>> = Mutex::new(None);

pub fn choices() -> Choices {
    *CHOICES.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub fn set_choices(choices: Choices) {
    *CHOICES.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = choices;
    super::super::theme::set_compact(choices.compact);
}

/// chatd's settings as last read (`Request::Settings`). Defaults until the first answer.
pub fn values() -> Values {
    VALUES.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone().unwrap_or_default()
}

/// Whether chatd's settings have been read since the window opened.
pub fn values_known() -> bool {
    VALUES.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_some()
}

pub fn set_values(values: Values) {
    *VALUES.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(values);
}

#[derive(Clone, Debug, PartialEq)]
pub struct Prefs {
    pub sidebar: f32,
    /// The one right-pane width.
    pub right: f32,
    pub pins: Vec<Key>,
    /// The window's frame in screen points: x, y, width, height.
    pub frame: Option<(f32, f32, f32, f32)>,
    /// The conversation that was open, and its title for the strip until its row is read.
    pub open: Option<(Key, String)>,
    pub mode: Mode,
    /// The Details switch for channels and groups, and the one for DMs (rule 2).
    pub details_channels: bool,
    pub details_directs: bool,
    pub text_step: i32,
    pub choices: Choices,
    /// Lines this build does not know, as they were.
    pub kept: Vec<String>,
    pub autoplay: Autoplay,
    /// Each Tiny server person's volume and mute for me: server, user, percent, muted. Only
    /// those away from 100 % and heard.
    pub tiny_gains: Vec<(String, String, u16, bool)>,
    /// The Tiny server's member list is shown beside the pane.
    pub tiny_members: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            sidebar: layout::SIDEBAR_WIDTH,
            right: layout::RIGHT_WIDTH,
            pins: Vec::new(),
            frame: None,
            open: None,
            mode: Mode::Inbox,
            details_channels: false,
            details_directs: false,
            text_step: 0,
            choices: Choices::default(),
            kept: Vec::new(),
            autoplay: Autoplay::Always,
            tiny_gains: Vec::new(),
            tiny_members: true,
        }
    }
}

fn network_word(network: Network) -> &'static str {
    match network {
        Network::Telegram => "telegram",
        Network::Discord => "discord",
        Network::Slack => "slack",
        Network::Teams => "teams",
        Network::Signal => "signal",
        Network::Messages => "messages",
        Network::Tiny => "tiny",
    }
}

fn network(word: &str) -> Option<Network> {
    Network::ALL.into_iter().find(|n| network_word(*n) == word)
}

/// Fields are tab-separated, so a tab or newline inside one (a title) becomes a space.
fn clean(text: &str) -> String {
    text.replace(['\t', '\n', '\r'], " ")
}

fn key<'a>(mut fields: impl Iterator<Item = &'a str>) -> Option<Key> {
    let network = network(fields.next()?)?;
    let (account, id) = (fields.next()?, fields.next()?);
    Some(Key { network, account: account.to_owned(), id: id.to_owned() })
}

fn width(text: &str, range: (f32, f32), or: f32) -> f32 {
    text.parse::<f32>().ok().filter(|w| w.is_finite()).map_or(or, |w| w.clamp(range.0, range.1))
}

pub fn parse(text: &str) -> Prefs {
    let mut prefs = Prefs::default();
    for line in text.lines() {
        let mut fields = line.split('\t');
        let Some(name) = fields.next() else { continue };
        match name {
            "sidebar" => {
                prefs.sidebar = width(fields.next().unwrap_or(""), layout::SIDEBAR_RANGE, prefs.sidebar);
            }
            "right" => prefs.right = width(fields.next().unwrap_or(""), layout::RIGHT_RANGE, prefs.right),
            "pin" => prefs.pins.extend(key(fields)),
            "frame" => {
                let numbers: Vec<f32> = fields.filter_map(|n| n.parse().ok()).filter(|n: &f32| n.is_finite()).collect();
                if let [x, y, w, h] = numbers[..] {
                    let (min_w, min_h) = layout::WINDOW_MIN;
                    prefs.frame = Some((x, y, w.max(min_w), h.max(min_h)));
                }
            }
            "open" => {
                let mut fields = fields;
                let found = key(fields.by_ref());
                prefs.open = found.map(|key| (key, fields.next().unwrap_or("").to_owned()));
            }
            "mode" => {
                prefs.mode = match (fields.next(), fields.next()) {
                    (Some("people"), _) => Mode::People,
                    (Some("network"), Some(word)) => network(word).map_or(Mode::Inbox, Mode::Network),
                    (Some("space"), Some(word)) => {
                        let rest = std::iter::once(word).chain(fields);
                        key(rest).map_or(Mode::Inbox, Mode::Space)
                    }
                    _ => Mode::Inbox,
                };
            }
            "details" => {
                let on = |word: Option<&str>| word == Some("on");
                prefs.details_channels = on(fields.next());
                prefs.details_directs = on(fields.next());
            }
            "autoplay" => prefs.autoplay = fields.next().and_then(Autoplay::from_word).unwrap_or_default(),
            "text" => {
                prefs.text_step = fields.next().and_then(|n| n.parse::<i32>().ok()).unwrap_or(0).clamp(-1, 3);
            }
            // A value that does not parse reads as the default.
            "density" => prefs.choices.compact = fields.next() == Some("compact"),
            "previews" => prefs.choices.previews = fields.next() != Some("off"),
            "send" => prefs.choices.command_return = fields.next() == Some("cmd-return"),
            "open_at" => prefs.choices.newest = fields.next() == Some("newest"),
            "typing" => prefs.choices.typing = fields.next() != Some("off"),
            "emoticons" => prefs.choices.emoticons = fields.next() == Some("on"),
            "images" => prefs.choices.images = fields.next() != Some("click"),
            "updates" => prefs.choices.updates = fields.next() != Some("off"),
            "tiny_gain" => {
                let fields: Vec<&str> = fields.collect();
                if let [server, user, percent, muted] = fields[..] {
                    if let Ok(percent) = percent.parse::<u16>() {
                        prefs.tiny_gains.push((server.to_owned(), user.to_owned(), percent.min(protocol::GAIN_MAX), muted == "muted"));
                    }
                }
            }
            "tiny_members" => prefs.tiny_members = fields.next() != Some("off"),
            "settings_section" => prefs.choices.section = fields.next().and_then(Section::from_word).unwrap_or(Section::Accounts),
            _ if !line.trim().is_empty() => prefs.kept.push(line.to_owned()),
            _ => {}
        }
    }
    prefs
}

pub fn write(prefs: &Prefs) -> String {
    let mut text = format!("sidebar\t{}\nright\t{}\n", prefs.sidebar, prefs.right);
    let key = |key: &Key| format!("{}\t{}\t{}", network_word(key.network), clean(&key.account), clean(&key.id));
    for pin in &prefs.pins {
        text.push_str(&format!("pin\t{}\n", key(pin)));
    }
    if let Some((x, y, w, h)) = prefs.frame {
        text.push_str(&format!("frame\t{x}\t{y}\t{w}\t{h}\n"));
    }
    if let Some((open, title)) = &prefs.open {
        text.push_str(&format!("open\t{}\t{}\n", key(open), clean(title)));
    }
    match &prefs.mode {
        Mode::Inbox => text.push_str("mode\tinbox\n"),
        Mode::People => text.push_str("mode\tpeople\n"),
        Mode::Network(network) => text.push_str(&format!("mode\tnetwork\t{}\n", network_word(*network))),
        Mode::Space(space) => text.push_str(&format!("mode\tspace\t{}\n", key(space))),
    }
    let word = |on: bool| if on { "on" } else { "off" };
    text.push_str(&format!("details\t{}\t{}\n", word(prefs.details_channels), word(prefs.details_directs)));
    text.push_str(&format!("text\t{}\n", prefs.text_step));
    let (choices, default) = (prefs.choices, Choices::default());
    let lines: [(&str, bool, &str); 9] = [
        ("density", choices.compact != default.compact, "compact"),
        ("previews", choices.previews != default.previews, "off"),
        ("send", choices.command_return != default.command_return, "cmd-return"),
        ("open_at", choices.newest != default.newest, "newest"),
        ("typing", choices.typing != default.typing, "off"),
        ("emoticons", choices.emoticons != default.emoticons, "on"),
        ("images", choices.images != default.images, "click"),
        ("updates", choices.updates != default.updates, "off"),
        ("settings_section", choices.section != default.section, ""),
    ];
    for (name, changed, value) in lines {
        if !changed {
            continue;
        }
        match name {
            "settings_section" => text.push_str(&format!("{name}\t{}\n", choices.section.word())),
            _ => text.push_str(&format!("{name}\t{value}\n")),
        }
    }
    for line in &prefs.kept {
        text.push_str(line);
        text.push('\n');
    }
    for (server, user, percent, muted) in &prefs.tiny_gains {
        text.push_str(&format!("tiny_gain\t{}\t{}\t{percent}\t{}\n", clean(server), clean(user), if *muted { "muted" } else { "heard" }));
    }
    if !prefs.tiny_members {
        text.push_str("tiny_members\toff\n");
    }
    if prefs.autoplay != Autoplay::default() {
        text.push_str(&format!("autoplay\t{}\n", prefs.autoplay.word()));
    }
    text
}

/// Reads the file now. Small, and read once before the window opens.
pub fn read() -> Prefs {
    std::fs::read_to_string(protocol::support_dir().join(FILE)).map(|text| parse(&text)).unwrap_or_default()
}

/// Whether a saved frame may be used on these displays: at least half of it on one of them.
pub fn frame_visible(frame: (f32, f32, f32, f32), displays: &[(f32, f32, f32, f32)]) -> bool {
    let (x, y, w, h) = frame;
    displays.iter().any(|&(dx, dy, dw, dh)| {
        let across = (x + w).min(dx + dw) - x.max(dx);
        let down = (y + h).min(dy + dh) - y.max(dy);
        across > 0. && down > 0. && across * down >= w * h / 2.
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(network: Network, id: &str) -> Key {
        Key { network, account: "T1".into(), id: id.into() }
    }

    #[test]
    fn everything_the_window_remembers_survives_a_round_trip() {
        let saved = Prefs {
            sidebar: 300.,
            right: 380.,
            pins: vec![key(Network::Slack, "T1"), key(Network::Discord, "9")],
            frame: Some((120., 80., 1300., 800.)),
            open: Some((key(Network::Discord, "c1"), "#general\twith a tab".into())),
            mode: Mode::Space(key(Network::Discord, "9")),
            details_channels: true,
            details_directs: false,
            text_step: 2,
            choices: Choices { compact: true, previews: false, command_return: true, newest: true, typing: false, emoticons: true, images: false, updates: false, section: Section::Audio },
            kept: Vec::new(),
            autoplay: Autoplay::Hover,
            tiny_gains: vec![("srv".into(), "u1".into(), 150, false), ("srv".into(), "u2".into(), 100, true)],
            tiny_members: false,
        };
        let read = parse(&write(&saved));
        assert_eq!(read, Prefs { open: Some((key(Network::Discord, "c1"), "#general with a tab".into())), ..saved.clone() });
        for mode in [Mode::Inbox, Mode::People, Mode::Network(Network::Telegram)] {
            assert_eq!(parse(&write(&Prefs { mode: mode.clone(), ..Prefs::default() })).mode, mode);
        }
    }

    #[test]
    fn bad_lines_are_skipped_and_values_are_kept_in_range() {
        let odd = parse(
            "sidebar\t9000\nright\twide\npin\tmyspace\ta\tb\nnonsense\npin\tslack\tT1\tT1\nframe\t1\t2\t100\t100\ntext\t9\nmode\tspace\tnope\nautoplay\tsometimes\n",
        );
        assert_eq!(odd.sidebar, 420.);
        assert_eq!(odd.right, layout::RIGHT_WIDTH);
        assert_eq!(odd.pins, vec![key(Network::Slack, "T1")]);
        // A frame below the minimum grows to it; the text step stops at +3.
        assert_eq!(odd.frame, Some((1., 2., 540., 440.)));
        assert_eq!((odd.text_step, odd.mode, odd.autoplay), (3, Mode::Inbox, Autoplay::Always));
        assert_eq!(parse(""), Prefs::default());
    }

    #[test]
    fn only_changed_choices_are_written_and_lines_from_a_newer_build_are_kept() {
        let text = write(&Prefs::default());
        for name in ["density", "previews", "send", "open_at", "typing", "emoticons", "images", "settings_section"] {
            assert!(!text.contains(name), "{name} is at its default and not written");
        }
        let newer = "sidebar\t300\ntheme\tlight\nbounce\tonce\n";
        let read = parse(newer);
        assert_eq!(read.kept, ["theme\tlight", "bounce\tonce"]);
        let again = write(&read);
        assert!(again.contains("theme\tlight\n") && again.contains("bounce\tonce\n"), "{again}");
        // Junk reads as the default.
        assert_eq!(parse("density\tcosy\nsend\t?\nsettings_section\tx\n").choices, Choices::default());
        // A section is kept by its name: an index from an older build, or a section a newer
        // build added, opens Accounts rather than whatever sits at that place now.
        assert!(write(&Prefs { choices: Choices { section: Section::Privacy, ..Choices::default() }, ..Prefs::default() }).contains("settings_section\tprivacy\n"));
        assert_eq!(parse("settings_section\tstorage\n").choices.section, Section::Storage);
        assert_eq!(parse("settings_section\t6\n").choices.section, Section::Accounts);
        assert_eq!(parse("settings_section\tnotifications\n").choices.section, Section::Accounts);
    }

    #[test]
    fn a_frame_is_restored_only_when_half_of_it_is_on_a_display() {
        let main = [(0., 0., 1512., 982.)];
        assert!(frame_visible((100., 100., 1120., 720.), &main));
        // Mostly off the right edge: centered instead.
        assert!(!frame_visible((1200., 100., 1120., 720.), &main));
        // On a display that is gone.
        assert!(!frame_visible((-1900., 0., 1120., 720.), &main));
        assert!(frame_visible((-1900., 0., 1120., 720.), &[(0., 0., 1512., 982.), (-1920., 0., 1920., 1080.)]));
    }
}
