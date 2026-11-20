//! A Telegram secret chat, named and left where it works. The row says the chat exists and that
//! it only works in Telegram's phone apps. This adapter never accepts one, never starts one,
//! never discards one, and never claims the messages are end-to-end encrypted here.

use super::peers::Who;
use super::wire::Fault;
use super::Account;
use crate::store::{ConversationRow, Filing};
use grammers_tl_types as tl;
use protocol::{ConversationKind, Message, MessageKind};

/// What the row and a send into it say. The messages themselves are not here.
pub const PHONE: &str = "Secret chats only work in Telegram's phone apps.";

/// `e<id>`. `Who::parse` rejects the prefix, so opening the row does not fetch a normal peer.
pub fn row_id(id: i32) -> String {
    format!("e{id}")
}

pub fn is_row(id: &str) -> bool {
    let Some(rest) = id.strip_prefix('e') else {
        return false;
    };
    !rest.is_empty() && rest.bytes().all(|byte| byte.is_ascii_digit())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// The other person asked. We do not accept.
    Started,
    /// We asked. We do not continue it here.
    Asked,
    /// Telegram says the chat is open on the phone. The messages stay there.
    Open,
    Gone,
}

struct Found {
    id: i32,
    other: i64,
    kind: Kind,
}

impl Found {
    fn words(&self, name: &str) -> String {
        match self.kind {
            Kind::Started => format!("{name} started a secret chat on Telegram. {PHONE}"),
            Kind::Asked => format!("You asked {name} to start a secret chat on Telegram. {PHONE}"),
            Kind::Open => format!("A secret chat with {name} is open on Telegram. {PHONE}"),
            Kind::Gone => String::new(),
        }
    }
}

fn other(me: i64, admin: i64, participant: i64) -> i64 {
    if admin != me {
        admin
    } else {
        participant
    }
}

fn classify(me: i64, chat: &tl::enums::EncryptedChat) -> Option<Found> {
    Some(match chat {
        tl::enums::EncryptedChat::Empty(_) => return None,
        tl::enums::EncryptedChat::Discarded(gone) => Found {
            id: gone.id,
            other: 0,
            kind: Kind::Gone,
        },
        tl::enums::EncryptedChat::Requested(chat) => Found {
            id: chat.id,
            other: other(me, chat.admin_id, chat.participant_id),
            kind: if chat.participant_id == me {
                Kind::Started
            } else {
                Kind::Asked
            },
        },
        tl::enums::EncryptedChat::Waiting(chat) => Found {
            id: chat.id,
            other: other(me, chat.admin_id, chat.participant_id),
            kind: if chat.admin_id == me {
                Kind::Asked
            } else {
                Kind::Started
            },
        },
        tl::enums::EncryptedChat::Chat(chat) => Found {
            id: chat.id,
            other: other(me, chat.admin_id, chat.participant_id),
            kind: Kind::Open,
        },
    })
}

fn person_name(account: &Account, id: i64) -> String {
    account
        .dir()
        .get(Who::User(id))
        .map(|known| known.name.clone())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Someone".to_owned())
}

/// One `updateEncryption`. A discarded chat removes the row. Anything else is a read-only row
/// and one system line. Nothing is sent back to Telegram.
pub fn apply(account: &Account, update: &tl::types::UpdateEncryption) {
    let Some(found) = classify(account.me, &update.chat) else {
        return;
    };
    let id = row_id(found.id);
    if found.kind == Kind::Gone {
        let _ = account.ctx.conversation_gone(&id);
        return;
    }
    let name = person_name(account, found.other);
    let mut row = ConversationRow::new(&id, ConversationKind::Direct, name.clone());
    row.read_only = true;
    row.listed = true;
    row.topic = PHONE.to_owned();
    row.filing = Some(Filing::default());
    let _ = account.ctx.conversation(&row);
    let mut message = Message::plain(
        format!("s{}", found.id),
        "",
        found.words(&name),
        i64::from(update.date),
        false,
    );
    message.kind = MessageKind::System;
    let _ = account.ctx.message(&id, &message);
}

