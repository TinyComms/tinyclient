//! Discord voice extras that do not need a window: an Activity line, where a stage seat sits,
//! the soundboard list, and the phone call screen's chat button. The views draw what these return.

use super::stage::Seat;
use protocol::{ConversationKind, SoundboardSound, StageVoiceAction};

/// "Playing <name>" under a voice row and in the call header. An unnamed activity still says so.
pub(in crate::gui) fn activity_line(name: &str) -> String {
    let name = name.trim();
    if name.is_empty() {
        "Playing an activity".to_owned()
    } else {
        format!("Playing {name}")
    }
}

/// Where someone sits on a Discord stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::gui) enum StageSeat {
    Speaker,
    Raised,
    Audience,
}

/// A raised hand is still in the audience until a moderator (or an accepted invite) clears suppress.
pub(in crate::gui) fn stage_place(suppressed: bool, raised: bool) -> StageSeat {
    if !suppressed {
        StageSeat::Speaker
    } else if raised {
        StageSeat::Raised
    } else {
        StageSeat::Audience
    }
}

/// Speakers, then raised hands, then the audience. `self_raised` is the local user's hand.
pub(in crate::gui) fn order_seats(seats: &mut [Seat], raised: &[String], self_raised: bool) {
    seats.sort_by_key(|seat| {
        let hand = if seat.me { self_raised } else { raised.iter().any(|id| id == &seat.id) };
        match stage_place(seat.suppressed, hand) {
            StageSeat::Speaker => 0,
            StageSeat::Raised => 1,
            StageSeat::Audience => 2,
        }
    });
}

/// The primary stage control for the person in the call.
pub(in crate::gui) fn stage_button(stage: bool, request_to_speak: bool, suppressed: bool) -> Option<StageVoiceAction> {
    if !stage {
        return None;
    }
    Some(if !suppressed {
        StageVoiceAction::Audience
    } else if request_to_speak {
        StageVoiceAction::Accept
    } else {
        StageVoiceAction::RequestToSpeak
    })
}

/// A raised hand can be lowered without taking the speaker seat.
pub(in crate::gui) fn stage_lower_hand(stage: bool, request_to_speak: bool, suppressed: bool) -> bool {
    stage && request_to_speak && suppressed
}

pub(in crate::gui) fn stage_button_label(action: StageVoiceAction) -> &'static str {
    match action {
        StageVoiceAction::RequestToSpeak => "Request to speak",
        StageVoiceAction::Accept | StageVoiceAction::Invite => "Accept invite",
        StageVoiceAction::Audience | StageVoiceAction::ToAudience => "Move to audience",
    }
}

/// A moderator with mute rights invites a listener up, or sends a speaker back.
pub(in crate::gui) fn moderator_stage_action(can_mute: bool, suppressed: bool, raised: bool) -> Option<&'static str> {
    if !can_mute {
        return None;
    }
    Some(if suppressed || raised { "Invite to speak" } else { "Move to audience" })
}

pub(in crate::gui) fn stage_roster_line(speakers: usize, raised: usize, audience: usize) -> String {
    format!("{speakers} speaking · {raised} raised · {audience} listening")
}

/// A soundboard row matches an empty query, or a case-insensitive name or emoji.
pub(in crate::gui) fn soundboard_matches(sound: &SoundboardSound, query: &str) -> bool {
    let query = query.trim().to_ascii_lowercase();
    if query.is_empty() {
        return true;
    }
    sound.name.to_ascii_lowercase().contains(&query) || sound.emoji.to_ascii_lowercase().contains(&query)
}

pub(in crate::gui) fn favorite_ids(stored: &str) -> Vec<String> {
    stored.split([',', '\n']).map(str::trim).filter(|id| !id.is_empty()).map(str::to_owned).collect()
}

/// Adds or removes `id`. A list that would pass 500 characters is left as it was.
pub(in crate::gui) fn toggle_favorite(stored: &str, id: &str) -> String {
    let mut ids = favorite_ids(stored);
    if let Some(at) = ids.iter().position(|known| known == id) {
        ids.remove(at);
    } else {
        ids.push(id.to_owned());
    }
    let next = ids.join(",");
    if next.chars().count() > 500 { stored.to_owned() } else { next }
}

/// Favorites first, then by name. Sounds the query misses are left out.
pub(in crate::gui) fn order_sounds<'a>(sounds: &'a [SoundboardSound], favorites: &[String], query: &str) -> Vec<&'a SoundboardSound> {
    let mut kept: Vec<_> = sounds.iter().filter(|sound| soundboard_matches(sound, query)).collect();
    kept.sort_by_key(|sound| (!favorites.iter().any(|id| id == &sound.id), sound.name.to_ascii_lowercase()));
    kept
}

/// The flash stays up for four seconds from `at`.
pub(in crate::gui) fn effect_visible(at: i64, now: i64) -> bool {
    (0..4).contains(&now.saturating_sub(at))
}

pub(in crate::gui) fn effect_label(emoji: &str) -> String {
    if emoji.is_empty() { "Sound".to_owned() } else { emoji.to_owned() }
}

/// `Some(0)` is a dot. A mention count wins over a plain unread count.
pub(in crate::gui) fn phone_chat_badge(unread: u32, mentions: u32) -> Option<u32> {
    if mentions > 0 {
        Some(mentions)
    } else if unread > 0 {
        Some(0)
    } else {
        None
    }
}

/// Voice channels, direct calls, and group calls can open their chat over the call screen.
pub(in crate::gui) fn phone_chat_offered(kind: ConversationKind) -> bool {
    matches!(kind, ConversationKind::Voice | ConversationKind::Direct | ConversationKind::Group)
}

