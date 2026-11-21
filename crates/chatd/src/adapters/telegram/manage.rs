//! Group and channel management for Telegram: editing info and photos, member moderation
//! (kick, ban, unban, restrict), admin promotion/demotion, ownership transfer with 2FA SRP proof,
//! group/channel settings (default permissions, slow mode, join approval, forum topics,
//! pre-history visibility, signatures, protected content), recent-actions admin log, and send-as
//! posting identities (`channels.getSendAs` / `messages.saveDefaultSendAs`).

use super::live;
use super::peers::Who;
use super::signin;
use super::sync;
use super::wire::Fault;
use super::{random_id, Account};
use grammers_tl_types as tl;
use protocol::{AdminLogEntry, AdminRights, ChatSettings, MemberModeration, SendAs, SendRules};

/// Part size for uploading a group/channel photo (512 KiB, matching `send.rs`).
const PART: usize = 512 * 1024;
/// Past 10 MiB Telegram expects `upload.saveBigFilePart`.
const BIG: u64 = 10 * 1024 * 1024;

fn invalid(name: &str) -> Fault {
    Fault::Rpc { code: 400, name: name.to_owned(), value: None }
}

/// Converts [`SendRules`] (allowed = true) into Telegram's [`tl::types::ChatBannedRights`]
/// (banned = true).
pub(super) fn banned_rights_from_rules(rules: &SendRules, until_date: i32) -> tl::enums::ChatBannedRights {
    tl::types::ChatBannedRights {
        view_messages: false,
        send_messages: !rules.send_messages,
        send_media: !rules.send_media,
        send_stickers: !rules.send_stickers,
        send_gifs: !rules.send_gifs,
        send_games: !rules.send_gifs,
        send_inline: !rules.send_gifs,
        embed_links: !rules.embed_links,
        send_polls: !rules.send_polls,
        change_info: true,
        invite_users: false,
        pin_messages: true,
        manage_topics: true,
        send_photos: !rules.send_photos,
        send_videos: !rules.send_videos,
        send_roundvideos: !rules.send_round_videos,
        send_audios: !rules.send_audios,
        send_voices: !rules.send_voices,
        send_docs: !rules.send_files,
        send_plain: !rules.send_plain,
        edit_rank: true,
        send_reactions: false,
        until_date,
    }
    .into()
}

fn blank_banned_rights(view_messages: bool, until_date: i32) -> tl::enums::ChatBannedRights {
    tl::types::ChatBannedRights {
        view_messages,
        send_messages: view_messages,
        send_media: false,
        send_stickers: false,
        send_gifs: false,
        send_games: false,
        send_inline: false,
        embed_links: false,
        send_polls: false,
        change_info: false,
        invite_users: false,
        pin_messages: false,
        manage_topics: false,
        send_photos: false,
        send_videos: false,
        send_roundvideos: false,
        send_audios: false,
        send_voices: false,
        send_docs: false,
        send_plain: false,
        edit_rank: false,
        send_reactions: false,
        until_date,
    }
    .into()
}

pub(super) fn admin_rights_to_tl(rights: &AdminRights) -> tl::enums::ChatAdminRights {
    tl::types::ChatAdminRights {
        change_info: rights.change_info,
        post_messages: rights.post_messages,
        edit_messages: rights.edit_messages,
        delete_messages: rights.delete_messages,
        ban_users: rights.ban_users,
        invite_users: rights.invite_users,
        pin_messages: rights.pin_messages,
        add_admins: rights.add_admins,
        anonymous: rights.anonymous,
        manage_call: rights.manage_call,
        other: false,
        manage_topics: rights.manage_topics,
        post_stories: false,
        edit_stories: false,
        delete_stories: false,
        manage_direct_messages: false,
        manage_ranks: false,
    }
    .into()
}

fn parse_user(account: &Account, person: &str) -> Result<(Who, tl::enums::InputUser), Fault> {
    let (who @ Who::User(_), None) = Who::parse(person).ok_or_else(|| invalid("USER_ID_INVALID"))? else {
        return Err(invalid("USER_ID_INVALID"));
    };
    account.input(who)?;
    let input = account.dir().input_user(who).ok_or_else(|| invalid("USER_ID_INVALID"))?;
    Ok((who, input))
}

