//! On-demand translation of messages this account already has. The stored text is never replaced
//! and never sent: Telegram is asked by peer and message id. A secret chat is refused here as
//! well as in chatd, before the request.

use super::content;
use super::peers::Who;
use super::secret;
use super::wire::{self, Fault};
use super::Account;
use grammers_tl_types as tl;

/// At most one page. The shell asks again for whatever it has loaded past this.
const LIMIT: usize = 100;

pub(super) fn translate(
    account: &Account,
    conversation_id: &str,
    message_ids: &[String],
    to: &str,
) -> Result<Vec<protocol::TranslatedMessage>, Fault> {
    if secret::is_row(conversation_id) {
        return Err(Fault::Rpc { code: 400, name: "CHAT_SECRET".into(), value: None });
    }
    let Some((who, _)) = Who::parse(conversation_id) else {
        return Err(Fault::Rpc { code: 400, name: "PEER_ID_INVALID".into(), value: None });
    };
    // A comment row is asked of the discussion group that holds the message.
    let who = super::sync::comment_target(account, conversation_id)?.map_or(who, |(group, _)| group);
    let mut ids = Vec::new();
    for id in message_ids.iter().take(LIMIT) {
        let Ok(n) = id.parse::<i32>() else { continue };
        if n > 0 {
            ids.push(n);
        }
    }
    if ids.is_empty() {
        return Err(Fault::Rpc { code: 400, name: "MESSAGE_ID_INVALID".into(), value: None });
    }
    let request = tl::functions::messages::TranslateText {
        peer: Some(account.input(who)?),
        id: Some(ids.clone()),
        text: None,
        to_lang: to.to_owned(),
        tone: None,
    };
    let tl::enums::messages::TranslatedText::TranslateResult(found) = wire::call(account.wire.as_ref(), &request)?;
    let mut items = Vec::new();
    for (id, item) in ids.iter().zip(found.result) {
        let tl::enums::TextWithEntities::Entities(body) = item;
        items.push(protocol::TranslatedMessage {
            message_id: id.to_string(),
            text: body.text.clone(),
            spans: content::spans(&body.text, &body.entities),
        });
    }
    Ok(items)
}

/// One message, in the shape every network answers `Request::Translate` with.
pub(super) fn one(account: &Account, conversation_id: &str, message_id: &str, to: &str) -> Result<protocol::Translation, Fault> {
    let target_lang = protocol::normalize_language_tag(to);
    let text = translate(account, conversation_id, &[message_id.to_owned()], &target_lang)?.into_iter().next().map(|item| item.text).unwrap_or_default();
    if text.is_empty() {
        return Err(Fault::Refused("Telegram returned an empty translation.".to_owned()));
    }
    Ok(protocol::Translation { text, source_lang: None, target_lang })
}
