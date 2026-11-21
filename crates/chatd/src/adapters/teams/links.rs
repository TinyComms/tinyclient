//! Microsoft's documented Teams deep links (`https://teams.microsoft.com/l/…`), and the same
//! paths with the `msteams:` scheme. Copied from the Teams platform docs (chat, channel,
//! message, team). A link that does not name a conversation we could open is `None`.

use super::{escaped, TEAMS};

/// Which documented form a link is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShareKind {
    Chat,
    Channel,
    Message,
    Team,
    Meeting,
}

/// A deep link opened back into a conversation, a message, or a team.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShareTarget {
    pub kind: ShareKind,
    pub conversation: String,
    pub message: String,
    pub parent: String,
    pub group: String,
    pub title: String,
}

impl ShareTarget {
    fn bare(kind: ShareKind) -> ShareTarget {
        ShareTarget {
            kind,
            conversation: String::new(),
            message: String::new(),
            parent: String::new(),
            group: String::new(),
            title: String::new(),
        }
    }
}

/// `https://<teams-host>/l/chat/<chatId>/conversations`
pub(crate) fn chat_share_on(teams_host: &str, thread: &str) -> String {
    format!("{}/l/chat/{}/conversations", teams_host.trim_end_matches('/'), escaped(thread))
}

/// `https://teams.microsoft.com/l/chat/<chatId>/conversations`
pub(crate) fn chat_share(thread: &str) -> String {
    chat_share_on(TEAMS, thread)
}

/// `https://<teams-host>/l/channel/<channelId>/<channelName>?groupId=&tenantId=`
pub(crate) fn channel_share_on(teams_host: &str, thread: &str, title: &str, group: &str, tenant: &str) -> String {
    format!(
        "{}/l/channel/{}/{}?groupId={}&tenantId={}",
        teams_host.trim_end_matches('/'),
        escaped(thread),
        escaped(title),
        escaped(group),
        escaped(tenant)
    )
}

/// `https://teams.microsoft.com/l/channel/<channelId>/<channelName>?groupId=&tenantId=`
pub(crate) fn channel_share(thread: &str, title: &str, group: &str, tenant: &str) -> String {
    channel_share_on(TEAMS, thread, title, group, tenant)
}

/// A message in a channel, or in a chat (which adds the documented `context` query).
/// `parent` is the post a channel reply hangs off. The post's own id is not a parent.
pub(crate) fn message_share_on(
    teams_host: &str,
    conversation: &str,
    id: &str,
    tenant: &str,
    parent: Option<&str>,
) -> String {
    let thread = super::thread_of(conversation);
    let channel = thread.contains("@thread.tacv2") || thread.contains("@thread.skype");
    let mut url = format!(
        "{}/l/message/{}/{}?tenantId={}",
        teams_host.trim_end_matches('/'),
        escaped(thread),
        escaped(id),
        escaped(tenant)
    );
    if let Some(parent) = parent.filter(|parent| !parent.is_empty() && *parent != id) {
        url.push_str("&parentMessageId=");
        url.push_str(&escaped(parent));
    }
    if !channel {
        url.push_str("&context=%7B%22contextType%22%3A%22chat%22%7D");
    }
    url
}

/// A message in a channel, or in a chat, on the worldwide Teams host.
pub(crate) fn message_share(
    conversation: &str,
    id: &str,
    tenant: &str,
    parent: Option<&str>,
) -> String {
    message_share_on(TEAMS, conversation, id, tenant, parent)
}

/// `https://teams.microsoft.com/l/team/<channelId>/conversations?groupId=&tenantId=`
pub(crate) fn team_share(channel: &str, group: &str, tenant: &str) -> String {
    format!(
        "{TEAMS}/l/team/{}/conversations?groupId={}&tenantId={}",
        escaped(channel),
        escaped(group),
        escaped(tenant)
    )
}

/// `https://<teams-host>/l/meetup-join/<thread>/0`
pub(crate) fn meeting_share_on(teams_host: &str, thread: &str) -> String {
    format!("{}/l/meetup-join/{}/0", teams_host.trim_end_matches('/'), escaped(thread))
}

/// `https://teams.microsoft.com/l/meetup-join/<thread>/0`
pub(crate) fn meeting_share(thread: &str) -> String {
    meeting_share_on(TEAMS, thread)
}