/// Renames a group/channel and/or updates its description (`messages.editChatTitle` /
/// `channels.editTitle` + `messages.editChatAbout`).
pub(super) fn update_conversation(
    account: &Account,
    conversation: &str,
    title: Option<&str>,
    topic: Option<&str>,
    description: Option<&str>,
) -> Result<(), Fault> {
    let (who, _) = Who::parse(conversation).ok_or_else(|| invalid("PEER_ID_INVALID"))?;
    if let Some(title) = title.map(str::trim).filter(|t| !t.is_empty()) {
        let answer = match who {
            Who::Group(chat_id) => account.ask(&tl::functions::messages::EditChatTitle {
                chat_id,
                title: title.to_owned(),
            })?,
            Who::Channel(_) => account.ask(&tl::functions::channels::EditTitle {
                channel: account.input_channel(who)?,
                title: title.to_owned(),
            })?,
            Who::User(_) => return Err(invalid("PEER_ID_INVALID")),
        };
        live::answered(account, &answer);
        sync::refresh_row(account, who);
    }
    if let Some(about) = description.or(topic) {
        let peer = account.input(who)?;
        account.ask(&tl::functions::messages::EditChatAbout {
            peer,
            about: about.trim().to_owned(),
        })?;
    }
    Ok(())
}

/// Uploads a local photo file in 512 KiB chunks and returns Telegram's `InputFile`.
fn upload_photo(account: &Account, path: &str) -> Result<tl::enums::InputFile, Fault> {
    let (real, _) = crate::blobs::upload::allowed(path).map_err(|_| invalid("PHOTO_INVALID"))?;
    let bytes = std::fs::read(&real).map_err(|_| invalid("PHOTO_INVALID"))?;
    if bytes.is_empty() {
        return Err(invalid("PHOTO_INVALID"));
    }
    let name = real
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty())
        .unwrap_or("photo.jpg")
        .to_owned();
    let total = bytes.len() as u64;
    let big = total > BIG;
    let file_id = random_id(&format!("chat-photo:{name}:{total}:{}", crate::hub::now()));
    let chunks: Vec<&[u8]> = bytes.chunks(PART).collect();
    let parts = chunks.len() as i32;
    for (index, chunk) in chunks.into_iter().enumerate() {
        let ok = if big {
            account.ask(&tl::functions::upload::SaveBigFilePart {
                file_id,
                file_part: index as i32,
                file_total_parts: parts,
                bytes: chunk.to_vec(),
            })?
        } else {
            account.ask(&tl::functions::upload::SaveFilePart {
                file_id,
                file_part: index as i32,
                bytes: chunk.to_vec(),
            })?
        };
        if !ok {
            return Err(Fault::Net("Telegram did not keep a part of the photo.".to_owned()));
        }
    }
    Ok(if big {
        tl::types::InputFileBig { id: file_id, parts, name }.into()
    } else {
        tl::types::InputFile { id: file_id, parts, name, md5_checksum: String::new() }.into()
    })
}

/// Sets or clears a group/channel's photo (`messages.editChatPhoto` / `channels.editPhoto`).
pub(super) fn update_picture(account: &Account, conversation: &str, path: &str) -> Result<(), Fault> {
    let (who, _) = Who::parse(conversation).ok_or_else(|| invalid("PEER_ID_INVALID"))?;
    let photo = if path.trim().is_empty() {
        tl::enums::InputChatPhoto::Empty
    } else {
        let file = upload_photo(account, path.trim())?;
        tl::types::InputChatUploadedPhoto {
            file: Some(file),
            video: None,
            video_start_ts: None,
            video_emoji_markup: None,
        }
        .into()
    };
    let answer = match who {
        Who::Group(chat_id) => account.ask(&tl::functions::messages::EditChatPhoto { chat_id, photo })?,
        Who::Channel(_) => account.ask(&tl::functions::channels::EditPhoto {
            channel: account.input_channel(who)?,
            photo,
        })?,
        Who::User(_) => return Err(invalid("PEER_ID_INVALID")),
    };
    live::answered(account, &answer);
    sync::refresh_row(account, who);
    Ok(())
}

