//! Reactions: what a chat takes, and giving one. Telegram has its own short list of reaction
//! emoji (`messages.getAvailableReactions`, asked once a connection), each chat's admins pick
//! from it (`available_reactions`: all, some, none), Premium adds custom emoji and more
//! reactions a person, and a reaction is sent as the whole set of ours on the message.

use super::peers::Who;
use super::wire::Fault;
use super::Account;
use grammers_tl_types as tl;
use protocol::{EmojiScope, ReactionRules, ReactionSet};

/// Different reactions a message holds where a chat sets no `reactions_limit`: the app
/// config's `reactions_uniq_max`, read from Telegram's documentation rather than asked.
pub const DISTINCT: u32 = 11;
/// Reactions a person may give one message (`reactions_user_max_default` and
/// `reactions_user_max_premium`), likewise.
pub const MINE: u32 = 1;
pub const MINE_PREMIUM: u32 = 3;

/// What a chat's admins allow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Allowed {
    /// Every reaction of Telegram's list, and custom emoji when `custom`.
    All { custom: bool },
    /// Only these: emoji, or custom emoji as `e<document>`.
    Some(Vec<String>),
    None,
}

/// The rules, from Telegram's list (each emoji and whether it needs Premium), the chat's
/// setting, whether we have Premium and the chat's own cap on different reactions.
pub fn rules_of(standard: &[(String, bool)], allowed: Allowed, premium: bool, limit: Option<i32>) -> ReactionRules {
    let usable = |(_, needs): &&(String, bool)| premium || !needs;
    let mut rules = ReactionRules {
        set: ReactionSet::Only,
        // A reaction the chat no longer allows cannot be given again, even where others gave it.
        join: false,
        mine: if premium { MINE_PREMIUM } else { MINE },
        // Telegram takes the set of ours whole, so a new one past the limit drops the oldest.
        replaces: true,
        distinct: limit.filter(|limit| *limit > 0).map_or(DISTINCT, |limit| limit as u32),
        ..ReactionRules::default()
    };
    match allowed {
        Allowed::None => return ReactionRules::off("Reactions are turned off in this chat."),
        Allowed::All { custom } => {
            rules.only = standard.iter().filter(usable).map(|(emoji, _)| emoji.clone()).collect();
            rules.custom = if custom && premium { EmojiScope::All } else { EmojiScope::None };
        }
        // Custom emoji in the chat's own list are Premium's too.
        Allowed::Some(list) => rules.only = list.into_iter().filter(|emoji| premium || !is_custom(emoji)).collect(),
    }
    rules
}

fn is_custom(emoji: &str) -> bool {
    emoji.strip_prefix('e').is_some_and(|id| id.parse::<i64>().is_ok())
}

fn spelled(reaction: &tl::enums::Reaction) -> Option<String> {
    match reaction {
        tl::enums::Reaction::Emoji(emoji) => Some(emoji.emoticon.clone()),
        tl::enums::Reaction::CustomEmoji(custom) => Some(format!("e{}", custom.document_id)),
        // Paid (⭐) reactions cost Stars and are never offered.
        tl::enums::Reaction::Paid | tl::enums::Reaction::Empty => None,
    }
}

fn allowed_of(set: Option<&tl::enums::ChatReactions>) -> Allowed {
    match set {
        Some(tl::enums::ChatReactions::None) => Allowed::None,
        Some(tl::enums::ChatReactions::Some(some)) => Allowed::Some(some.reactions.iter().filter_map(spelled).collect()),
        Some(tl::enums::ChatReactions::All(all)) => Allowed::All { custom: all.allow_custom },
        // Unverified: a chat that says nothing is taken to allow the whole list, as a new
        // group does, rather than none. Telegram refusing one is still shown and taken back.
        None => Allowed::All { custom: false },
    }
}

/// Telegram's list, asked once a connection.
fn standard(account: &Account) -> Result<Vec<(String, bool)>, Fault> {
    if let Some(known) = account.live().reactions.clone() {
        return Ok(known);
    }
    let list = match account.ask(&tl::functions::messages::GetAvailableReactions { hash: 0 })? {
        tl::enums::messages::AvailableReactions::Reactions(found) => found
            .reactions
            .into_iter()
            .map(|tl::enums::AvailableReaction::Reaction(one)| one)
            .filter(|one| !one.inactive)
            .map(|one| (one.reaction, one.premium))
            .collect(),
        tl::enums::messages::AvailableReactions::NotModified => Vec::new(),
    };
    account.live().reactions = Some(list.clone());
    Ok(list)
}