/// The lab's telegram-secret-chat check. A request is a row id that is not a peer, and the
/// notice names the phone apps. Sending into that row says the same.
pub fn prove() -> Result<(), String> {
    let fault = Fault::Rpc {
        code: 400,
        name: "SECRET_CHAT".into(),
        value: None,
    };
    if fault.words() != PHONE {
        return Err(format!("a secret chat send is unnamed: {}", fault.words()));
    }
    if !is_row("e42") || is_row("u7002") || Who::parse("e42").is_some() {
        return Err("a secret chat must not be opened as a normal peer".into());
    }
    let requested = tl::types::EncryptedChatRequested {
        folder_id: None,
        id: 42,
        access_hash: 1,
        date: 1,
        admin_id: 7_002,
        participant_id: 1,
        g_a: vec![1],
    };
    let found = classify(1, &requested.into()).ok_or("a request is a notice")?;
    let line = found.words("Odalys Brandt");
    if found.kind != Kind::Started
        || !line.contains("Odalys Brandt")
        || !line.contains("phone apps")
    {
        return Err(line);
    }
    let waiting = tl::types::EncryptedChatWaiting {
        id: 42,
        access_hash: 1,
        date: 1,
        admin_id: 1,
        participant_id: 7_002,
    };
    let asked = classify(1, &waiting.into()).ok_or("a wait is a notice")?;
    if asked.kind != Kind::Asked || !asked.words("Odalys Brandt").contains("You asked") {
        return Err(asked.words("Odalys Brandt"));
    }
    let open = tl::types::EncryptedChat {
        id: 42,
        access_hash: 1,
        date: 1,
        admin_id: 7_002,
        participant_id: 1,
        g_a_or_b: vec![1],
        key_fingerprint: 9,
    };
    let open = classify(1, &open.into()).ok_or("an open chat is a notice")?;
    if open.kind != Kind::Open || !open.words("Odalys Brandt").contains("is open") {
        return Err(open.words("Odalys Brandt"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn requested(admin: i64, participant: i64) -> tl::enums::EncryptedChat {
        tl::types::EncryptedChatRequested {
            folder_id: None,
            id: 42,
            access_hash: 1,
            date: 1,
            admin_id: admin,
            participant_id: participant,
            g_a: vec![1],
        }
        .into()
    }

    #[test]
    fn a_request_a_wait_and_an_open_chat_each_name_the_phone_apps() {
        let started = classify(1, &requested(7, 1)).unwrap();
        assert_eq!(
            started.words("Odalys Brandt"),
            format!("Odalys Brandt started a secret chat on Telegram. {PHONE}")
        );
        let waiting = tl::types::EncryptedChatWaiting {
            id: 42,
            access_hash: 1,
            date: 1,
            admin_id: 1,
            participant_id: 7,
        }
        .into();
        let asked = classify(1, &waiting).unwrap();
        assert_eq!(
            asked.words("Odalys Brandt"),
            format!("You asked Odalys Brandt to start a secret chat on Telegram. {PHONE}")
        );
        let open = tl::types::EncryptedChat {
            id: 42,
            access_hash: 1,
            date: 1,
            admin_id: 7,
            participant_id: 1,
            g_a_or_b: vec![1],
            key_fingerprint: 9,
        };
        assert!(classify(1, &open.into())
            .unwrap()
            .words("Someone")
            .contains("is open on Telegram"));
        assert!(
            classify(
                1,
                &tl::types::EncryptedChatDiscarded {
                    history_deleted: false,
                    id: 42
                }
                .into()
            )
            .unwrap()
            .kind
                == Kind::Gone
        );
        assert!(classify(1, &tl::types::EncryptedChatEmpty { id: 42 }.into()).is_none());
    }
}