/// Adds or invites people to a group (`messages.addChatUser`) or channel/supergroup
/// (`channels.inviteToChannel`).
pub(super) fn members_add(account: &Account, conversation: &str, people: &[String]) -> Result<(), Fault> {
    let (who, _) = Who::parse(conversation).ok_or_else(|| invalid("PEER_ID_INVALID"))?;
    let mut users = Vec::with_capacity(people.len());
    for person in people {
        let (_, user) = parse_user(account, person)?;
        users.push(user);
    }
    match who {
        Who::Group(chat_id) => {
            for user_id in users {
                let tl::enums::messages::InvitedUsers::Users(invited) = account.ask(&tl::functions::messages::AddChatUser {
                    chat_id,
                    user_id,
                    fwd_limit: 50,
                })?;
                live::answered(account, &invited.updates);
            }
        }
        Who::Channel(_) => {
            let channel = account.input_channel(who)?;
            let tl::enums::messages::InvitedUsers::Users(invited) = account.ask(&tl::functions::channels::InviteToChannel {
                channel,
                users,
            })?;
            live::answered(account, &invited.updates);
        }
        Who::User(_) => return Err(invalid("PEER_ID_INVALID")),
    }
    account.live().roster = None;
    let _ = account.ctx.members_changed(conversation);
    Ok(())
}

/// Removes one member from a basic group (`messages.deleteChatUser`) or kicks them from a
/// channel/supergroup (`channels.editBanned`).
pub(super) fn members_remove(account: &Account, conversation: &str, person: &str) -> Result<(), Fault> {
    moderate_member(account, conversation, person, &MemberModeration::Kick)
}

/// Kicks, bans, unbans, or restricts one member of a group or channel.
pub(super) fn moderate_member(
    account: &Account,
    conversation: &str,
    person: &str,
    action: &MemberModeration,
) -> Result<(), Fault> {
    let (who, _) = Who::parse(conversation).ok_or_else(|| invalid("PEER_ID_INVALID"))?;
    let (user_who, input_user) = parse_user(account, person)?;
    match who {
        Who::Group(chat_id) => match action {
            MemberModeration::Kick | MemberModeration::Ban { .. } => {
                let answer = account.ask(&tl::functions::messages::DeleteChatUser {
                    revoke_history: false,
                    chat_id,
                    user_id: input_user,
                })?;
                live::answered(account, &answer);
            }
            MemberModeration::Unban | MemberModeration::Restrict { .. } => {
                return Err(invalid("CHAT_ADMIN_REQUIRED"));
            }
        },
        Who::Channel(_) => {
            let channel = account.input_channel(who)?;
            let participant = account.input(user_who)?;
            match action {
                MemberModeration::Kick => {
                    let banned = account.ask(&tl::functions::channels::EditBanned {
                        channel: channel.clone(),
                        participant: participant.clone(),
                        banned_rights: blank_banned_rights(true, 0),
                    })?;
                    live::answered(account, &banned);
                    // Immediately lift the ban so a kick removes without permanently banning.
                    let unbanned = account.ask(&tl::functions::channels::EditBanned {
                        channel,
                        participant,
                        banned_rights: blank_banned_rights(false, 0),
                    })?;
                    live::answered(account, &unbanned);
                }
                MemberModeration::Ban { until } => {
                    let until_date = until.unwrap_or(0).clamp(0, i64::from(i32::MAX)) as i32;
                    let answer = account.ask(&tl::functions::channels::EditBanned {
                        channel,
                        participant,
                        banned_rights: blank_banned_rights(true, until_date),
                    })?;
                    live::answered(account, &answer);
                }
                MemberModeration::Unban => {
                    let answer = account.ask(&tl::functions::channels::EditBanned {
                        channel,
                        participant,
                        banned_rights: blank_banned_rights(false, 0),
                    })?;
                    live::answered(account, &answer);
                }
                MemberModeration::Restrict { rules, until } => {
                    let until_date = until.unwrap_or(0).clamp(0, i64::from(i32::MAX)) as i32;
                    let answer = account.ask(&tl::functions::channels::EditBanned {
                        channel,
                        participant,
                        banned_rights: banned_rights_from_rules(rules, until_date),
                    })?;
                    live::answered(account, &answer);
                }
            }
        }
        Who::User(_) => return Err(invalid("PEER_ID_INVALID")),
    }
    account.live().roster = None;
    let _ = account.ctx.members_changed(conversation);
    Ok(())
}

