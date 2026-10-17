//! Native shortcut modifiers and labels. GPUI's `secondary` is Command on macOS
//! and Control elsewhere; Super is reserved for the Linux desktop.

pub fn primary(modifiers: gpui::Modifiers) -> bool {
    if cfg!(target_os = "macos") {
        modifiers.platform
    } else {
        modifiers.control
    }
}

pub fn label(source: &str) -> String {
    native_label(source, cfg!(target_os = "macos"))
}

fn native_label(source: &str, mac: bool) -> String {
    if mac {
        return source.to_owned();
    }
    let mut out = String::new();
    let mut chars = source.chars().peekable();
    while let Some(c) = chars.next() {
        if matches!(c, '⌘' | '⌃' | '⌥' | '⇧') {
            let mut modifiers = vec![c];
            while chars
                .peek()
                .is_some_and(|c| matches!(c, '⌘' | '⌃' | '⌥' | '⇧'))
            {
                modifiers.push(chars.next().unwrap());
            }
            let command = modifiers.contains(&'⌘');
            let control = modifiers.contains(&'⌃');
            if command || control {
                out.push_str("Ctrl+");
            }
            // Control+Command on Mac uses Ctrl+Alt elsewhere, avoiding Ctrl+Ctrl.
            if modifiers.contains(&'⌥') || (command && control) {
                out.push_str("Alt+");
            }
            if modifiers.contains(&'⇧') {
                out.push_str("Shift+");
            }
            if chars.peek().is_none() && out.ends_with('+') {
                out.pop();
            }
        } else {
            match c {
                '⎋' => out.push_str("Esc"),
                '↩' => out.push_str("Enter"),
                '↑' => out.push_str("Up"),
                '↓' => out.push_str("Down"),
                '←' => out.push_str("Left"),
                '→' => out.push_str("Right"),
                '−' => out.push('-'),
                _ => out.push(c),
            }
        }
    }
    out.replace("Return", "Enter")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_labels_cover_menus_tooltips_and_sentences() {
        for (mac, native) in [
            ("⌘K", "Ctrl+K"),
            ("⇧⌘G", "Ctrl+Shift+G"),
            ("⌥⌘I", "Ctrl+Alt+I"),
            ("⌃⌘F", "Ctrl+Alt+F"),
            ("⇧⎋", "Shift+Esc"),
            ("⌥↑", "Alt+Up"),
            ("⌥", "Alt"),
            ("⌘Return", "Ctrl+Enter"),
            ("⌘K jumps anywhere.", "Ctrl+K jumps anywhere."),
        ] {
            assert_eq!(native_label(mac, false), native);
            assert_eq!(native_label(mac, true), mac);
        }
    }

    #[test]
    fn secondary_shortcuts_use_the_native_primary_modifier() {
        let keys = gpui::Keystroke::parse("secondary-k").unwrap();
        assert!(primary(keys.modifiers));
        assert_eq!(keys.modifiers.control, !cfg!(target_os = "macos"));
        assert_eq!(keys.modifiers.platform, cfg!(target_os = "macos"));
        assert_eq!(keys.key, "k");
        let binding = crate::gui::shell_bindings()
            .into_iter()
            .find(|b| b.match_keystrokes(&[keys.clone()]) == Some(false))
            .unwrap();
        assert!(binding.action().name().ends_with("Switcher"));
    }
    #[test]
    #[cfg(not(target_os = "macos"))]
    fn input_keeps_ctrl_k_and_app_keys_free_and_supports_native_editing() {
        let bindings = crate::gui::input::text_bindings();
        for key in ["ctrl-k", "ctrl-f", "ctrl-n", "ctrl-w", "ctrl-e", "ctrl-u"] {
            let stroke = gpui::Keystroke::parse(key).unwrap();
            assert!(
                bindings
                    .iter()
                    .all(|binding| binding.match_keystrokes(&[stroke.clone()]).is_none()),
                "{key} shadows an application shortcut"
            );
        }
        for (key, action) in [
            ("ctrl-a", "SelectAll"),
            ("ctrl-c", "Copy"),
            ("ctrl-v", "Paste"),
            ("ctrl-left", "WordLeft"),
            ("ctrl-backspace", "DeleteWordLeft"),
        ] {
            let stroke = gpui::Keystroke::parse(key).unwrap();
            let binding = bindings
                .iter()
                .find(|b| b.match_keystrokes(&[stroke.clone()]) == Some(false))
                .unwrap();
            assert!(binding.action().name().ends_with(action));
        }
    }
}
