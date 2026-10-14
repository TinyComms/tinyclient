//! What reactions a conversation takes, as its network decides them for this account: which
//! emoji, how many of mine one message may carry, and how many different ones a message holds.
//! The shell offers only what passes [`ReactionRules::check`], and chatd runs the same check
//! before the network hears of a reaction, so a reaction the network would refuse is never sent.
//!
//! Every field is optional on the wire. A chatd from before these rules answers `Reactions`
//! with an error, and the shell then offers what it always did.

use crate::{is_false, is_zero, EmojiScope, Reaction};
use serde::{Deserialize, Serialize};

/// Most emoji [`ReactionRules::only`] lists. Telegram's whole set is under a hundred.
pub const REACTIONS_LISTED: usize = 500;

fn is_true(value: &bool) -> bool {
    *value
}

fn yes() -> bool {
    true
}

/// Which emoji may be chosen as a new reaction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReactionSet {
    /// Every Unicode emoji (Discord, Slack, Signal).
    #[default]
    Any,
    /// Only those in [`ReactionRules::only`] (a Telegram chat's set, Teams' catalog).
    Only,
    /// None: reactions are off here, and [`ReactionRules::why`] says so.
    Off,
}

/// What one conversation takes as reactions from this account. The default is what a network
/// that never said anything is taken to allow: any emoji, no limits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReactionRules {
    #[serde(default, skip_serializing_if = "is_any")]
    pub set: ReactionSet,
    /// With `Only`: the emoji allowed, in the network's order, spelled as `React` sends them. A
    /// custom one is named by its registered file's id.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub only: Vec<String>,
    /// A listed emoji may be given in a skin tone too (Slack, Teams), not only as listed.
    #[serde(default, skip_serializing_if = "is_false")]
    pub tones: bool,
    /// Which of the account's custom emoji (`Catalog` with `Emoji`) may be chosen here.
    #[serde(default, skip_serializing_if = "EmojiScope::is_none")]
    pub custom: EmojiScope,
    /// Animated custom emoji may be chosen too (Discord: Nitro only).
    #[serde(default, skip_serializing_if = "is_false")]
    pub animated: bool,
    /// A reaction already on a message may be added to even when it could not be chosen here:
    /// Discord lets anyone join another server's emoji. False on Telegram, where a reaction
    /// the chat no longer allows cannot be given again.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub join: bool,
    /// Only reactions already on a message may be added to (Discord without Add Reactions).
    #[serde(default, skip_serializing_if = "is_false")]
    pub join_only: bool,
    /// Most of my reactions on one message. 0: no limit.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub mine: u32,
    /// At `mine`, a new one takes the place of my oldest (Telegram, Signal) instead of being
    /// refused.
    #[serde(default, skip_serializing_if = "is_false")]
    pub replaces: bool,
    /// Most different emoji one message holds. 0: no limit.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub distinct: u32,
    /// Why reactions are off, in words, when `set` is `Off`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub why: String,
}

fn is_any(set: &ReactionSet) -> bool {
    *set == ReactionSet::Any
}

impl Default for ReactionRules {
    fn default() -> ReactionRules {
        ReactionRules {
            set: ReactionSet::Any,
            only: Vec::new(),
            tones: false,
            custom: EmojiScope::None,
            animated: false,
            join: true,
            join_only: false,
            mine: 0,
            replaces: false,
            distinct: 0,
            why: String::new(),
        }
    }
}

/// An emoji without the variation selector, which some networks write and others leave off
/// (Telegram's ❤ is the picker's ❤️).
pub fn bare_emoji(emoji: &str) -> String {
    emoji.chars().filter(|ch| *ch != '\u{FE0F}').collect()
}

/// An emoji without its variation selector or skin tone.
fn untoned(emoji: &str) -> String {
    emoji.chars().filter(|ch| *ch != '\u{FE0F}' && !('\u{1F3FB}'..='\u{1F3FF}').contains(ch)).collect()
}

/// The reaction on a message that is this one: by what it is called, or by its file's id.
fn same(have: &Reaction, emoji: &str) -> bool {
    have.emoji == emoji || have.file.as_ref().is_some_and(|file| file.id == emoji) || (!emoji.starts_with(':') && bare_emoji(&have.emoji) == bare_emoji(emoji))
}