/// What a chat takes from us: one request for the chat's setting (none for a person), and the
/// list once a connection.
pub(super) fn rules(account: &Account, conversation: &str) -> Result<ReactionRules, Fault> {
    let invalid = || Fault::Rpc { code: 400, name: "PEER_ID_INVALID".to_owned(), value: None };
    let (who, _) = Who::parse(conversation).ok_or_else(invalid)?;
    let premium = account.dir().get(Who::User(account.me)).is_some_and(|known| known.premium);
    let (allowed, limit) = match who {
        // A chat with a person takes every reaction, and custom ones with Premium.
        Who::User(_) => (Allowed::All { custom: true }, None),
        Who::Group(chat_id) => {
            let tl::enums::messages::ChatFull::Full(full) = account.ask(&tl::functions::messages::GetFullChat { chat_id })?;
            account.learn(&full.users, &full.chats);
            match full.full_chat {
                tl::enums::ChatFull::Full(chat) => (allowed_of(chat.available_reactions.as_ref()), chat.reactions_limit),
                tl::enums::ChatFull::ChannelFull(channel) => (allowed_of(channel.available_reactions.as_ref()), channel.reactions_limit),
            }
        }
        Who::Channel(_) => {
            let channel = account.input_channel(who)?;
            let tl::enums::messages::ChatFull::Full(full) = account.ask(&tl::functions::channels::GetFullChannel { channel })?;
            account.learn(&full.users, &full.chats);
            match full.full_chat {
                tl::enums::ChatFull::ChannelFull(channel) => (allowed_of(channel.available_reactions.as_ref()), channel.reactions_limit),
                tl::enums::ChatFull::Full(chat) => (allowed_of(chat.available_reactions.as_ref()), chat.reactions_limit),
            }
        }
    };
    Ok(rules_of(&standard(account)?, allowed, premium, limit))
}

/// Gives or takes back a reaction. Telegram takes the whole set of ours on the message, so
/// it is worked out from the stored message, which already shows the change (chatd made it,
/// dropping any of ours a new one replaces): ours as stored, with this one added or taken out,
/// the newest kept where there are more than we may give.
pub(super) fn react(account: &Account, conversation: &str, message: &str, emoji: &str, on: bool) -> Result<(), Fault> {
    let invalid = || Fault::Rpc { code: 400, name: "MESSAGE_ID_INVALID".to_owned(), value: None };
    let ((who, _), msg_id) = (Who::parse(conversation).ok_or_else(invalid)?, message.parse::<i32>().map_err(|_| invalid())?);
    let stored = account.ctx.stored_message(conversation, message).ok().flatten();
    let reactions = stored.map(|stored| stored.reactions).unwrap_or_default();
    // A custom emoji on the message is named `:emoji:` there; its file says which one it is.
    let named = |have: &protocol::Reaction| have.file.as_ref().map_or_else(|| have.emoji.clone(), |file| file.id.clone());
    let emoji = match is_custom(emoji) {
        true => emoji.to_owned(),
        false => reactions.iter().find(|have| have.emoji == emoji).map_or_else(|| emoji.to_owned(), named),
    };
    let mut ours: Vec<String> = reactions.iter().filter(|have| have.mine).map(named).filter(|have| *have != emoji).collect();
    if on {
        ours.push(emoji);
    }
    let premium = account.dir().get(Who::User(account.me)).is_some_and(|known| known.premium);
    let most = if premium { MINE_PREMIUM } else { MINE } as usize;
    if ours.len() > most {
        ours.drain(..ours.len() - most);
    }
    let reaction = ours
        .into_iter()
        .map(|emoji| match emoji.strip_prefix('e').and_then(|id| id.parse::<i64>().ok()) {
            Some(document_id) => tl::types::ReactionCustomEmoji { document_id }.into(),
            None => tl::types::ReactionEmoji { emoticon: emoji }.into(),
        })
        .collect();
    let request = tl::functions::messages::SendReaction {
        big: false,
        add_to_recent: on,
        peer: account.input(who)?,
        msg_id,
        reaction: Some(reaction),
    };
    let answer = account.ask(&request)?;
    super::live::answered(account, &answer);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list() -> Vec<(String, bool)> {
        vec![("👍".into(), false), ("❤".into(), false), ("🔥".into(), false), ("🦄".into(), true)]
    }

    #[test]
    fn a_chat_offers_telegrams_list_or_its_own_part_of_it() {
        let all = rules_of(&list(), Allowed::All { custom: true }, false, None);
        assert_eq!((all.set, all.only.len(), all.custom, all.mine, all.replaces, all.distinct, all.join), (ReactionSet::Only, 3, EmojiScope::None, 1, true, 11, false));
        assert!(all.offers("❤️", false) && !all.offers("🦄", false), "Premium's are left out");
        let premium = rules_of(&list(), Allowed::All { custom: true }, true, Some(4));
        assert_eq!((premium.only.len(), premium.custom, premium.mine, premium.distinct), (4, EmojiScope::All, 3, 4));
        let some = rules_of(&list(), Allowed::Some(vec!["🔥".into(), "e55".into()]), false, None);
        assert_eq!(some.only, ["🔥"]);
        assert_eq!(rules_of(&list(), Allowed::Some(vec!["🔥".into(), "e55".into()]), true, None).only, ["🔥", "e55"]);
        assert_eq!(rules_of(&list(), Allowed::None, true, None).set, ReactionSet::Off);
    }
}