/// The conversation, message and team a Teams link names. `None` when it is not one, or when
/// it starts a new chat (`/l/chat/0/0`) and names no thread we have.
pub(crate) fn parse_share(url: &str) -> Option<ShareTarget> {
    let rest = after_scheme(url.trim())?;
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    let parts: Vec<String> = path
        .split('/')
        .filter(|part| !part.is_empty())
        .map(decode)
        .collect();
    let first = parts.first().map(String::as_str)?;
    let id = parts.get(1).filter(|id| !id.is_empty())?.clone();
    match first {
        "chat" if id != "0" => {
            let mut found = ShareTarget::bare(ShareKind::Chat);
            found.conversation = id;
            Some(found)
        }
        "channel" => {
            let mut found = ShareTarget::bare(ShareKind::Channel);
            found.conversation = id;
            found.title = parts.get(2).cloned().unwrap_or_default();
            found.group = query_value(query, "groupId");
            Some(found)
        }
        "message" => {
            let message = parts.get(2).filter(|id| !id.is_empty())?.clone();
            let parent = query_value(query, "parentMessageId");
            let mut found = ShareTarget::bare(ShareKind::Message);
            found.conversation = id;
            found.message = message.clone();
            found.parent = if parent == message {
                String::new()
            } else {
                parent
            };
            Some(found)
        }
        "team" => {
            let mut found = ShareTarget::bare(ShareKind::Team);
            found.conversation = id;
            found.group = query_value(query, "groupId");
            Some(found)
        }
        "meetup-join" => {
            let mut found = ShareTarget::bare(ShareKind::Meeting);
            found.conversation = id;
            Some(found)
        }
        _ => None,
    }
}

/// The lab's teams-copy-link driver. The four documented forms parse back to the same place.
pub(crate) fn prove_share_links() -> Result<(), String> {
    let tenant = "11111111-1111-4111-8111-111111111111";
    let channel = "19:platform0general@thread.tacv2";
    let chat = "19:grp1@thread.v2";
    let group = "e0873025-0000-4000-8000-00000000beef";
    let chat_link = chat_share(chat);
    match parse_share(&chat_link) {
        Some(found) if found.kind == ShareKind::Chat && found.conversation == chat => {}
        other => return Err(format!("chat link {chat_link} parsed as {other:?}")),
    }
    let channel_link = channel_share(channel, "General", group, tenant);
    match parse_share(&channel_link) {
        Some(found)
            if found.kind == ShareKind::Channel
                && found.conversation == channel
                && found.group == group
                && found.title == "General" => {}
        other => return Err(format!("channel link {channel_link} parsed as {other:?}")),
    }
    let message_link = message_share(channel, "1760000210210", tenant, Some("1760000200200"));
    match parse_share(&message_link) {
        Some(found)
            if found.kind == ShareKind::Message
                && found.conversation == channel
                && found.message == "1760000210210"
                && found.parent == "1760000200200" => {}
        other => return Err(format!("message link {message_link} parsed as {other:?}")),
    }
    let team_link = team_share(channel, group, tenant);
    match parse_share(&team_link) {
        Some(found)
            if found.kind == ShareKind::Team
                && found.conversation == channel
                && found.group == group => {}
        other => return Err(format!("team link {team_link} parsed as {other:?}")),
    }
    if parse_share("https://ex.com/plan").is_some()
        || parse_share("https://teams.microsoft.com/l/chat/0/0?users=a@contoso.com").is_some()
    {
        return Err("a link that is not a conversation was accepted".to_owned());
    }
    Ok(())
}

fn after_scheme(url: &str) -> Option<&str> {
    let heads = [
        "https://teams.microsoft.com/l/",
        "http://teams.microsoft.com/l/",
        "https://gov.teams.microsoft.us/l/",
        "https://dod.teams.microsoft.us/l/",
        "https://teams.microsoft.cn/l/",
        "msteams://l/",
        "msteams:/l/",
    ];
    heads.into_iter().find_map(|head| {
        let start = url.get(..head.len())?;
        start
            .eq_ignore_ascii_case(head)
            .then_some(&url[head.len()..])
    })
}

fn query_value(query: &str, name: &str) -> String {
    query
        .split('&')
        .find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            (decode(key) == name).then(|| decode(value))
        })
        .unwrap_or_default()
}

fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let hex = bytes
            .get(at + 1..at + 3)
            .and_then(|hex| std::str::from_utf8(hex).ok());
        match (
            bytes[at],
            hex.and_then(|hex| u8::from_str_radix(hex, 16).ok()),
        ) {
            (b'%', Some(byte)) => {
                out.push(byte);
                at += 3;
            }
            (byte, _) => {
                out.push(byte);
                at += 1;
            }
        }
    }
    String::from_utf8(out)
        .unwrap_or_else(|err| String::from_utf8_lossy(err.as_bytes()).into_owned())
}