/// Promotes or demotes an admin with specific rights and optional custom rank (`channels.editAdmin`).
pub(super) fn set_admin(
    account: &Account,
    conversation: &str,
    person: &str,
    rights: &AdminRights,
) -> Result<(), Fault> {
    let (who @ Who::Channel(_), _) = Who::parse(conversation).ok_or_else(|| invalid("PEER_ID_INVALID"))? else {
        return Err(invalid("CHANNEL_ONLY"));
    };
    let (_, user_id) = parse_user(account, person)?;
    let channel = account.input_channel(who)?;
    let rank = (!rights.rank.trim().is_empty()).then(|| rights.rank.trim().to_owned());
    let answer = account.ask(&tl::functions::channels::EditAdmin {
        channel,
        user_id,
        admin_rights: admin_rights_to_tl(rights),
        rank,
    })?;
    live::answered(account, &answer);
    account.live().roster = None;
    let _ = account.ctx.members_changed(conversation);
    Ok(())
}

/// Transfers ownership of a group or channel using Telegram's 2FA SRP password proof
/// (`account.getPassword` + `messages.editChatCreator`).
pub(super) fn transfer_ownership(
    account: &Account,
    conversation: &str,
    person: &str,
    password: &str,
) -> Result<(), Fault> {
    let (who, _) = Who::parse(conversation).ok_or_else(|| invalid("PEER_ID_INVALID"))?;
    if matches!(who, Who::User(_)) {
        return Err(invalid("PEER_ID_INVALID"));
    }
    let (_, user_id) = parse_user(account, person)?;
    let peer = account.input(who)?;
    let srp = signin::srp_proof(account.wire.as_ref(), password)?;
    let answer = account.ask(&tl::functions::messages::EditChatCreator {
        peer,
        user_id,
        password: srp,
    })?;
    live::answered(account, &answer);
    account.live().roster = None;
    let _ = account.ctx.members_changed(conversation);
    Ok(())
}

/// Updates group/channel defaults and toggles: default banned rights, slow mode, join approval,
/// forum mode, pre-history hidden, signatures, and protected content (`no_forwards`).
pub(super) fn update_settings(
    account: &Account,
    conversation: &str,
    settings: &ChatSettings,
) -> Result<(), Fault> {
    let (who, _) = Who::parse(conversation).ok_or_else(|| invalid("PEER_ID_INVALID"))?;
    if matches!(who, Who::User(_)) {
        return Err(invalid("PEER_ID_INVALID"));
    }
    let peer = account.input(who)?;
    let channel_only = settings.slowmode_seconds.is_some()
        || settings.join_request.is_some()
        || settings.forum.is_some()
        || settings.pre_history_hidden.is_some()
        || settings.signatures.is_some();
    if channel_only && !matches!(who, Who::Channel(_)) {
        return Err(invalid("CHANNEL_ONLY"));
    }

    if let Some(rules) = &settings.default_send_rules {
        let answer = account.ask(&tl::functions::messages::EditChatDefaultBannedRights {
            peer: peer.clone(),
            banned_rights: banned_rights_from_rules(rules, 0),
        })?;
        live::answered(account, &answer);
    }
    if let Some(seconds) = settings.slowmode_seconds {
        let channel = account.input_channel(who)?;
        let answer = account.ask(&tl::functions::channels::ToggleSlowMode {
            channel,
            seconds: seconds.min(i32::MAX as u32) as i32,
        })?;
        live::answered(account, &answer);
    }
    if let Some(enabled) = settings.join_request {
        let channel = account.input_channel(who)?;
        let answer = account.ask(&tl::functions::channels::ToggleJoinRequest {
            apply_to_invites: false,
            channel,
            enabled,
            guard_bot: None,
        })?;
        live::answered(account, &answer);
    }
    if let Some(enabled) = settings.forum {
        let channel = account.input_channel(who)?;
        let answer = account.ask(&tl::functions::channels::ToggleForum {
            channel,
            enabled,
            tabs: false,
        })?;
        live::answered(account, &answer);
    }
    if let Some(enabled) = settings.pre_history_hidden {
        let channel = account.input_channel(who)?;
        let answer = account.ask(&tl::functions::channels::TogglePreHistoryHidden {
            channel,
            enabled,
        })?;
        live::answered(account, &answer);
    }
    if let Some(enabled) = settings.signatures {
        let channel = account.input_channel(who)?;
        let answer = account.ask(&tl::functions::channels::ToggleSignatures {
            signatures_enabled: enabled,
            profiles_enabled: false,
            channel,
        })?;
        live::answered(account, &answer);
    }
    if let Some(enabled) = settings.no_forwards {
        let answer = account.ask(&tl::functions::messages::ToggleNoForwards {
            peer,
            enabled,
            request_msg_id: None,
        })?;
        live::answered(account, &answer);
    }
    sync::refresh_row(account, who);
    Ok(())
}

