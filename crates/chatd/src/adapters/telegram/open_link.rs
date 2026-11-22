//! A `t.me` or `telegram.me` link the person clicked. Opens a chat already here, or a preview of
//! a public username. Never joins, never imports an invite, and never fetches a private `c/` id
//! this account does not already know.

use super::peers::Who;
use super::secret;
use super::sync;
use super::wire::{self, Fault};
use super::Account;
use crate::adapters::OpenedLink;
use grammers_tl_types as tl;

pub(super) fn open(account: &Account, url: &str) -> Result<Option<OpenedLink>, Fault> {
    let Some(parsed) = protocol::link::telegram_app_link(url) else {
        return Ok(None);
    };
    let message_id = parsed.message_id;
    if secret::is_row(&parsed.channel) {
        return Ok(None);
    }
    // t.me/c/{id} is a private supergroup. Show it only when this account already has the row.
    if parsed.space.as_deref() == Some("c") {
        return stored(account, &parsed.channel, message_id);
    }
    if let Some(opened) = stored(account, &parsed.channel, message_id.clone())? {
        return Ok(Some(opened));
    }
    let who = match account.dir().by_handle(&parsed.channel) {
        Some(who) => Some(who),
        None => resolve_username(account, &parsed.channel)?,
    };
    let Some(who) = who else {
        return Ok(None);
    };
    if secret::is_row(&who.text()) {
        return Ok(None);
    }
    if let Some(opened) = stored(account, &who.text(), message_id.clone())? {
        return Ok(Some(opened));
    }
    let Some(title) = sync::preview(account, who)? else {
        return Ok(None);
    };
    Ok(Some(OpenedLink { conversation_id: who.text(), message_id, title }))
}

fn stored(account: &Account, id: &str, message_id: Option<String>) -> Result<Option<OpenedLink>, Fault> {
    if secret::is_row(id) {
        return Ok(None);
    }
    match account.ctx.known(id) {
        Ok(Some(row)) => Ok(Some(OpenedLink { conversation_id: row.key.id, message_id, title: row.title })),
        Ok(None) => Ok(None),
        Err(err) => Err(Fault::Net(err.to_string())),
    }
}

/// A read of `contacts.resolveUsername`. A flood or a network failure is returned; a missing
/// name is "not this chat", not an error the shell has to show.
fn resolve_username(account: &Account, username: &str) -> Result<Option<Who>, Fault> {
    let request = tl::functions::contacts::ResolveUsername { username: username.to_owned(), referer: None };
    match wire::call(account.wire.as_ref(), &request) {
        Ok(tl::enums::contacts::ResolvedPeer::Peer(found)) => {
            let who = Who::of(&found.peer);
            account.learn(&found.users, &found.chats);
            Ok(Some(who))
        }
        Err(fault) if fault.is("USERNAME_NOT_OCCUPIED") || fault.is("USERNAME_INVALID") => Ok(None),
        Err(fault) => Err(fault),
    }
}
