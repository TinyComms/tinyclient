//! Which controls the card lays out, from what the account may send. A control the network
//! lacks is absent, never disabled: neighbors close up.

use super::picker::Tab;
use protocol::{Capabilities, EmojiScope};

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
    /// The mic: the network names a recorded file it is sent a voice note with.
    pub voice: bool,
    /// Right-click on Send can hand the network a time (`Outgoing::schedule`).
    pub schedule: bool,
    /// The "Create poll" button beside Attach (`Capabilities::polls`).
    pub poll: bool,
    /// Mark message as Important or Urgent (`Capabilities::importance`, Teams).
    pub importance: bool,
    /// "Also send to #channel" on a thread reply (`Capabilities::thread_broadcast`, Slack).
    pub broadcast: bool,
    /// Telegram message effects selector (`Capabilities::effects`).
    pub effects: bool,
    /// View-once per-file toggle (`Capabilities::view_once`, Signal/Telegram).
    pub view_once: bool,
}

/// `editing`: edit mode is text only on every network.
pub fn controls(can: &Capabilities, editing: bool) -> Controls {
    let catalog = can.gifs;
    let gif_file = can.gif_file && can.max_file_bytes > 0;
    let mut tabs = vec![Tab::Emoji];
    if catalog && !editing {
        tabs.push(Tab::Gifs);
    }
    if can.stickers && !editing {
        tabs.push(Tab::Stickers);
    }
    Controls {
        attach: can.max_file_bytes > 0 && !editing,
        gif: (catalog || gif_file) && !editing,
        emoji: true,
        tabs,
        silent: can.silent && !editing,
        ping: can.reply_ping,
        link_chip: can.suppress_embeds && !editing,
        custom_emoji: can.custom_emoji != EmojiScope::None,
        voice: super::rec::available(can) && !editing,
        schedule: can.schedule && !editing,
        poll: can.polls.is_some() && !editing,
        importance: can.importance && !editing,
        broadcast: can.thread_broadcast && !editing,
        effects: can.effects && !editing,
        view_once: can.view_once && !editing,
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
            voice_notes: true,
            ..Capabilities::default()
        }
    }

    #[test]
    fn each_network_lays_out_only_what_it_can_do() {
        let d = controls(&discord(), false);
        assert!(d.attach && d.gif && d.emoji && d.silent && d.ping && d.link_chip && d.custom_emoji);
        assert!(d.voice, "the account names its files voice notes");
        assert_eq!(d.tabs, [Tab::Emoji, Tab::Gifs, Tab::Stickers]);

        let slack = Capabilities { max_file_bytes: 1 << 30, max_files: 10, custom_emoji: EmojiScope::All, suppress_embeds: true, ..Capabilities::default() };
        let s = controls(&slack, false);
        assert!(s.attach && !s.gif && !s.silent && !s.ping && s.link_chip && s.custom_emoji);
        assert_eq!(s.tabs, [Tab::Emoji]);

        let telegram = Capabilities { max_file_bytes: 2 << 30, gifs: true, stickers: true, silent: true, suppress_embeds: true, ..Capabilities::default() };
        let t = controls(&telegram, false);
        assert!(t.gif && t.silent && !t.ping && !t.custom_emoji);
        assert_eq!(t.tabs, [Tab::Emoji, Tab::Gifs, Tab::Stickers]);

        let teams = Capabilities { max_file_bytes: 250 << 20, max_files: 10, ..Capabilities::default() };
        let m = controls(&teams, false);
        assert!(m.attach && !m.gif && !m.silent && !m.link_chip);
        assert_eq!(m.tabs, [Tab::Emoji]);

        let tiny = Capabilities { max_file_bytes: 8 << 20, max_files: 10, gif_file: true, ..Capabilities::default() };
        let y = controls(&tiny, false);
        assert!(y.attach && y.gif && y.emoji && !y.custom_emoji);
        assert_eq!(y.tabs, [Tab::Emoji]);
        let tiny_catalog = Capabilities { max_file_bytes: 8 << 20, max_files: 10, gifs: true, gif_file: true, ..Capabilities::default() };
        let yc = controls(&tiny_catalog, false);
        assert!(yc.gif);
        assert_eq!(yc.tabs, [Tab::Emoji, Tab::Gifs]);
        let bare = controls(&Capabilities { gif_file: true, ..Capabilities::default() }, false);
        assert!(!bare.attach && !bare.gif && bare.emoji);
        assert_eq!(bare.tabs, [Tab::Emoji]);
    }

    #[test]
    fn nothing_is_allowed_until_the_session_says_so_and_edit_is_text_only() {
        let none = controls(&Capabilities::default(), false);
        assert!(!none.attach && !none.gif && none.emoji && !none.silent && !none.link_chip);
        let edit = controls(&discord(), true);
        assert!(!edit.attach && !edit.gif && edit.emoji && !edit.silent && !edit.link_chip);
        assert!(!edit.voice, "edit mode is text only");
        assert_eq!(edit.tabs, [Tab::Emoji]);
    }

    #[test]
    fn schedule_shows_only_when_the_network_schedules_and_an_edit_hides_it() {
        let slack = Capabilities { schedule: true, silent: true, ..Capabilities::default() };
        let open = controls(&slack, false);
        assert!(open.schedule && open.silent);
        let edit = controls(&slack, true);
        assert!(!edit.schedule && !edit.silent, "edit mode is text only");
        assert!(!controls(&discord(), false).schedule);
    }

    #[test]
    fn poll_button_follows_capability_and_hides_when_editing() {
        let with_polls = Capabilities {
            polls: Some(protocol::PollCapabilities { max_options: 10, max_duration_secs: 86_400, multiple: true, anonymous: false }),
            ..Capabilities::default()
        };
        assert!(controls(&with_polls, false).poll);
        assert!(!controls(&with_polls, true).poll);
        assert!(!controls(&Capabilities::default(), false).poll);
    }

    #[test]
    fn send_option_controls_follow_capabilities_and_hide_when_editing() {
        let teams = Capabilities { importance: true, ..Capabilities::default() };
        let m = controls(&teams, false);
        assert!(m.importance && !m.broadcast && !m.effects && !m.view_once);
        assert!(!controls(&teams, true).importance);

        let slack = Capabilities { thread_broadcast: true, ..Capabilities::default() };
        let s = controls(&slack, false);
        assert!(s.broadcast && !s.importance && !s.effects && !s.view_once);

        let telegram = Capabilities { effects: true, view_once: true, ..Capabilities::default() };
        let t = controls(&telegram, false);
        assert!(t.effects && t.view_once && !t.importance && !t.broadcast);
        assert!(!controls(&telegram, true).effects);
    }
}