impl ReactionRules {
    /// Reactions are off here.
    pub fn off(why: impl Into<String>) -> ReactionRules {
        ReactionRules { set: ReactionSet::Off, why: why.into(), ..ReactionRules::default() }
    }

    /// Cuts what a network sent to the contract's limits.
    pub fn bound(&mut self) {
        self.only.truncate(REACTIONS_LISTED);
        self.only.iter_mut().for_each(|emoji| crate::clip(emoji, 64));
        crate::clip(&mut self.why, 200);
    }

    /// The emoji, or the custom emoji with this name or file id, may be chosen as a new
    /// reaction. Whether a custom one is animated is the caller's to check against `animated`.
    pub fn offers(&self, emoji: &str, custom: bool) -> bool {
        if self.set == ReactionSet::Off || self.join_only {
            return false;
        }
        let (bare, plain) = (bare_emoji(emoji), untoned(emoji));
        if self.only.iter().any(|allowed| bare_emoji(allowed) == bare || (self.tones && untoned(allowed) == plain)) {
            return true;
        }
        match custom {
            true => self.custom != EmojiScope::None,
            false => self.set == ReactionSet::Any,
        }
    }

    /// The network's own spelling of an emoji it lists (`❤` for `❤️`), to send as it is.
    pub fn spelled<'a>(&'a self, emoji: &'a str) -> &'a str {
        let bare = bare_emoji(emoji);
        self.only.iter().find(|allowed| bare_emoji(allowed) == bare).map_or(emoji, String::as_str)
    }

    /// Anything at all may be added to a message with these reactions: what decides whether
    /// the add affordance is drawn. A message at the cap of different emoji can still take one
    /// already on it, but not from the picker.
    pub fn adds(&self, reactions: &[Reaction]) -> bool {
        if self.set == ReactionSet::Off || self.join_only {
            return false;
        }
        let mine = reactions.iter().filter(|have| have.mine).count();
        let full = self.distinct > 0 && reactions.len() >= self.distinct as usize;
        // A message at the cap still takes a new one where mine is replaced and was alone.
        let freed = self.replaces && self.mine > 0 && mine >= self.mine as usize && reactions.iter().any(|have| have.mine && have.count == 1);
        !full || freed
    }

