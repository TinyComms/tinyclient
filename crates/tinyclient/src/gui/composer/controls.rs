//! Which controls the card lays out, from what the account may send. A control the network
//! lacks is absent, never disabled: neighbors close up.

use super::picker::Tab;
use protocol::{Capabilities, EmojiScope, Network};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Controls {
    pub attach: bool,
    pub gif: bool,
    /// Every network has Unicode emoji, so the smile is always there while typing is.
    pub emoji: bool,
    pub tabs: Vec<Tab>,
    /// Option-Return sends without a notification. Elsewhere it is a new line.
    pub silent: bool,
    /// The "@ On / @ Off" toggle in the reply bar.
    pub ping: bool,
    /// "Preview: example.com ×" under the card.
    pub link_chip: bool,
    pub custom_emoji: bool,
}

/// `editing`: edit mode is text only on every network.
pub fn controls(network: Network, can: &Capabilities, editing: bool) -> Controls {
    // Slack and Teams have GIF pickers whose client calls are not captured yet (ruling 09):
    // hidden there whatever a session reports.
    let gifs = can.gifs && matches!(network, Network::Discord | Network::Telegram);
    let mut tabs = vec![Tab::Emoji];
    if gifs && !editing {
        tabs.push(Tab::Gifs);
    }
    if can.stickers && !editing {
        tabs.push(Tab::Stickers);
    }
    Controls {
        attach: can.max_file_bytes > 0 && !editing,
        gif: gifs && !editing,
        emoji: true,
        tabs,
        silent: can.silent && !editing,
        ping: can.reply_ping,
        // Teams builds the card at the sender; there is nothing to suppress.
        link_chip: can.suppress_embeds && network != Network::Teams && !editing,
        custom_emoji: can.custom_emoji != EmojiScope::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn discord() -> Capabilities {
        Capabilities {
            max_file_bytes: 10 << 20,
            max_files: 10,
            spoiler: true,
            gifs: true,
            stickers: true,
            custom_emoji: EmojiScope::Space,
            silent: true,
            suppress_embeds: true,
            reply_ping: true,
            ..Capabilities::default()
        }
    }

    #[test]
    fn each_network_lays_out_only_what_it_can_do() {
        let d = controls(Network::Discord, &discord(), false);
        assert!(d.attach && d.gif && d.emoji && d.silent && d.ping && d.link_chip && d.custom_emoji);
        assert_eq!(d.tabs, [Tab::Emoji, Tab::Gifs, Tab::Stickers]);

        let slack = Capabilities { max_file_bytes: 1 << 30, max_files: 10, custom_emoji: EmojiScope::All, suppress_embeds: true, gifs: true, ..Capabilities::default() };
        let s = controls(Network::Slack, &slack, false);
        assert!(s.attach && !s.gif && !s.silent && !s.ping && s.link_chip && s.custom_emoji);
        assert_eq!(s.tabs, [Tab::Emoji], "Slack's GIFs wait for its client call");

        let telegram = Capabilities { max_file_bytes: 2 << 30, gifs: true, stickers: true, silent: true, suppress_embeds: true, ..Capabilities::default() };
        let t = controls(Network::Telegram, &telegram, false);
        assert!(t.gif && t.silent && !t.ping && !t.custom_emoji);
        assert_eq!(t.tabs, [Tab::Emoji, Tab::Gifs, Tab::Stickers]);

        let teams = Capabilities { max_file_bytes: 250 << 20, max_files: 10, suppress_embeds: true, ..Capabilities::default() };
        let m = controls(Network::Teams, &teams, false);
        assert!(m.attach && !m.gif && !m.silent && !m.link_chip);
        assert_eq!(m.tabs, [Tab::Emoji]);
    }

    #[test]
    fn nothing_is_allowed_until_the_session_says_so_and_edit_is_text_only() {
        let none = controls(Network::Discord, &Capabilities::default(), false);
        assert!(!none.attach && !none.gif && none.emoji && !none.silent && !none.link_chip);
        let edit = controls(Network::Discord, &discord(), true);
        assert!(!edit.attach && !edit.gif && edit.emoji && !edit.silent);
        assert_eq!(edit.tabs, [Tab::Emoji]);
    }
}