/// Hide the round controls only while the chat sheet is open and the keyboard is up,
/// so the composer sits on the keyboard and the call header stays.
pub(in crate::gui) fn phone_chat_hides_controls(chat: bool, keyboard: f32) -> bool {
    chat && keyboard > 0.
}

/// "Open chat" is the phone long-press of a voice row, and it does not join the channel.
pub(in crate::gui) fn open_chat_item(phone: bool, kind: ConversationKind) -> bool {
    phone && kind == ConversationKind::Voice
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seat(id: &str, suppressed: bool, me: bool) -> Seat {
        Seat { id: id.to_owned(), suppressed, me, name: id.to_owned(), ..Seat::default() }
    }

    fn sound(id: &str, name: &str, emoji: &str) -> SoundboardSound {
        SoundboardSound { id: id.to_owned(), name: name.to_owned(), emoji: emoji.to_owned(), ..SoundboardSound::default() }
    }

    #[test]
    fn an_activity_line_names_the_game_and_an_empty_one_still_says_playing() {
        assert_eq!(activity_line("Chess"), "Playing Chess");
        assert_eq!(activity_line("  "), "Playing an activity");
    }

    #[test]
    fn a_stage_groups_speakers_then_raised_hands_then_the_audience() {
        assert_eq!(stage_place(false, false), StageSeat::Speaker);
        assert_eq!(stage_place(true, true), StageSeat::Raised);
        assert_eq!(stage_place(true, false), StageSeat::Audience);
        let mut seats = vec![seat("listen", true, false), seat("me", true, true), seat("talk", false, false)];
        order_seats(&mut seats, &["2001".to_owned()], true);
        assert_eq!(seats.iter().map(|seat| seat.id.as_str()).collect::<Vec<_>>(), ["talk", "me", "listen"]);
        assert_eq!(stage_roster_line(1, 1, 1), "1 speaking · 1 raised · 1 listening");
    }

    #[test]
    fn stage_controls_request_accept_and_step_back() {
        assert_eq!(stage_button(false, false, true), None);
        assert_eq!(stage_button(true, false, true), Some(StageVoiceAction::RequestToSpeak));
        assert_eq!(stage_button_label(StageVoiceAction::RequestToSpeak), "Request to speak");
        assert_eq!(stage_button(true, true, true), Some(StageVoiceAction::Accept));
        assert_eq!(stage_button_label(StageVoiceAction::Accept), "Accept invite");
        assert!(stage_lower_hand(true, true, true));
        assert!(!stage_lower_hand(true, false, true));
        assert_eq!(stage_button(true, false, false), Some(StageVoiceAction::Audience));
        assert_eq!(stage_button_label(StageVoiceAction::Audience), "Move to audience");
        assert_eq!(moderator_stage_action(false, true, false), None);
        assert_eq!(moderator_stage_action(true, true, false), Some("Invite to speak"));
        assert_eq!(moderator_stage_action(true, false, true), Some("Invite to speak"));
        assert_eq!(moderator_stage_action(true, false, false), Some("Move to audience"));
    }

    #[test]
    fn the_soundboard_search_keeps_favorites_first_and_a_favorite_toggles() {
        let sounds = vec![sound("b", "Boom", ""), sound("q", "Quack", "🦆"), sound("h", "Horn", "")];
        let all = order_sounds(&sounds, &["q".to_owned()], "");
        assert_eq!(all.iter().map(|sound| sound.id.as_str()).collect::<Vec<_>>(), ["q", "b", "h"]);
        // "o" matches Boom and Horn. Quack is a favorite and still stays out.
        let shown = order_sounds(&sounds, &["q".to_owned()], "o");
        assert_eq!(shown.iter().map(|sound| sound.id.as_str()).collect::<Vec<_>>(), ["b", "h"]);
        let favorite_hit = order_sounds(&sounds, &["h".to_owned()], "o");
        assert_eq!(favorite_hit.iter().map(|sound| sound.id.as_str()).collect::<Vec<_>>(), ["h", "b"]);
        assert!(soundboard_matches(&sounds[1], "🦆"));
        assert!(!soundboard_matches(&sounds[0], "quack"));
        assert_eq!(toggle_favorite("q", "b"), "q,b");
        assert_eq!(toggle_favorite("q,b", "q"), "b");
        assert_eq!(favorite_ids("q\nb"), ["q", "b"]);
        let long = "x".repeat(500);
        assert_eq!(toggle_favorite(&long, "y"), long);
    }

    #[test]
    fn a_sound_flash_lasts_four_seconds_and_a_missing_emoji_says_sound() {
        assert!(effect_visible(10, 10));
        assert!(effect_visible(10, 13));
        assert!(!effect_visible(10, 14));
        assert!(!effect_visible(10, 9));
        assert_eq!(effect_label("📯"), "📯");
        assert_eq!(effect_label(""), "Sound");
    }

    #[test]
    fn the_phone_chat_button_badges_mentions_and_opens_only_offered_chats() {
        assert_eq!(phone_chat_badge(0, 0), None);
        assert_eq!(phone_chat_badge(3, 0), Some(0));
        assert_eq!(phone_chat_badge(3, 2), Some(2));
        assert!(phone_chat_offered(ConversationKind::Voice));
        assert!(phone_chat_offered(ConversationKind::Direct));
        assert!(phone_chat_offered(ConversationKind::Group));
        assert!(!phone_chat_offered(ConversationKind::Channel));
        assert!(phone_chat_hides_controls(true, 320.));
        assert!(!phone_chat_hides_controls(true, 0.));
        assert!(!phone_chat_hides_controls(false, 320.));
        assert!(open_chat_item(true, ConversationKind::Voice));
        assert!(!open_chat_item(false, ConversationKind::Voice));
        assert!(!open_chat_item(true, ConversationKind::Channel));
    }
}
