//! A key the shell watches for anywhere, focused or not (Settings › Voice, V8 and V13 to V15):
//! push-to-talk, priority push-to-talk, toggle mute, toggle deafen.
//!
//! Stored as text (`settings::PTT_KEY` and the rest): lower case, `+` between the parts,
//! modifiers first in the order `ctrl`, `alt`, `shift`, `meta`, then one key: `f13`,
//! `ctrl+shift+m`, `mouse4`. A modifier can be the key itself (`ctrl`), which is how a whole
//! key is given to push-to-talk. Each platform's shell maps [`Key`] to its own codes.

use std::fmt;

/// Every key a binding can name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    /// `a` to `z`.
    Letter(char),
    /// `0` to `9`, on the main row.
    Digit(char),
    /// F1 to F24.
    F(u8),
    Space,
    Tab,
    Enter,
    Backspace,
    Insert,
    Delete,
    Home,
    End,
    PageUp,
    PageDown,
    Up,
    Down,
    Left,
    Right,
    CapsLock,
    /// `` ` - = [ ] \ ; ' , . / ``, by the character on a US keyboard.
    Punctuation(char),
    /// A modifier held on its own, either side.
    Ctrl,
    Alt,
    Shift,
    Meta,
    /// The middle button, and the back and forward side buttons.
    Mouse3,
    Mouse4,
    Mouse5,
}

const PUNCTUATION: &str = "`-=[]\\;',./";

const NAMED: [(&str, Key); 22] = [
    ("space", Key::Space),
    ("tab", Key::Tab),
    ("enter", Key::Enter),
    ("backspace", Key::Backspace),
    ("insert", Key::Insert),
    ("delete", Key::Delete),
    ("home", Key::Home),
    ("end", Key::End),
    ("pageup", Key::PageUp),
    ("pagedown", Key::PageDown),
    ("up", Key::Up),
    ("down", Key::Down),
    ("left", Key::Left),
    ("right", Key::Right),
    ("capslock", Key::CapsLock),
    ("ctrl", Key::Ctrl),
    ("alt", Key::Alt),
    ("shift", Key::Shift),
    ("meta", Key::Meta),
    ("mouse3", Key::Mouse3),
    ("mouse4", Key::Mouse4),
    ("mouse5", Key::Mouse5),
];

impl Key {
    pub fn parse(name: &str) -> Option<Key> {
        if let Some((_, key)) = NAMED.iter().find(|(word, _)| *word == name) {
            return Some(*key);
        }
        let mut chars = name.chars();
        let (first, rest) = (chars.next()?, chars.as_str());
        if rest.is_empty() {
            return match first {
                'a'..='z' => Some(Key::Letter(first)),
                '0'..='9' => Some(Key::Digit(first)),
                c if PUNCTUATION.contains(c) => Some(Key::Punctuation(c)),
                _ => None,
            };
        }
        let n: u8 = name.strip_prefix('f')?.parse().ok()?;
        (1..=24).contains(&n).then_some(Key::F(n))
    }

    pub fn name(self) -> String {
        match self {
            Key::Letter(c) | Key::Digit(c) | Key::Punctuation(c) => c.to_string(),
            Key::F(n) => format!("f{n}"),
            other => NAMED.iter().find(|(_, key)| *key == other).map(|(word, _)| (*word).to_owned()).unwrap_or_default(),
        }
    }

    /// A mouse button rather than a key.
    pub fn is_mouse(self) -> bool {
        matches!(self, Key::Mouse3 | Key::Mouse4 | Key::Mouse5)
    }

    fn label(self, mac: bool) -> String {
        match self {
            Key::Letter(c) => c.to_ascii_uppercase().to_string(),
            Key::Digit(c) | Key::Punctuation(c) => c.to_string(),
            Key::F(n) => format!("F{n}"),
            Key::Space => "Space".into(),
            Key::Tab => "Tab".into(),
            Key::Enter => if mac { "Return" } else { "Enter" }.into(),
            Key::Backspace => if mac { "Delete" } else { "Backspace" }.into(),
            Key::Insert => "Insert".into(),
            Key::Delete => if mac { "Forward Delete" } else { "Delete" }.into(),
            Key::Home => "Home".into(),
            Key::End => "End".into(),
            Key::PageUp => "Page Up".into(),
            Key::PageDown => "Page Down".into(),
            Key::Up => "↑".into(),
            Key::Down => "↓".into(),
            Key::Left => "←".into(),
            Key::Right => "→".into(),
            Key::CapsLock => "Caps Lock".into(),
            Key::Ctrl => if mac { "Control" } else { "Ctrl" }.into(),
            Key::Alt => if mac { "Option" } else { "Alt" }.into(),
            Key::Shift => "Shift".into(),
            Key::Meta => if mac { "Command" } else { "Windows" }.into(),
            Key::Mouse3 => "Middle button".into(),
            Key::Mouse4 => "Mouse 4".into(),
            Key::Mouse5 => "Mouse 5".into(),
        }
    }
}

