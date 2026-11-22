use super::{Action, Binding, Key};

/// XDG's trigger syntax uses XKB key names. It has no portable mouse-button trigger.
pub(super) fn preferred_trigger(binding: Binding) -> Option<String> {
    let mut trigger = String::new();
    for (held, name) in [
        (binding.modifiers.ctrl, "CTRL+"),
        (binding.modifiers.alt, "ALT+"),
        (binding.modifiers.shift, "SHIFT+"),
        (binding.modifiers.meta, "LOGO+"),
    ] {
        if held {
            trigger.push_str(name);
        }
    }
    let key = match binding.key {
        Key::Letter(c) | Key::Digit(c) => {
            trigger.push(c);
            return Some(trigger);
        }
        Key::F(n) => {
            trigger.push_str(&format!("F{n}"));
            return Some(trigger);
        }
        Key::Space => "space",
        Key::Tab => "Tab",
        Key::Enter => "Return",
        Key::Backspace => "BackSpace",
        Key::Insert => "Insert",
        Key::Delete => "Delete",
        Key::Home => "Home",
        Key::End => "End",
        Key::PageUp => "Page_Up",
        Key::PageDown => "Page_Down",
        Key::Left => "Left",
        Key::Right => "Right",
        Key::Up => "Up",
        Key::Down => "Down",
        Key::CapsLock => "Caps_Lock",
        Key::Ctrl => "Control_L",
        Key::Alt => "Alt_L",
        Key::Shift => "Shift_L",
        Key::Meta => "Super_L",
        Key::Punctuation(c) => match c {
            '`' => "grave",
            '-' => "minus",
            '=' => "equal",
            '[' => "bracketleft",
            ']' => "bracketright",
            '\\' => "backslash",
            ';' => "semicolon",
            '\'' => "apostrophe",
            ',' => "comma",
            '.' => "period",
            '/' => "slash",
            _ => return None,
        },
        Key::Mouse3 | Key::Mouse4 | Key::Mouse5 => return None,
    };
    trigger.push_str(key);
    Some(trigger)
}

#[derive(Default)]
pub(super) struct Keys {
    physical: [bool; 4],
    talk: [bool; 2],
}

impl Keys {
    /// Portal signals come in press/release pairs. Suppress repeats and presses made during
    /// Settings recording, and never let a release be lost when the recorder opens.
    pub(super) fn edge(&mut self, action: Action, down: bool, paused: bool) -> Option<bool> {
        let at = action as usize;
        if std::mem::replace(&mut self.physical[at], down) == down {
            return None;
        }
        if let Some(talking) = self.talk.get_mut(at) {
            let active = down && !paused;
            return (std::mem::replace(talking, active) != active).then_some(active);
        }
        (down && !paused).then_some(true)
    }

    pub(super) fn release_talk(&mut self) -> [bool; 2] {
        std::mem::take(&mut self.talk)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferred_trigger_uses_xkb_names_and_declines_mouse_buttons() {
        let trigger = |text| preferred_trigger(Binding::parse(text).unwrap());
        assert_eq!(
            trigger("ctrl+shift+meta+f13").as_deref(),
            Some("CTRL+SHIFT+LOGO+F13")
        );
        assert_eq!(trigger("alt+enter").as_deref(), Some("ALT+Return"));
        assert_eq!(trigger("ctrl+;").as_deref(), Some("CTRL+semicolon"));
        assert_eq!(trigger("mouse4"), None);
        assert_eq!(trigger("mouse5"), None);
    }

    #[test]
    fn talk_and_priority_each_send_one_press_and_release() {
        let mut keys = Keys::default();
        assert_eq!(keys.edge(Action::Talk, true, false), Some(true));
        assert_eq!(keys.edge(Action::Talk, true, false), None);
        assert_eq!(keys.edge(Action::Priority, true, false), Some(true));
        assert_eq!(keys.edge(Action::Talk, false, false), Some(false));
        assert_eq!(keys.edge(Action::Priority, false, false), Some(false));
        assert_eq!(keys.edge(Action::Talk, false, false), None);
    }

    #[test]
    fn pausing_releases_talk_and_ignores_the_rest_of_its_hold() {
        let mut keys = Keys::default();
        assert_eq!(keys.edge(Action::Talk, true, false), Some(true));
        assert_eq!(keys.release_talk(), [true, false]);
        assert_eq!(keys.edge(Action::Talk, true, true), None);
        assert_eq!(keys.edge(Action::Talk, true, false), None);
        assert_eq!(keys.edge(Action::Talk, false, false), None);
        assert_eq!(keys.edge(Action::Talk, true, false), Some(true));
    }

    #[test]
    fn mute_toggles_only_once_per_activation() {
        let mut keys = Keys::default();
        assert_eq!(keys.edge(Action::Mute, true, false), Some(true));
        assert_eq!(keys.edge(Action::Mute, true, false), None);
        assert_eq!(keys.edge(Action::Mute, false, false), None);
        assert_eq!(keys.edge(Action::Mute, true, true), None);
        assert_eq!(keys.edge(Action::Mute, false, false), None);
        assert_eq!(keys.edge(Action::Mute, true, false), Some(true));
    }
}