/// Formats one `ChannelAdminLogEventAction` into a plain-English summary line.
pub(super) fn describe_admin_action(action: &tl::enums::ChannelAdminLogEventAction) -> String {
    use tl::enums::ChannelAdminLogEventAction as A;
    match action {
        A::ChangeTitle(ev) => format!("Changed title to \"{}\"", ev.new_value),
        A::ChangeAbout(ev) => {
            if ev.new_value.is_empty() {
                "Cleared description".to_owned()
            } else {
                format!("Updated description to \"{}\"", ev.new_value)
            }
        }
        A::ChangePhoto(_) => "Updated photo".to_owned(),
        A::ChangeUsername(_) | A::ChangeUsernames(_) => "Changed public link".to_owned(),
        A::ToggleSlowMode(ev) => {
            if ev.new_value <= 0 {
                "Turned slow mode off".to_owned()
            } else {
                format!("Set slow mode to {}s", ev.new_value)
            }
        }
        A::ToggleNoForwards(ev) => {
            if ev.new_value {
                "Enabled protected content".to_owned()
            } else {
                "Disabled protected content".to_owned()
            }
        }
        A::ToggleForum(ev) => {
            if ev.new_value {
                "Enabled forum topics".to_owned()
            } else {
                "Disabled forum topics".to_owned()
            }
        }
        A::TogglePreHistoryHidden(ev) => {
            if ev.new_value {
                "Hid chat history for new members".to_owned()
            } else {
                "Made chat history visible for new members".to_owned()
            }
        }
        A::ToggleSignatures(ev) => {
            if ev.new_value {
                "Enabled post signatures".to_owned()
            } else {
                "Disabled post signatures".to_owned()
            }
        }
        A::DefaultBannedRights(_) => "Updated default member permissions".to_owned(),
        A::ParticipantToggleBan(_) => "Updated member restriction or ban".to_owned(),
        A::ParticipantToggleAdmin(_) => "Updated admin permissions".to_owned(),
        A::DeleteMessage(_) => "Deleted a message".to_owned(),
        A::EditMessage(_) => "Edited a message".to_owned(),
        A::UpdatePinned(_) => "Updated pinned message".to_owned(),
        A::ParticipantJoin | A::ParticipantJoinByInvite(_) | A::ParticipantJoinByRequest(_) => "Joined the chat".to_owned(),
        A::ParticipantLeave => "Left the chat".to_owned(),
        A::ParticipantInvite(_) => "Added a member".to_owned(),
        A::ChangeHistoryTtl(_) => "Changed auto-delete timer".to_owned(),
        A::ChangeAvailableReactions(_) => "Updated allowed reactions".to_owned(),
        A::CreateTopic(_) => "Created a topic".to_owned(),
        A::EditTopic(_) => "Edited a topic".to_owned(),
        A::DeleteTopic(_) => "Deleted a topic".to_owned(),
        _ => "Admin action".to_owned(),
    }
}