    /// Whether adding `emoji` to a message with these reactions would go through. `Ok` names my
    /// reactions it replaces (oldest first); `Err` says why not, in words for a toast. Taking
    /// one back is never checked.
    pub fn check(&self, reactions: &[Reaction], emoji: &str, custom: bool) -> Result<Vec<String>, String> {
        if self.set == ReactionSet::Off {
            return Err(if self.why.is_empty() { "Reactions are turned off here.".to_owned() } else { self.why.clone() });
        }
        let on_it = reactions.iter().find(|have| same(have, emoji));
        match on_it {
            Some(have) if have.mine => return Ok(Vec::new()),
            Some(_) if self.join || self.offers(emoji, custom) => {}
            Some(_) => return Err("That reaction isn't allowed here any more.".to_owned()),
            None if self.join_only => return Err("You can only add to reactions already on this message.".to_owned()),
            None if !self.offers(emoji, custom) => return Err("That reaction isn't allowed here.".to_owned()),
            None => {}
        }
        let mine: Vec<&Reaction> = reactions.iter().filter(|have| have.mine).collect();
        let mut replacing = Vec::new();
        if self.mine > 0 && mine.len() >= self.mine as usize {
            if !self.replaces {
                return Err(match self.mine {
                    1 => "You can add one reaction to a message here.".to_owned(),
                    most => format!("You can add {most} reactions to a message here."),
                });
            }
            let drop = mine.len() + 1 - self.mine as usize;
            replacing = mine.iter().take(drop).map(|have| have.emoji.clone()).collect();
        }
        if on_it.is_none() && self.distinct > 0 {
            let freed = mine.iter().filter(|have| have.count == 1 && replacing.contains(&have.emoji)).count();
            if reactions.len() - freed >= self.distinct as usize {
                return Err(format!("This message has {} different reactions, as many as allowed.", self.distinct));
            }
        }
        Ok(replacing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on(emoji: &str, count: u32, mine: bool) -> Reaction {
        Reaction { emoji: emoji.into(), file: None, count, mine }
    }

    #[test]
    fn rules_from_before_they_existed_allow_everything() {
        let rules: ReactionRules = serde_json::from_str("{}").unwrap();
        assert_eq!(rules, ReactionRules::default());
        assert_eq!(serde_json::to_string(&rules).unwrap(), "{}");
        assert!(rules.offers("🦑", false) && !rules.offers(":party:", true));
        assert_eq!(rules.check(&[on("👍", 4, false)], "🦑", false), Ok(Vec::new()));
        assert_eq!(rules.check(&[on(":party:", 1, false)], ":party:", true), Ok(Vec::new()), "a custom one on the message is joined");
    }

    #[test]
    fn a_chat_with_a_set_offers_only_it_and_spells_hearts_either_way() {
        let rules = ReactionRules { set: ReactionSet::Only, only: vec!["👍".into(), "❤".into()], join: false, ..ReactionRules::default() };
        assert!(rules.offers("❤️", false) && rules.offers("👍", false));
        assert_eq!((rules.spelled("❤️"), rules.spelled("🦑")), ("❤", "🦑"));
        assert!(!rules.offers("👍🏽", false), "a tone is another reaction on Telegram");
        let toned = ReactionRules { tones: true, ..rules.clone() };
        assert!(toned.offers("👍🏽", false));
        assert_eq!(toned.spelled("👍🏽"), "👍🏽", "the tone is kept");
        assert!(!rules.offers("🦑", false) && !rules.offers(":party:", true));
        assert!(rules.check(&[], "🦑", false).is_err());
        // A reaction given before the set changed cannot be given again.
        assert_eq!(rules.check(&[on("🦑", 2, false)], "🦑", false), Err("That reaction isn't allowed here any more.".into()));
    }

    #[test]
    fn one_reaction_a_person_replaces_and_a_cap_refuses() {
        let single = ReactionRules { mine: 1, replaces: true, ..ReactionRules::default() };
        assert_eq!(single.check(&[on("👍", 1, true), on("🎉", 2, false)], "🎉", false), Ok(vec!["👍".into()]));
        assert_eq!(single.check(&[on("👍", 1, true)], "👍", false), Ok(Vec::new()), "already mine");
        let slack = ReactionRules { mine: 2, ..ReactionRules::default() };
        assert_eq!(slack.check(&[on("👍", 1, true), on("🎉", 1, true)], "🔥", false), Err("You can add 2 reactions to a message here.".into()));
        let three = ReactionRules { mine: 3, replaces: true, ..ReactionRules::default() };
        let held = [on("a", 1, true), on("b", 1, true), on("c", 1, true)];
        assert_eq!(three.check(&held, "d", false), Ok(vec!["a".into()]), "the oldest goes");
    }

    #[test]
    fn a_full_message_takes_only_what_is_on_it() {
        let rules = ReactionRules { distinct: 2, ..ReactionRules::default() };
        let full = [on("👍", 3, false), on("🎉", 1, false)];
        assert!(!rules.adds(&full));
        assert_eq!(rules.check(&full, "🎉", false), Ok(Vec::new()));
        assert!(rules.check(&full, "🔥", false).unwrap_err().contains("2 different"));
        // Replacing my lone reaction frees its place.
        let swap = ReactionRules { distinct: 2, mine: 1, replaces: true, ..ReactionRules::default() };
        let mine = [on("👍", 3, false), on("🎉", 1, true)];
        assert!(swap.adds(&mine));
        assert_eq!(swap.check(&mine, "🔥", false), Ok(vec!["🎉".into()]));
    }

    #[test]
    fn off_and_join_only_say_why() {
        let off = ReactionRules::off("Reactions are turned off in this channel.");
        assert!(!off.adds(&[]) && !off.offers("👍", false));
        assert_eq!(off.check(&[on("👍", 1, false)], "👍", false), Err("Reactions are turned off in this channel.".into()));
        let join = ReactionRules { join_only: true, ..ReactionRules::default() };
        assert!(!join.adds(&[on("👍", 1, false)]));
        assert_eq!(join.check(&[on("👍", 1, false)], "👍", false), Ok(Vec::new()));
        assert!(join.check(&[], "👍", false).is_err());
        let json = serde_json::to_string(&off).unwrap();
        assert_eq!(serde_json::from_str::<ReactionRules>(&json).unwrap(), off);
    }
}