/// The modifiers that must be held with a binding's key. Others held as well do not stop it,
/// so push-to-talk keeps working while Shift is down for typing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub meta: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Binding {
    pub modifiers: Modifiers,
    pub key: Key,
}

impl Binding {
    pub fn new(key: Key) -> Binding {
        Binding { modifiers: Modifiers::default(), key }
    }

    /// Reads what [`fmt::Display`] writes. Anything else, including a modifier named twice or
    /// out of order, is `None`.
    pub fn parse(text: &str) -> Option<Binding> {
        if text.is_empty() || text.len() > 40 {
            return None;
        }
        let mut parts: Vec<&str> = text.split('+').collect();
        let key = Key::parse(parts.pop()?)?;
        let mut modifiers = Modifiers::default();
        let mut rank = 0;
        for part in parts {
            let (at, flag) = match part {
                "ctrl" => (1, &mut modifiers.ctrl),
                "alt" => (2, &mut modifiers.alt),
                "shift" => (3, &mut modifiers.shift),
                "meta" => (4, &mut modifiers.meta),
                _ => return None,
            };
            if at <= rank {
                return None;
            }
            rank = at;
            *flag = true;
        }
        Some(Binding { modifiers, key })
    }

    /// As the Settings window shows it: `⌃⇧M` on macOS, `Ctrl+Shift+M` elsewhere.
    pub fn label(&self) -> String {
        let mac = cfg!(target_os = "macos");
        let m = self.modifiers;
        let held = [(m.ctrl, "⌃", "Ctrl"), (m.alt, "⌥", "Alt"), (m.shift, "⇧", "Shift"), (m.meta, "⌘", "Win")];
        let mut out = String::new();
        for (on, symbol, word) in held {
            if on {
                if mac {
                    out.push_str(symbol);
                } else {
                    out.push_str(word);
                    out.push('+');
                }
            }
        }
        out + &self.key.label(mac)
    }
}

impl fmt::Display for Binding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let m = self.modifiers;
        for (on, word) in [(m.ctrl, "ctrl"), (m.alt, "alt"), (m.shift, "shift"), (m.meta, "meta")] {
            if on {
                write!(f, "{word}+")?;
            }
        }
        f.write_str(&self.key.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_reads_back_what_it_writes() {
        let mut keys: Vec<Key> = NAMED.iter().map(|(_, key)| *key).collect();
        keys.extend(('a'..='z').map(Key::Letter));
        keys.extend(('0'..='9').map(Key::Digit));
        keys.extend((1..=24).map(Key::F));
        keys.extend(PUNCTUATION.chars().map(Key::Punctuation));
        for key in keys {
            assert_eq!(Key::parse(&key.name()), Some(key), "{key:?}");
            let binding = Binding { modifiers: Modifiers { ctrl: true, shift: true, ..Modifiers::default() }, key };
            assert_eq!(Binding::parse(&binding.to_string()), Some(binding));
        }
    }

    #[test]
    fn a_binding_is_modifiers_in_order_then_one_key() {
        let b = Binding::parse("ctrl+alt+shift+meta+f13").unwrap();
        assert!(b.modifiers.ctrl && b.modifiers.alt && b.modifiers.shift && b.modifiers.meta);
        assert_eq!(b.key, Key::F(13));
        assert_eq!(Binding::parse("mouse4"), Some(Binding::new(Key::Mouse4)));
        assert_eq!(Binding::parse("ctrl"), Some(Binding::new(Key::Ctrl)));
        assert_eq!(Binding::parse("ctrl+="), Some(Binding { modifiers: Modifiers { ctrl: true, ..Modifiers::default() }, key: Key::Punctuation('=') }));
        for bad in ["", "f25", "f0", "shift+ctrl+a", "ctrl+ctrl+a", "hyper+a", "A", "ctrl+", "+", "escape", "mouse6"] {
            assert_eq!(Binding::parse(bad), None, "{bad}");
        }
        assert!(Key::Mouse5.is_mouse() && !Key::Ctrl.is_mouse());
    }

    #[test]
    fn its_label_is_in_words_people_read() {
        let label = Binding::parse("ctrl+shift+m").unwrap().label();
        if cfg!(target_os = "macos") {
            assert_eq!(label, "⌃⇧M");
        } else {
            assert_eq!(label, "Ctrl+Shift+M");
        }
        assert_eq!(Binding::new(Key::Mouse4).label(), "Mouse 4");
        assert_eq!(Binding::new(Key::F(13)).label(), "F13");
    }
}