/// Reads recent admin actions for a channel/supergroup (`channels.getAdminLog`).
pub(super) fn admin_log(account: &Account, conversation: &str, limit: u32) -> Result<Vec<AdminLogEntry>, Fault> {
    let (who @ Who::Channel(_), _) = Who::parse(conversation).ok_or_else(|| invalid("PEER_ID_INVALID"))? else {
        return Err(invalid("CHANNEL_INVALID"));
    };
    let channel = account.input_channel(who)?;
    let tl::enums::channels::AdminLogResults::Results(results) = account.ask(&tl::functions::channels::GetAdminLog {
        channel,
        q: String::new(),
        events_filter: None,
        admins: None,
        max_id: 0,
        min_id: 0,
        limit: limit.clamp(1, 100) as i32,
    })?;
    account.learn(&results.users, &results.chats);
    let dir = account.dir();
    let entries = results
        .events
        .into_iter()
        .map(|tl::enums::ChannelAdminLogEvent::Event(ev)| {
            let actor = dir.name(Who::User(ev.user_id));
            AdminLogEntry {
                id: ev.id.to_string(),
                date: i64::from(ev.date),
                actor,
                summary: describe_admin_action(&ev.action),
            }
        })
        .collect();
    Ok(entries)
}

/// Lists selectable posting identities (`channels.getSendAs`) and the default send-as peer.
pub(super) fn send_as_options(account: &Account, conversation: &str) -> Result<(Vec<SendAs>, Option<String>), Fault> {
    let (who @ Who::Channel(_), _) = Who::parse(conversation).ok_or_else(|| invalid("PEER_ID_INVALID"))? else {
        return Ok((Vec::new(), None));
    };
    let peer = account.input(who)?;
    let tl::enums::channels::SendAsPeers::Peers(found) = account.ask(&tl::functions::channels::GetSendAs {
        for_paid_reactions: false,
        for_live_stories: false,
        peer,
    })?;
    account.learn(&found.users, &found.chats);
    let dir = account.dir();
    let mut peers = Vec::with_capacity(found.peers.len());
    for tl::enums::SendAsPeer::Peer(entry) in found.peers {
        let who = Who::of(&entry.peer);
        let label = dir.name(who);
        let detail = if entry.premium_required {
            "Telegram Premium".to_owned()
        } else if matches!(who, Who::Channel(_)) {
            "Channel".to_owned()
        } else {
            "Personal account".to_owned()
        };
        peers.push(SendAs { id: who.text(), label, color: None, detail });
    }
    let default = peers.first().map(|p| p.id.clone());
    Ok((peers, default))
}

/// Saves the default posting identity for a conversation (`messages.saveDefaultSendAs`).
pub(super) fn set_default_send_as(account: &Account, conversation: &str, peer_id: &str) -> Result<(), Fault> {
    let (who, _) = Who::parse(conversation).ok_or_else(|| invalid("PEER_ID_INVALID"))?;
    let (as_who, None) = Who::parse(peer_id).ok_or_else(|| invalid("SEND_AS_PEER_INVALID"))? else {
        return Err(invalid("SEND_AS_PEER_INVALID"));
    };
    let peer = account.input(who)?;
    let send_as = account.input(as_who)?;
    account.ask(&tl::functions::messages::SaveDefaultSendAs { peer, send_as })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banned_rights_inverts_send_rules_flags() {
        let rules = SendRules {
            send_media: false,
            send_stickers: false,
            send_polls: false,
            embed_links: false,
            ..SendRules::default()
        };
        let tl::enums::ChatBannedRights::Rights(r) = banned_rights_from_rules(&rules, 1_900_000_000);
        assert!(!r.view_messages && !r.send_messages && !r.send_plain);
        assert!(r.send_media && r.send_stickers && r.send_polls && r.embed_links);
        assert!(!r.send_gifs && !r.send_voices);
        assert_eq!(r.until_date, 1_900_000_000);
    }

    #[test]
    fn admin_log_actions_produce_readable_summaries() {
        let title = tl::enums::ChannelAdminLogEventAction::ChangeTitle(
            tl::types::ChannelAdminLogEventActionChangeTitle { prev_value: "Old".into(), new_value: "Ridge".into() },
        );
        assert_eq!(describe_admin_action(&title), "Changed title to \"Ridge\"");
        let slow = tl::enums::ChannelAdminLogEventAction::ToggleSlowMode(
            tl::types::ChannelAdminLogEventActionToggleSlowMode { prev_value: 0, new_value: 30 },
        );
        assert_eq!(describe_admin_action(&slow), "Set slow mode to 30s");
        let nf = tl::enums::ChannelAdminLogEventAction::ToggleNoForwards(
            tl::types::ChannelAdminLogEventActionToggleNoForwards { new_value: true },
        );
        assert_eq!(describe_admin_action(&nf), "Enabled protected content");
    }
}
