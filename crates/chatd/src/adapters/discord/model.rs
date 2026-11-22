//! Discord's objects as the gateway and the HTTP API send them, naming only the fields used.
//! Everything else in a payload is skipped while parsing.

use crate::store::PersonRow;
use protocol::{MediaFile, MediaKind, Presence};
use serde::de::{self, Deserializer, IgnoredAny};
use serde::{Deserialize, Serialize};

const CDN: &str = "https://cdn.discordapp.com";
const MEDIA: &str = "https://media.discordapp.net";

/// Where avatars, icons and emoji are served from. `TINYCLIENT_DISCORD_CDN` replaces it, and the
/// media proxy with it, so a test's pictures come from its own fake and never from Discord.
pub fn cdn() -> &'static str {
    static CDN_AT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    CDN_AT.get_or_init(|| super::super::net::address("TINYCLIENT_DISCORD_CDN", CDN))
}

/// The media proxy, which serves stickers and resized pictures.
pub fn media() -> &'static str {
    static MEDIA_AT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    MEDIA_AT.get_or_init(|| super::super::net::address("TINYCLIENT_DISCORD_CDN", MEDIA))
}

/// Channel types.
pub const TEXT: u8 = 0;
pub const DM: u8 = 1;
pub const VOICE: u8 = 2;
pub const GROUP_DM: u8 = 3;
pub const CATEGORY: u8 = 4;
pub const NEWS: u8 = 5;
pub const NEWS_THREAD: u8 = 10;
pub const PUBLIC_THREAD: u8 = 11;
pub const PRIVATE_THREAD: u8 = 12;
pub const STAGE: u8 = 13;
pub const FORUM: u8 = 15;
pub const MEDIA_FORUM: u8 = 16;

pub const CREATE_INSTANT_INVITE: u64 = 1 << 0;
/// Channel flags: bit 1 = pinned thread in a forum, bit 4 = tag required to post in a forum.
pub const THREAD_PINNED: u32 = 1 << 1;
pub const REQUIRE_TAG: u32 = 1 << 4;

pub const KICK_MEMBERS: u64 = 1 << 1;
pub const BAN_MEMBERS: u64 = 1 << 2;
pub const ADMINISTRATOR: u64 = 1 << 3;
pub const MANAGE_CHANNELS: u64 = 1 << 4;
pub const MANAGE_GUILD: u64 = 1 << 5;
pub const ADD_REACTIONS: u64 = 1 << 6;
pub const VIEW_AUDIT_LOG: u64 = 1 << 7;
#[allow(dead_code)]
pub const STREAM: u64 = 1 << 9;
pub const VIEW_CHANNEL: u64 = 1 << 10;
pub const SEND_MESSAGES: u64 = 1 << 11;
pub const MANAGE_MESSAGES: u64 = 1 << 13;
pub const READ_MESSAGE_HISTORY: u64 = 1 << 16;
pub const CONNECT: u64 = 1 << 20;
pub const SPEAK: u64 = 1 << 21;
pub const MOVE_MEMBERS: u64 = 1 << 24;
#[allow(dead_code)]
pub const USE_VAD: u64 = 1 << 25;
pub const CHANGE_NICKNAME: u64 = 1 << 26;
pub const MANAGE_NICKNAMES: u64 = 1 << 27;
pub const MANAGE_ROLES: u64 = 1 << 28;
pub const MODERATE_MEMBERS: u64 = 1 << 40;
pub const MANAGE_THREADS: u64 = 1 << 34;
pub const CREATE_PUBLIC_THREADS: u64 = 1 << 35;
pub const CREATE_PRIVATE_THREADS: u64 = 1 << 36;
pub const SEND_MESSAGES_IN_THREADS: u64 = 1 << 38;
pub const MANAGE_WEBHOOKS: u64 = 1 << 29;
pub const MANAGE_EVENTS: u64 = 1 << 33;
pub const SEND_VOICE_MESSAGES: u64 = 1 << 46;
pub const MANAGE_GUILD_EXPRESSIONS: u64 = 1 << 30;
pub const CREATE_GUILD_EXPRESSIONS: u64 = 1 << 43;
pub const PIN_MESSAGES: u64 = 1 << 51;

/// Formats Unix seconds as UTC ISO 8601 (`YYYY-MM-DDTHH:MM:SSZ`) for Discord REST bodies.
pub fn iso_timestamp(unix_secs: i64) -> String {
    let days = unix_secs.div_euclid(86_400);
    let secs = unix_secs.rem_euclid(86_400);
    let hour = secs / 3_600;
    let minute = (secs % 3_600) / 60;
    let second = secs % 60;
    // Howard Hinnant's civil_from_days algorithm.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = y + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}
/// Discord message flags.
pub const CROSSPOSTED: u64 = 1 << 0;
pub const IS_CROSSPOST: u64 = 1 << 1;
pub const SUPPRESS_EMBEDS: u64 = 1 << 2;
/// Validates a Discord snowflake id before interpolating it into an API path.
pub fn safe_discord_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 32 && id.bytes().all(|b| b.is_ascii_digit())
}

/// Where Discord's own app, or its site when the app is not installed, shows a person.
pub fn user_link(id: &str) -> String {
    format!("https://discord.com/users/{id}")
}

/// Unix seconds from a Discord id, which starts with a timestamp.
pub fn seconds(id: &str) -> i64 {
    let id: u64 = id.parse().unwrap_or(0);
    (((id >> 22) + 1_420_070_400_000) / 1000) as i64
}

/// Lowest Discord snowflake for a Unix timestamp in seconds, clamped to Discord's 2015 epoch.
pub fn snowflake_from_unix(unix_secs: i64) -> u64 {
    const DISCORD_EPOCH_MS: i64 = 1_420_070_400_000;
    let ms = unix_secs.saturating_mul(1000).saturating_sub(DISCORD_EPOCH_MS);
    if ms <= 0 {
        0
    } else {
        (ms as u64) << 22
    }
}

pub fn number(id: &str) -> u64 {
    id.parse().unwrap_or(0)
}

/// Discord sends some numbers as strings (permissions) and some ids as either.
fn lenient_u64<'de, D: Deserializer<'de>>(de: D) -> Result<u64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Either {
        Number(u64),
        Text(String),
    }
    Ok(match Option::<Either>::deserialize(de)? {
        Some(Either::Number(number)) => number,
        Some(Either::Text(text)) => text.parse().unwrap_or(0),
        None => 0,
    })
}

/// An overwrite's `type` is 0 or 1, and `"role"` or `"member"` on old payloads.
fn overwrite_kind<'de, D: Deserializer<'de>>(de: D) -> Result<bool, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Either {
        Number(u8),
        Text(String),
    }
    Ok(match Either::deserialize(de)? {
        Either::Number(number) => number == 1,
        Either::Text(text) => text == "member" || text == "1",
    })
}

/// A field that is `null`, absent or a value: all but the last are `None`.
fn nullable<'de, D: Deserializer<'de>, T: Deserialize<'de> + Default>(de: D) -> Result<T, D::Error> {
    Ok(Option::<T>::deserialize(de)?.unwrap_or_default())
}

/// A phone number is a secret. Missing stays unknown (`None`, and this is not called).
/// Null or an empty string is "no phone". Anything else is "a phone is set", and the digits are dropped.
fn phone_known<'de, D: Deserializer<'de>>(de: D) -> Result<Option<bool>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Phone {
        Text(String),
        Other(IgnoredAny),
    }
    let known = match Option::<Phone>::deserialize(de)? {
        None => false,
        Some(Phone::Text(text)) => !text.trim().is_empty(),
        Some(Phone::Other(_)) => true,
    };
    Ok(Some(known))
}

#[derive(Deserialize, Default, Clone, Debug)]
pub struct PrimaryGuild {
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub identity_enabled: Option<bool>,
}

#[derive(Deserialize, Default, Clone, Debug)]
pub struct User {
    pub id: String,
    #[serde(default, deserialize_with = "nullable")]
    pub username: String,
    #[serde(default)]
    pub global_name: Option<String>,
    #[serde(default)]
    pub avatar: Option<String>,
    #[serde(default)]
    pub banner: Option<String>,
    #[serde(default)]
    pub accent_color: Option<u32>,
    #[serde(default)]
    pub primary_guild: Option<PrimaryGuild>,
    #[serde(default, deserialize_with = "nullable")]
    pub discriminator: String,
    #[serde(default, deserialize_with = "nullable")]
    pub bot: bool,
    /// Discord's own account, which writes the system DMs.
    #[serde(default, deserialize_with = "nullable")]
    pub system: bool,
    /// Our own plan, on READY's user: 0 none, 1 Nitro Classic, 2 Nitro, 3 Nitro Basic.
    #[serde(default, deserialize_with = "nullable")]
    pub premium_type: u8,
    /// On READY's user: `Some(false)` when the account is underage / not permitted in age-restricted channels.
    #[serde(default)]
    pub nsfw_allowed: Option<bool>,
    /// Verified email, when Discord says. Missing and null are unknown, not a failure.
    #[serde(default)]
    pub verified: Option<bool>,
    /// Whether a phone is on the account. The number is never stored.
    #[serde(default, deserialize_with = "phone_known")]
    pub phone: Option<bool>,
    /// Present on the users of a message's `mentions`.
    #[serde(default)]
    pub member: Option<Box<Member>>,
}

impl User {
    pub fn name(&self) -> &str {
        match self.global_name.as_deref() {
            Some(name) if !name.is_empty() => name,
            _ => &self.username,
        }
    }

    /// The picture at list size, and where it is fetched from. No session is needed.
    pub fn avatar(&self) -> (MediaFile, String) {
        avatar(&self.id, self.avatar.as_deref(), &self.discriminator)
    }

    /// The row People stores. `name` wins over the user's own names (a friend nickname).
    pub fn person(&self, name: Option<&str>) -> PersonRow {
        PersonRow {
            link: user_link(&self.id),
            id: self.id.clone(),
            name: name.filter(|name| !name.is_empty()).unwrap_or(self.name()).to_owned(),
            handle: self.username.clone(),
            avatar: Some(self.avatar().0.id),
            bot: self.bot,
            status: String::new(),
        }
    }
}

fn sized(id: String, kind: MediaKind, side: u32) -> MediaFile {
    MediaFile {
        width: side,
        height: side,
        ..MediaFile::new(id, kind)
    }
}

/// What a CDN address adds for a picture that moves: Discord marks those with an `a_` hash,
/// and serves their WebP still unless `animated=true` is asked for. The same small file then
/// holds every frame; the window plays it under the pointer or as its setting says.
fn moving(hash: &str) -> &'static str {
    if hash.starts_with("a_") {
        "&animated=true"
    } else {
        ""
    }
}

pub fn avatar(user: &str, hash: Option<&str>, discriminator: &str) -> (MediaFile, String) {
    match hash {
        Some(hash) if cdn_token(hash) => (
            sized(format!("avatar:{user}:{hash}"), MediaKind::Avatar, 80),
            format!("{}/avatars/{user}/{hash}.webp?size=80{}", cdn(), moving(hash)),
        ),
        _ => {
            // New-style names (discriminator 0) pick by id, old ones by discriminator.
            let index = match discriminator.parse::<u64>() {
                Ok(old) if old > 0 => old % 5,
                _ => (number(user) >> 22) % 6,
            };
            (
                sized(format!("avatar:default:{index}"), MediaKind::Avatar, 80),
                format!("{}/embed/avatars/{index}.png", cdn()),
            )
        }
    }
}

/// A registered avatar at the sizes a profile card draws it (80 pt, so 160 px on a 2x screen;
/// the CDN serves powers of two). `None` for a default avatar, which has one size, and for an
/// id that is not an avatar's. Supports both global (`avatar:`) and per-server (`mavatar:`).
pub fn avatar_variants(file: &str) -> Option<Vec<(u32, String)>> {
    if let Some(rest) = file.strip_prefix("mavatar:") {
        let mut parts = rest.split(':');
        let (guild, user, hash) = (parts.next()?, parts.next()?, parts.next()?);
        if guild.is_empty() || user.is_empty() || hash.is_empty() {
            return None;
        }
        return Some(
            [128, 256]
                .into_iter()
                .map(|size| {
                    (
                        size,
                        format!("{}/guilds/{guild}/users/{user}/avatars/{hash}.webp?size={size}{}", cdn(), moving(hash)),
                    )
                })
                .collect(),
        );
    }
    let mut parts = file.strip_prefix("avatar:")?.split(':');
    let (user, hash) = (parts.next()?, parts.next()?);
    if user == "default" || !cdn_token(hash) {
        return None;
    }
    Some([128, 256].into_iter().map(|size| (size, format!("{}/avatars/{user}/{hash}.webp?size={size}{}", cdn(), moving(hash)))).collect())
}

pub fn member_avatar(guild: &str, user: &str, hash: &str) -> Option<(MediaFile, String)> {
    if !cdn_token(hash) {
        return None;
    }
    Some((
        sized(format!("mavatar:{guild}:{user}:{hash}"), MediaKind::Avatar, 80),
        format!("{}/guilds/{guild}/users/{user}/avatars/{hash}.webp?size=80{}", cdn(), moving(hash)),
    ))
}

/// A Discord CDN hash or invite code: non-empty ASCII letters, digits, `_` and `-`.
pub fn cdn_token(token: &str) -> bool {
    !token.is_empty() && token.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

pub fn user_banner(user: &str, hash: &str) -> Option<(MediaFile, String)> {
    if !cdn_token(hash) {
        return None;
    }
    Some((
        MediaFile {
            width: 600,
            height: 240,
            ..MediaFile::new(format!("banner:{user}:{hash}"), MediaKind::Image)
        },
        format!("{}/banners/{user}/{hash}.webp?size=600{}", cdn(), moving(hash)),
    ))
}

pub fn member_banner(guild: &str, user: &str, hash: &str) -> Option<(MediaFile, String)> {
    if !cdn_token(hash) {
        return None;
    }
    Some((
        MediaFile {
            width: 600,
            height: 240,
            ..MediaFile::new(format!("mbanner:{guild}:{user}:{hash}"), MediaKind::Image)
        },
        format!("{}/guilds/{guild}/users/{user}/banners/{hash}.webp?size=600{}", cdn(), moving(hash)),
    ))
}

pub fn badge_icon(hash: &str) -> Option<(MediaFile, String)> {
    if !cdn_token(hash) {
        return None;
    }
    Some((
        sized(format!("badge:{hash}"), MediaKind::Image, 24),
        format!("{}/badge-icons/{hash}.png", cdn()),
    ))
}

pub fn connection_label(kind: &str) -> String {
    match kind {
        "github" => "GitHub".to_owned(),
        "twitter" | "x" => "X".to_owned(),
        "twitch" => "Twitch".to_owned(),
        "youtube" => "YouTube".to_owned(),
        "spotify" => "Spotify".to_owned(),
        "steam" => "Steam".to_owned(),
        "reddit" => "Reddit".to_owned(),
        "bluesky" => "Bluesky".to_owned(),
        "tiktok" => "TikTok".to_owned(),
        "instagram" => "Instagram".to_owned(),
        "facebook" => "Facebook".to_owned(),
        "domain" => "Website".to_owned(),
        "xbox" => "Xbox".to_owned(),
        "playstation" => "PlayStation".to_owned(),
        "ebay" => "eBay".to_owned(),
        "paypal" => "PayPal".to_owned(),
        "epicgames" => "Epic Games".to_owned(),
        "battlenet" => "Battle.net".to_owned(),
        "riotgames" => "Riot Games".to_owned(),
        "leagueoflegends" => "League of Legends".to_owned(),
        "crunchyroll" => "Crunchyroll".to_owned(),
        "roblox" => "Roblox".to_owned(),
        "bungie" => "Bungie".to_owned(),
        "skype" => "Skype".to_owned(),
        other => {
            let mut chars = other.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
                None => String::new(),
            }
        }
    }
}

/// One path segment of a connection URL. Empty, traversal, or a character that would
/// change the path or the query is rejected.
fn path_token(raw: &str) -> Option<&str> {
    let token = raw.trim().trim_start_matches('@');
    if token.is_empty()
        || token.contains('/')
        || token.contains('\\')
        || token.contains('?')
        || token.contains('#')
        || token.contains('@')
        || token.contains("..")
        || token.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return None;
    }
    Some(token)
}

/// A bare hostname for a domain connection: no scheme, user, port, path, or traversal.
fn bare_host(raw: &str) -> Option<&str> {
    let host = raw.trim();
    if host.is_empty()
        || !host.contains('.')
        || host.contains("://")
        || host.contains('@')
        || host.contains(':')
        || host.contains('/')
        || host.contains('\\')
        || host.contains('?')
        || host.contains('#')
        || host.contains("..")
        || host.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return None;
    }
    Some(host)
}

/// Keeps `url` only when it is an https address with no userinfo.
fn https_link(url: String) -> String {
    match protocol::link::Destination::parse(&url) {
        Some(dest) if dest.url.starts_with("https://") && !dest.has_userinfo => url,
        _ => String::new(),
    }
}

pub fn connection_url(kind: &str, id: &str, name: &str) -> String {
    let clean = name.trim();
    let handle = clean.trim_start_matches('@');
    let url = match kind {
        "github" => path_token(clean).map(|token| format!("https://github.com/{token}")),
        "twitter" | "x" => path_token(handle).map(|token| format!("https://x.com/{token}")),
        "twitch" => path_token(clean).map(|token| format!("https://twitch.tv/{token}")),
        "youtube" => path_token(id).map(|token| format!("https://www.youtube.com/channel/{token}")),
        "spotify" => path_token(id).map(|token| format!("https://open.spotify.com/user/{token}")),
        "steam" => path_token(id).map(|token| format!("https://steamcommunity.com/profiles/{token}")),
        "reddit" => path_token(clean).map(|token| format!("https://www.reddit.com/u/{token}")),
        "bluesky" => path_token(handle).map(|token| format!("https://bsky.app/profile/{token}")),
        "tiktok" => path_token(handle).map(|token| format!("https://www.tiktok.com/@{token}")),
        "instagram" => path_token(handle).map(|token| format!("https://www.instagram.com/{token}")),
        "facebook" => path_token(id).map(|token| format!("https://www.facebook.com/{token}")),
        "domain" => bare_host(clean).map(|host| format!("https://{host}")),
        "ebay" => path_token(clean).map(|token| format!("https://www.ebay.com/usr/{token}")),
        "roblox" => path_token(id).map(|token| format!("https://www.roblox.com/users/{token}/profile")),
        _ => None,
    };
    url.map(https_link).unwrap_or_default()
}

pub fn guild_icon(guild: &str, hash: &str) -> Option<(MediaFile, String)> {
    if !cdn_token(hash) {
        return None;
    }
    Some((
        sized(format!("icon:{guild}:{hash}"), MediaKind::Icon, 96),
        format!("{}/icons/{guild}/{hash}.webp?size=96{}", cdn(), moving(hash)),
    ))
}

pub fn role_icon(role: &str, hash: &str) -> (MediaFile, String) {
    (
        sized(format!("roleicon:{role}:{hash}"), MediaKind::Icon, 48),
        format!("{}/role-icons/{role}/{hash}.webp?size=48", cdn()),
    )
}

/// A server's banner image, served from Discord's CDN at 512px wide.
pub fn guild_banner(guild: &str, hash: &str) -> (MediaFile, String) {
    (
        MediaFile {
            width: 512,
            height: 288,
            ..MediaFile::new(format!("banner:{guild}:{hash}"), MediaKind::Image)
        },
        format!("{}/banners/{guild}/{hash}.webp?size=512{}", cdn(), moving(hash)),
    )
}

/// Whether a server's `features` list marks it as verified or partnered.
pub fn guild_mark(features: &[String]) -> Option<protocol::Mark> {
    features
        .iter()
        .any(|f| f == "VERIFIED" || f == "PARTNERED")
        .then_some(protocol::Mark::Verified)
}

/// A group direct message's own picture. The address is from the community documentation and
/// unconfirmed against a real account.
pub fn group_icon(channel: &str, hash: &str) -> Option<(MediaFile, String)> {
    if !cdn_token(hash) {
        return None;
    }
    Some((
        sized(format!("gicon:{channel}:{hash}"), MediaKind::Avatar, 80),
        format!("{}/channel-icons/{channel}/{hash}.webp?size=80{}", cdn(), moving(hash)),
    ))
}

/// A custom emoji at 48 px; an animated one as an animated WebP of that size, not its GIF.
pub fn emoji(id: &str, animated: bool) -> (MediaFile, String) {
    (
        sized(format!("emoji:{id}"), MediaKind::Emoji, 48),
        format!("{}/emojis/{id}.webp?size=48{}", cdn(), if animated { "&animated=true" } else { "" }),
    )
}

#[derive(Deserialize, Default, Clone, Debug)]
pub struct Member {
    #[serde(default)]
    pub user: Option<User>,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub nick: Option<String>,
    #[serde(default)]
    pub avatar: Option<String>,
    #[serde(default)]
    pub banner: Option<String>,
    #[serde(default)]
    pub roles: Vec<String>,
    #[serde(default)]
    pub presence: Option<PresenceUpdate>,
    #[serde(default)]
    pub communication_disabled_until: Option<String>,
    /// Rules not accepted yet. Not a permission.
    #[serde(default, deserialize_with = "nullable")]
    pub pending: bool,
    /// Membership flags. Not a permission grant.
    #[serde(default, deserialize_with = "lenient_u64")]
    pub flags: u64,
}

impl Member {
    pub fn id(&self) -> &str {
        match (&self.user, &self.user_id) {
            (Some(user), _) => &user.id,
            (None, Some(id)) => id,
            (None, None) => "",
        }
    }

    pub fn name(&self) -> &str {
        match (self.nick.as_deref(), &self.user) {
            (Some(nick), _) if !nick.is_empty() => nick,
            (_, Some(user)) => user.name(),
            _ => "",
        }
    }

    /// Server avatar when set on this guild member, otherwise the user's global avatar.
    pub fn avatar(&self, guild: Option<&str>) -> Option<(MediaFile, String)> {
        if let (Some(guild), Some(hash)) = (guild, self.avatar.as_deref()) {
            let id = self.id();
            if !id.is_empty() {
                if let Some(picture) = member_avatar(guild, id, hash) {
                    return Some(picture);
                }
            }
        }
        self.user.as_ref().map(User::avatar)
    }
}

#[derive(Deserialize, Clone, Debug)]
pub struct Overwrite {
    pub id: String,
    #[serde(rename = "type", deserialize_with = "overwrite_kind")]
    pub member: bool,
    #[serde(default, deserialize_with = "lenient_u64")]
    pub allow: u64,
    #[serde(default, deserialize_with = "lenient_u64")]
    pub deny: u64,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct ThreadMetadata {
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub locked: bool,
    #[serde(default)]
    pub auto_archive_duration: u32,
    #[serde(default)]
    pub archive_timestamp: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default)]
pub struct ForumTag {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub moderated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emoji_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emoji_name: Option<String>,
}

/// Thread-member notification flags (`ThreadMember::flags`).
pub const THREAD_NOTIFY_ALL: u32 = 1 << 1;
pub const THREAD_NOTIFY_MENTIONS: u32 = 1 << 2;
pub const THREAD_NOTIFY_NONE: u32 = 1 << 3;
pub const THREAD_NOTIFY_MASK: u32 = THREAD_NOTIFY_ALL | THREAD_NOTIFY_MENTIONS | THREAD_NOTIFY_NONE;

/// Our own membership of a thread: holds its per-thread notification flags and mute state.
#[derive(Deserialize, Clone, Debug, Default)]
pub struct ThreadMember {
    #[serde(default, deserialize_with = "nullable")]
    pub flags: u32,
    #[serde(default, deserialize_with = "nullable")]
    pub muted: bool,
}

impl ThreadMember {
    /// This thread's explicit notification setting, or `None` when it inherits its parent.
    pub fn notify(&self) -> Option<protocol::Notify> {
        if self.muted || (self.flags & THREAD_NOTIFY_NONE != 0) {
            Some(protocol::Notify::Nothing)
        } else if self.flags & THREAD_NOTIFY_MENTIONS != 0 {
            Some(protocol::Notify::Mentions)
        } else if self.flags & THREAD_NOTIFY_ALL != 0 {
            Some(protocol::Notify::All)
        } else {
            None
        }
    }
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct Channel {
    pub id: String,
    #[serde(rename = "type", default)]
    pub kind: u8,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub position: i32,
    /// The most who may be in it at once (a voice channel), 0 when it has no limit.
    #[serde(default, deserialize_with = "nullable")]
    pub user_limit: u32,
    /// A voice channel's encode rate, in bits per second. 0 when the channel did not say.
    #[serde(default, deserialize_with = "nullable")]
    pub bitrate: u32,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub guild_id: Option<String>,
    #[serde(default)]
    pub last_message_id: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub permission_overwrites: Vec<Overwrite>,
    /// Whole users, or only their ids when the session asked for deduplicated users.
    #[serde(default)]
    pub recipients: Vec<User>,
    #[serde(default)]
    pub recipient_ids: Vec<String>,
    /// Who owns a group DM or forum post thread.
    #[serde(default)]
    pub owner_id: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub topic: Option<String>,
    /// Slow mode interval in seconds (0 = off).
    #[serde(default, deserialize_with = "nullable")]
    pub rate_limit_per_user: u32,
    /// Age-restricted channel flag.
    #[serde(default, deserialize_with = "nullable")]
    pub nsfw: bool,
    #[serde(default)]
    pub thread_metadata: Option<ThreadMetadata>,
    #[serde(default)]
    pub message_count: Option<u32>,
    #[serde(default)]
    pub total_message_sent: Option<u32>,
    /// Channel flags (e.g. `THREAD_PINNED`, `REQUIRE_TAG`).
    #[serde(default, deserialize_with = "nullable")]
    pub flags: u32,
    /// Tags defined on a forum or media channel.
    #[serde(default, deserialize_with = "nullable")]
    pub available_tags: Vec<ForumTag>,
    /// Tag ids applied to a forum post thread.
    #[serde(default, deserialize_with = "nullable")]
    pub applied_tags: Vec<String>,
    /// Default slowmode per newly created thread in a forum channel (seconds).
    #[serde(default, deserialize_with = "nullable")]
    pub default_thread_rate_limit_per_user: u32,
    /// Our own membership of a thread, present when we are in it.
    #[serde(default)]
    pub member: Option<ThreadMember>,
    /// A DM from someone who is not a friend, waiting behind Message Requests, and one Discord
    /// judged spam.
    #[serde(default, deserialize_with = "nullable")]
    pub is_message_request: bool,
    #[serde(default, deserialize_with = "nullable")]
    pub is_spam: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// What is kept of a server's channel between connects, in a note: enough to work out again
/// who may see it when a role or our own membership changes. Short names, because a large
/// server has hundreds.
#[derive(Deserialize, Serialize)]
pub struct Kept {
    #[serde(rename = "i")]
    pub id: String,
    #[serde(rename = "k")]
    pub kind: u8,
    #[serde(rename = "n", default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(rename = "p", default)]
    pub position: i32,
    /// A voice channel's occupant limit, 0 when it has none.
    #[serde(rename = "l", default, skip_serializing_if = "no_limit")]
    pub limit: u32,
    /// A voice channel's encode rate, in bits per second. 0 when it was not stored.
    #[serde(rename = "b", default, skip_serializing_if = "no_limit")]
    pub bitrate: u32,
    #[serde(rename = "u", default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(rename = "t", default, skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
    #[serde(rename = "o", default, skip_serializing_if = "Vec::is_empty")]
    pub overwrites: Vec<(String, bool, u64, u64)>,
    /// Slowmode interval in seconds, 0 when off.
    #[serde(rename = "s", default, skip_serializing_if = "no_limit")]
    pub slowmode: u32,
    #[serde(rename = "x", default, skip_serializing_if = "is_false")]
    pub nsfw: bool,
    #[serde(rename = "f", default, skip_serializing_if = "no_limit")]
    pub flags: u32,
    #[serde(rename = "g", default, skip_serializing_if = "Vec::is_empty")]
    pub available_tags: Vec<ForumTag>,
    #[serde(rename = "r", default, skip_serializing_if = "no_limit")]
    pub default_thread_rate_limit_per_user: u32,
}

/// A count that means "none" stays off a note.
fn no_limit(limit: &u32) -> bool {
    *limit == 0
}

impl Kept {
    pub fn of(channel: &Channel) -> Kept {
        let overwrites = channel.permission_overwrites.iter();
        Kept {
            id: channel.id.clone(),
            kind: channel.kind,
            name: channel.name.clone(),
            position: channel.position,
            limit: channel.user_limit,
            bitrate: channel.bitrate,
            parent: channel.parent_id.clone(),
            topic: channel.topic.clone().filter(|topic| !topic.is_empty()),
            overwrites: overwrites.map(|over| (over.id.clone(), over.member, over.allow, over.deny)).collect(),
            slowmode: channel.rate_limit_per_user,
            nsfw: channel.nsfw,
            flags: channel.flags,
            available_tags: channel.available_tags.clone(),
            default_thread_rate_limit_per_user: channel.default_thread_rate_limit_per_user,
        }
    }

    pub fn channel(self) -> Channel {
        let overwrites = self.overwrites.into_iter().map(|(id, member, allow, deny)| Overwrite { id, member, allow, deny });
        Channel {
            id: self.id,
            kind: self.kind,
            name: self.name,
            position: self.position,
            user_limit: self.limit,
            bitrate: self.bitrate,
            parent_id: self.parent,
            topic: self.topic,
            permission_overwrites: overwrites.collect(),
            rate_limit_per_user: self.slowmode,
            nsfw: self.nsfw,
            flags: self.flags,
            available_tags: self.available_tags,
            default_thread_rate_limit_per_user: self.default_thread_rate_limit_per_user,
            ..Channel::default()
        }
    }
}

impl Channel {
    pub fn is_thread(&self) -> bool {
        matches!(self.kind, NEWS_THREAD | PUBLIC_THREAD | PRIVATE_THREAD)
    }

    pub fn is_voice(&self) -> bool {
        matches!(self.kind, VOICE | STAGE)
    }
}

#[derive(Deserialize, Clone, Debug)]
pub struct Role {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default, deserialize_with = "lenient_u64")]
    pub permissions: u64,
    #[serde(default, deserialize_with = "nullable")]
    pub position: i32,
    #[serde(default, deserialize_with = "nullable")]
    pub hoist: bool,
    /// `0xRRGGBB`. Missing and null are no colour.
    #[serde(default, deserialize_with = "color_u32")]
    pub color: u32,
    #[serde(default)]
    pub mentionable: bool,
    #[serde(default)]
    pub managed: bool,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub unicode_emoji: Option<String>,
}

fn color_u32<'de, D: Deserializer<'de>>(de: D) -> Result<u32, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Either {
        Number(u64),
        Text(String),
    }
    let value = match Option::<Either>::deserialize(de)? {
        Some(Either::Number(number)) => number,
        Some(Either::Text(text)) => text.parse().unwrap_or(0),
        None => 0,
    };
    Ok((value & 0x00FF_FFFF) as u32)
}

/// A role-icon hash Discord would store. A URL, a slash, or anything outside the hash alphabet is dropped.
pub fn role_icon_hash(raw: Option<&str>) -> Option<String> {
    let hash = raw?.trim();
    if hash.is_empty()
        || hash.contains("://")
        || hash.contains('/')
        || hash.contains('\\')
        || hash.contains('?')
        || hash.contains('#')
        || hash.contains('@')
        || hash.contains('%')
        || hash.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return None;
    }
    let body = hash.strip_prefix("a_").unwrap_or(hash);
    if body.is_empty() || body.len() > 64 || !body.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(hash.to_owned())
}

/// A unicode emoji for a role. A URL, a slash, or a sentence is dropped.
pub fn role_emoji(raw: Option<&str>) -> Option<String> {
    let emoji = raw?.trim();
    if emoji.is_empty()
        || emoji.contains("://")
        || emoji.contains('/')
        || emoji.contains('\\')
        || emoji.chars().any(|c| c.is_whitespace() || c.is_control())
        || emoji.chars().count() > 32
    {
        return None;
    }
    Some(emoji.to_owned())
}

/// `https` anywhere, or `http` only for a loopback host (the test CDN). Userinfo and junk are refused.
pub fn cdn_base_ok(base: &str) -> bool {
    if base.is_empty() || base.contains('@') || base.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return false;
    }
    if let Some(rest) = base.strip_prefix("https://") {
        let host = rest.split('/').next().unwrap_or("");
        return !host.is_empty();
    }
    if let Some(rest) = base.strip_prefix("http://") {
        let authority = rest.split('/').next().unwrap_or("");
        let host = if let Some(inner) = authority.strip_prefix('[') {
            inner.split(']').next().unwrap_or("")
        } else {
            authority.split(':').next().unwrap_or("")
        };
        return matches!(host, "127.0.0.1" | "localhost" | "::1");
    }
    false
}

/// The picture URL for a role icon. Built only from a numeric role id, a hash, and the configured CDN.
pub fn role_icon_url(role_id: &str, hash: &str) -> Option<String> {
    let hash = role_icon_hash(Some(hash))?;
    if role_id.is_empty()
        || role_id.len() > 20
        || !role_id.bytes().all(|byte| byte.is_ascii_digit())
        || number(role_id) == 0
    {
        return None;
    }
    let base = cdn().trim_end_matches('/');
    if !cdn_base_ok(base) {
        return None;
    }
    Some(format!("{base}/role-icons/{role_id}/{hash}.png"))
}

/// What newer sessions wrap a guild's own fields in.
#[derive(Deserialize, Clone, Debug, Default)]
pub struct GuildProperties {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub banner: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub owner_id: Option<String>,
    #[serde(default)]
    pub premium_tier: Option<u8>,
    #[serde(default)]
    pub premium_subscription_count: Option<u32>,
    #[serde(default)]
    pub features: Option<Vec<String>>,
    #[serde(default)]
    pub vanity_url_code: Option<String>,
    #[serde(default)]
    pub system_channel_id: Option<String>,
    #[serde(default)]
    pub afk_channel_id: Option<String>,
    #[serde(default)]
    pub afk_timeout: Option<u32>,
    #[serde(default)]
    pub default_message_notifications: Option<u8>,
    pub verification_level: Option<u8>,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct Guild {
    pub id: String,
    #[serde(default)]
    pub properties: Option<GuildProperties>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub banner: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub owner_id: Option<String>,
    #[serde(default)]
    pub premium_tier: Option<u8>,
    #[serde(default)]
    pub premium_subscription_count: Option<u32>,
    #[serde(default)]
    pub features: Option<Vec<String>>,
    #[serde(default)]
    pub vanity_url_code: Option<String>,
    #[serde(default)]
    pub system_channel_id: Option<String>,
    #[serde(default)]
    pub afk_channel_id: Option<String>,
    #[serde(default)]
    pub afk_timeout: Option<u32>,
    #[serde(default)]
    pub default_message_notifications: Option<u8>,
    pub verification_level: Option<u8>,
    #[serde(default)]
    pub channels: Vec<Channel>,
    #[serde(default)]
    pub threads: Vec<Channel>,
    #[serde(default)]
    pub roles: Vec<Role>,
    #[serde(default)]
    pub joined_at: Option<String>,
    #[serde(default)]
    pub members: Vec<Member>,
    #[serde(default)]
    pub voice_states: Vec<VoiceState>,
    #[serde(default)]
    pub unavailable: Option<bool>,
    /// The server's own emoji and stickers, top-level even when the rest is in `properties`.
    #[serde(default, deserialize_with = "nullable")]
    pub emojis: Vec<GuildEmoji>,
    #[serde(default, deserialize_with = "nullable")]
    pub stickers: Vec<GuildSticker>,
    #[serde(default, deserialize_with = "nullable")]
    pub guild_scheduled_events: Vec<ScheduledEvent>,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct EventEntityMetadata {
    #[serde(default)]
    pub location: Option<String>,
}

/// One scheduled event in a Discord server (`guild_scheduled_events` in READY, gateway events,
/// and `GET /guilds/{id}/scheduled-events`).
#[derive(Deserialize, Clone, Debug, Default)]
pub struct ScheduledEvent {
    pub id: String,
    #[serde(default)]
    pub guild_id: String,
    #[serde(default)]
    pub channel_id: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub scheduled_start_time: Option<String>,
    #[serde(default)]
    pub scheduled_end_time: Option<String>,
    /// 1 = scheduled, 2 = active, 3 = completed, 4 = canceled.
    #[serde(default, deserialize_with = "nullable")]
    pub status: u8,
    /// 1 = stage, 2 = voice, 3 = external.
    #[serde(default, deserialize_with = "nullable")]
    pub entity_type: u8,
    #[serde(default)]
    pub entity_metadata: Option<EventEntityMetadata>,
    #[serde(default)]
    pub user_count: Option<u32>,
    #[serde(default)]
    pub image: Option<String>,
}

/// Payload for `GUILD_SCHEDULED_EVENT_USER_ADD` and `GUILD_SCHEDULED_EVENT_USER_REMOVE`.
#[derive(Deserialize, Clone, Debug, Default)]
pub struct ScheduledEventUser {
    pub guild_scheduled_event_id: String,
    pub user_id: String,
    pub guild_id: String,
}

/// Cover image for a Discord scheduled event (`guild-events/{id}/{hash}.webp?size=512`).
pub fn event_cover(event_id: &str, hash: &str) -> (MediaFile, String) {
    let url = format!("{}/guild-events/{event_id}/{hash}.webp?size=512", cdn());
    (MediaFile::new(format!("event_cover:{event_id}:{hash}"), MediaKind::Image), url)
}

#[derive(Deserialize, Clone, Debug)]
pub struct GuildEmoji {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub name: String,
    #[serde(default, deserialize_with = "nullable")]
    pub animated: bool,
    /// False once the server lost the boosts that paid for it.
    #[serde(default = "yes", deserialize_with = "nullable_yes")]
    pub available: bool,
    /// The roles allowed to use it; empty for everyone.
    #[serde(default, deserialize_with = "nullable")]
    pub roles: Vec<String>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct GuildSticker {
    pub id: String,
    #[serde(default, deserialize_with = "nullable")]
    pub name: String,
    /// 1 PNG, 2 APNG, 3 Lottie, 4 GIF.
    #[serde(default, deserialize_with = "nullable")]
    pub format_type: u8,
    #[serde(default = "yes", deserialize_with = "nullable_yes")]
    pub available: bool,
}

fn yes() -> bool {
    true
}

/// A flag that is on unless it says otherwise.
fn nullable_yes<'de, D: Deserializer<'de>>(de: D) -> Result<bool, D::Error> {
    Ok(Option::<bool>::deserialize(de)?.unwrap_or(true))
}

/// A sticker's picture from the media proxy, registered as a message shows it (`content.rs`), so
/// the picker and a sent sticker share one file. Lottie JSON is rendered by the shell.
pub fn sticker(id: &str, format: u8) -> Option<(MediaFile, String)> {
    let (kind, url) = match format {
        1 => (MediaKind::Image, format!("{}/stickers/{id}.png?size=160", media())),
        2 => (MediaKind::Image, apng_sticker(id)),
        3 => (MediaKind::File, format!("{}/stickers/{id}.json", cdn())),
        4 => (MediaKind::Gif, format!("{}/stickers/{id}.gif", media())),
        _ => return None,
    };
    Some((sized(format!("sticker:{id}"), kind, 160), url))
}

/// An APNG sticker, frames and all: the proxy flattens one to a still unless asked to pass it
/// through. The parameter is the web client's, unconfirmed against a real account.
pub fn apng_sticker(id: &str) -> String {
    format!("{}/stickers/{id}.png?size=160&passthrough=true", media())
}

impl Guild {
    pub fn name(&self) -> &str {
        let wrapped = self.properties.as_ref().and_then(|properties| properties.name.as_deref());
        wrapped.or(self.name.as_deref()).unwrap_or("Server")
    }

    pub fn icon(&self) -> Option<&str> {
        let wrapped = self.properties.as_ref().and_then(|properties| properties.icon.as_deref());
        wrapped.or(self.icon.as_deref())
    }

    pub fn owner(&self) -> &str {
        let wrapped = self.properties.as_ref().and_then(|properties| properties.owner_id.as_deref());
        wrapped.or(self.owner_id.as_deref()).unwrap_or("")
    }

    pub fn premium_tier(&self) -> Option<u8> {
        let wrapped = self.properties.as_ref().and_then(|properties| properties.premium_tier);
        wrapped.or(self.premium_tier)
    }

    pub fn banner(&self) -> Option<&str> {
        let wrapped = self.properties.as_ref().and_then(|properties| properties.banner.as_deref());
        wrapped.or(self.banner.as_deref()).filter(|h| !h.is_empty())
    }

    pub fn description(&self) -> Option<&str> {
        let wrapped = self.properties.as_ref().and_then(|properties| properties.description.as_deref());
        wrapped.or(self.description.as_deref())
    }

    pub fn premium_subscription_count(&self) -> Option<u32> {
        let wrapped = self.properties.as_ref().and_then(|properties| properties.premium_subscription_count);
        wrapped.or(self.premium_subscription_count)
    }

    pub fn features(&self) -> &[String] {
        let wrapped = self.properties.as_ref().and_then(|properties| properties.features.as_deref());
        wrapped.or(self.features.as_deref()).unwrap_or(&[])
    }

    pub fn vanity_url_code(&self) -> Option<&str> {
        let wrapped = self.properties.as_ref().and_then(|properties| properties.vanity_url_code.as_deref());
        wrapped.or(self.vanity_url_code.as_deref()).filter(|c| !c.is_empty())
    }

    pub fn system_channel_id(&self) -> Option<&str> {
        let wrapped = self.properties.as_ref().and_then(|properties| properties.system_channel_id.as_deref());
        wrapped.or(self.system_channel_id.as_deref()).filter(|id| !id.is_empty())
    }

    pub fn afk_channel_id(&self) -> Option<&str> {
        let wrapped = self.properties.as_ref().and_then(|properties| properties.afk_channel_id.as_deref());
        wrapped.or(self.afk_channel_id.as_deref()).filter(|id| !id.is_empty())
    }

    pub fn afk_timeout(&self) -> Option<u32> {
        let wrapped = self.properties.as_ref().and_then(|properties| properties.afk_timeout);
        wrapped.or(self.afk_timeout)
    }

    pub fn default_message_notifications(&self) -> Option<u8> {
        let wrapped = self.properties.as_ref().and_then(|properties| properties.default_message_notifications);
        wrapped.or(self.default_message_notifications)
    }
    /// Missing on a partial guild update, so a caller does not treat that as level 0.
    pub fn verification_level(&self) -> Option<u8> {
        let wrapped = self.properties.as_ref().and_then(|properties| properties.verification_level);
        wrapped.or(self.verification_level)
    }

    /// The feature list when this payload carries one. An omitted list is not an empty one.
    pub fn listed_features(&self) -> Option<&[String]> {
        if let Some(features) = self.properties.as_ref().and_then(|properties| properties.features.as_deref()) {
            return Some(features);
        }
        self.features.as_deref().filter(|features| !features.is_empty())
    }
}

#[derive(Deserialize, Clone, Debug)]
pub struct Relationship {
    #[serde(default)]
    pub id: String,
    #[serde(rename = "type", default)]
    pub kind: u8,
    #[serde(default)]
    pub nickname: Option<String>,
    #[serde(default)]
    pub user: Option<User>,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub user_ignored: bool,
}

impl Relationship {
    pub fn person(&self) -> &str {
        match (&self.user, &self.user_id) {
            (Some(user), _) => &user.id,
            (None, Some(id)) => id,
            (None, None) => &self.id,
        }
    }

    pub fn relation(&self) -> protocol::Relation {
        match self.kind {
            1 => protocol::Relation::Friend,
            2 => protocol::Relation::Blocked,
            3 => protocol::Relation::Incoming,
            4 => protocol::Relation::Outgoing,
            _ if self.user_ignored => protocol::Relation::Ignored,
            _ => protocol::Relation::None,
        }
    }
}

#[derive(Deserialize, Clone, Debug)]
pub struct ReadState {
    pub id: String,
    #[serde(default, deserialize_with = "nullable")]
    pub mention_count: u32,
    /// A string, or the number 0 for "never".
    #[serde(default, deserialize_with = "lenient_u64")]
    pub last_message_id: u64,
    /// 0 is a channel; the others (events, onboarding, the inbox) are not conversations.
    #[serde(default, deserialize_with = "nullable")]
    pub read_state_type: u8,
}

/// A list on older sessions, wrapped with a version on newer ones.
#[derive(Deserialize)]
#[serde(untagged)]
pub enum Versioned<T> {
    List(Vec<T>),
    Wrapped { entries: Vec<T> },
}

impl<T> Versioned<T> {
    pub fn into_entries(self) -> Vec<T> {
        match self {
            Versioned::List(entries) | Versioned::Wrapped { entries } => entries,
        }
    }
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct MuteConfig {
    /// ISO 8601, or null for "until I turn it back on".
    #[serde(default)]
    pub end_time: Option<String>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct ChannelOverride {
    pub channel_id: String,
    #[serde(default, deserialize_with = "nullable")]
    pub muted: bool,
    #[serde(default)]
    pub mute_config: Option<MuteConfig>,
    /// 0 all, 1 mentions, 2 nothing, 3 as the server says.
    #[serde(default = "inherit", deserialize_with = "nullable_inherit")]
    pub message_notifications: u8,
    /// `OPT_IN_ENABLED` and the like.
    #[serde(default, deserialize_with = "nullable")]
    pub flags: u32,
    /// Category collapse state synced with Discord.
    #[serde(default)]
    pub collapsed: Option<bool>,
}

fn inherit() -> u8 {
    3
}

fn nullable_inherit<'de, D: Deserializer<'de>>(de: D) -> Result<u8, D::Error> {
    Ok(Option::<u8>::deserialize(de)?.unwrap_or(3))
}

/// Guild settings flags: the user turned "Show all channels" off, so only opted-in ones show.
pub const OPT_IN_CHANNELS_ON: u32 = 1 << 14;
/// Channel override flags: this channel (or category) is opted in.
pub const OPT_IN_ENABLED: u32 = 1 << 12;

/// Server onboarding configuration (`GET /guilds/{id}/onboarding`).
#[derive(Deserialize, Clone, Debug, Default)]
pub struct Onboarding {
    #[serde(default, deserialize_with = "nullable")]
    pub default_channel_ids: Vec<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub enabled: bool,
}

#[derive(Deserialize, Clone, Debug)]
pub struct GuildSettings {
    /// Null for the entry that holds direct messages.
    #[serde(default)]
    pub guild_id: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub muted: bool,
    #[serde(default)]
    pub mute_config: Option<MuteConfig>,
    #[serde(default, deserialize_with = "nullable")]
    pub suppress_everyone: bool,
    #[serde(default, deserialize_with = "nullable")]
    pub suppress_roles: bool,
    /// 0 default, 1 disabled (suppressed), 2 enabled.
    #[serde(default, deserialize_with = "nullable")]
    pub notify_highlights: u8,
    #[serde(default, deserialize_with = "nullable")]
    pub mute_scheduled_events: bool,
    #[serde(default = "yes", deserialize_with = "nullable_yes")]
    pub mobile_push: bool,
    #[serde(default, deserialize_with = "nullable")]
    pub channel_overrides: Vec<ChannelOverride>,
    #[serde(default, deserialize_with = "nullable")]
    pub hide_muted_channels: bool,
    #[serde(default, deserialize_with = "nullable")]
    pub flags: u32,
    /// 0 all, 1 mentions, 2 nothing.
    #[serde(default, deserialize_with = "nullable")]
    pub message_notifications: u8,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct ActivityTimestamps {
    #[serde(default)]
    pub start: Option<i64>,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct ActivityAssets {
    #[serde(default)]
    pub large_image: Option<String>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Activity {
    #[serde(rename = "type", default)]
    pub kind: u8,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub details: Option<String>,
    /// The game, the service, the show.
    #[serde(default, deserialize_with = "nullable")]
    pub name: String,
    /// A custom status's emoji: a Unicode one has no id.
    #[serde(default)]
    pub emoji: Option<Emoji>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub application_id: Option<String>,
    #[serde(default)]
    pub timestamps: Option<ActivityTimestamps>,
    #[serde(default)]
    pub assets: Option<ActivityAssets>,
}

fn valid_https_url(url: &str) -> Option<String> {
    let trimmed = url.trim();
    let ok = trimmed.get(..8).is_some_and(|s| s.eq_ignore_ascii_case("https://"));
    if ok && trimmed.len() <= 512 && !trimmed.chars().any(|c| c.is_whitespace() || c.is_control()) {
        Some(trimmed.to_owned())
    } else {
        None
    }
}

fn activity_art(application_id: Option<&str>, large_image: Option<&str>) -> Option<(MediaFile, String)> {
    let asset = large_image?.trim();
    if asset.is_empty() {
        return None;
    }
    if let Some(spotify_id) = asset.strip_prefix("spotify:") {
        if !spotify_id.is_empty() && spotify_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
            return Some((
                sized(format!("actart:spotify:{spotify_id}"), MediaKind::Image, 64),
                format!("https://i.scdn.co/image/{spotify_id}"),
            ));
        }
        return None;
    }
    let app = application_id?.trim();
    if !app.is_empty()
        && app.bytes().all(|b| b.is_ascii_digit())
        && asset.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Some((
            sized(format!("actart:{app}:{asset}"), MediaKind::Image, 64),
            format!("{}/app-assets/{app}/{asset}.png?size=64", cdn()),
        ));
    }
    None
}

#[derive(Deserialize, Clone, Debug)]
pub struct PresenceUser {
    pub id: String,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct PresenceUpdate {
    #[serde(default)]
    pub user: Option<PresenceUser>,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub status: String,
    #[serde(default, deserialize_with = "nullable")]
    pub activities: Vec<Activity>,
}

impl PresenceUpdate {
    pub fn id(&self) -> &str {
        match (&self.user, &self.user_id) {
            (Some(user), _) => &user.id,
            (None, Some(id)) => id,
            (None, None) => "",
        }
    }

    pub fn presence(&self) -> Presence {
        match self.status.as_str() {
            "online" => Presence::Online,
            "idle" => Presence::Idle,
            "dnd" => Presence::Dnd,
            "offline" | "invisible" => Presence::Offline,
            _ => Presence::Unknown,
        }
    }

    /// The custom status line (activity type 4).
    pub fn status(&self) -> &str {
        let custom = self.activities.iter().find(|activity| activity.kind == 4);
        custom.and_then(|activity| activity.state.as_deref()).unwrap_or("")
    }

    /// The custom status's emoji when it is a Unicode one.
    pub fn emoji(&self) -> &str {
        let custom = self.activities.iter().find(|activity| activity.kind == 4);
        let emoji = custom.and_then(|activity| activity.emoji.as_ref()).filter(|emoji| emoji.id.is_none());
        emoji.and_then(|emoji| emoji.name.as_deref()).unwrap_or("")
    }

    /// The custom status's custom emoji image, when it has an `id`.
    pub fn emoji_file(&self) -> Option<(MediaFile, String)> {
        let custom = self.activities.iter().find(|activity| activity.kind == 4)?;
        let e = custom.emoji.as_ref()?;
        let id = e.id.as_deref().filter(|id| !id.is_empty())?;
        Some(emoji(id, e.animated))
    }

    /// What they are doing, as the first activity that is not the custom status says,
    /// along with its album art / large-image media file when available.
    pub fn doing_with_art(&self) -> Option<(protocol::Activity, Option<(MediaFile, String)>)> {
        use protocol::ActivityKind::*;
        self.activities.iter().find_map(|activity| {
            let kind = match activity.kind {
                0 => Playing,
                1 => Streaming,
                2 => Listening,
                3 => Watching,
                5 => Competing,
                _ => return None,
            };
            let started = activity
                .timestamps
                .as_ref()
                .and_then(|t| t.start)
                .filter(|&s| s > 0)
                .map(|s| if s > 10_000_000_000 { s / 1000 } else { s });
            let url = if kind == Streaming {
                activity.url.as_deref().and_then(valid_https_url).unwrap_or_default()
            } else {
                String::new()
            };
            let art = activity_art(
                activity.application_id.as_deref(),
                activity.assets.as_ref().and_then(|a| a.large_image.as_deref()),
            );
            let mut doing = protocol::Activity {
                details: activity.details.clone().unwrap_or_default(),
                state: activity.state.clone().unwrap_or_default(),
                started,
                url,
                art: art.as_ref().map(|(file, _)| file.clone()),
                ..protocol::Activity::new(kind, activity.name.clone())
            };
            doing.bound();
            Some((doing, art))
        })
    }

    /// What they are doing, as the first activity that is not the custom status says.
    pub fn doing(&self) -> Option<protocol::Activity> {
        self.doing_with_art().map(|(doing, _)| doing)
    }
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct VoiceState {
    #[serde(default)]
    pub guild_id: Option<String>,
    #[serde(default)]
    pub channel_id: Option<String>,
    #[serde(default)]
    pub user_id: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub self_mute: bool,
    #[serde(default, deserialize_with = "nullable")]
    pub self_deaf: bool,
    #[serde(default, deserialize_with = "nullable")]
    pub mute: bool,
    #[serde(default, deserialize_with = "nullable")]
    pub deaf: bool,
    /// An audience seat on a stage. In a voice channel the word means Discord's server mute,
    /// so it is only read for stages.
    #[serde(default, deserialize_with = "nullable")]
    pub suppress: bool,
    #[serde(default, deserialize_with = "nullable")]
    pub self_stream: bool,
    #[serde(default, deserialize_with = "nullable")]
    pub self_video: bool,
    #[serde(default)]
    pub member: Option<Member>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Emoji {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub animated: bool,
}

impl Emoji {
    /// How the protocol spells it, and its picture when it is a custom one.
    pub fn spelled(&self) -> (String, Option<(MediaFile, String)>) {
        let name = self.name.as_deref().unwrap_or("emoji");
        match &self.id {
            Some(id) => (format!(":{name}:"), Some(emoji(id, self.animated))),
            None => (name.to_owned(), None),
        }
    }
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct Attachment {
    pub id: String,
    #[serde(default)]
    pub filename: String,
    #[serde(default, deserialize_with = "nullable")]
    pub size: u64,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub proxy_url: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub width: u32,
    #[serde(default, deserialize_with = "nullable")]
    pub height: u32,
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub duration_secs: f64,
    /// A voice message's loudness, base64 of one byte a bar.
    #[serde(default)]
    pub waveform: Option<String>,
    /// Bit 3 marks a spoiler; older clients say it with the file's name.
    #[serde(default, deserialize_with = "nullable")]
    pub flags: u32,
}

impl Attachment {
    pub fn spoiler(&self) -> bool {
        self.flags & (1 << 3) != 0 || self.filename.starts_with("SPOILER_")
    }

    /// Flagged as explicit/sensitive media by Discord's scanner (`CONTAINS_EXPLICIT_MEDIA`, bit 4).
    pub fn is_explicit(&self) -> bool {
        self.flags & (1 << 4) != 0
    }
}

#[derive(Deserialize, Clone, Debug)]
pub struct EmbedImage {
    #[serde(default)]
    pub proxy_url: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub width: u32,
    #[serde(default, deserialize_with = "nullable")]
    pub height: u32,
    /// Seconds, on a video or gifv. Absent on a thumbnail or a still.
    #[serde(default, deserialize_with = "nullable")]
    pub duration_secs: f64,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Named {
    #[serde(default, deserialize_with = "nullable")]
    pub name: String,
}

#[derive(Deserialize, Clone, Debug)]
pub struct EmbedField {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub value: String,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Footer {
    #[serde(default, deserialize_with = "nullable")]
    pub text: String,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Embed {
    /// `rich` is what a bot or webhook composed; the others are built from a link.
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub footer: Option<Footer>,
    #[serde(default)]
    pub color: Option<u32>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub provider: Option<Named>,
    #[serde(default)]
    pub author: Option<Named>,
    #[serde(default)]
    pub thumbnail: Option<EmbedImage>,
    #[serde(default)]
    pub image: Option<EmbedImage>,
    /// The moving picture of a `gifv` (Tenor, Giphy) or `video` embed. Only its `proxy_url`,
    /// on Discord's own media host, is ever fetched.
    #[serde(default)]
    pub video: Option<EmbedImage>,
    #[serde(default, deserialize_with = "nullable")]
    pub fields: Vec<EmbedField>,
    /// Bit 4 marks explicit media (`CONTAINS_EXPLICIT_MEDIA`).
    #[serde(default, deserialize_with = "nullable")]
    pub flags: u32,
}

impl Embed {
    pub fn is_explicit(&self) -> bool {
        self.flags & (1 << 4) != 0
    }
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct CountDetails {
    #[serde(default)]
    pub burst: u32,
    #[serde(default)]
    pub normal: u32,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Reacted {
    #[serde(default)]
    pub count: u32,
    #[serde(default)]
    pub count_details: Option<CountDetails>,
    #[serde(default)]
    pub me: bool,
    #[serde(default)]
    pub me_burst: bool,
    #[serde(default, deserialize_with = "nullable")]
    pub burst_colors: Vec<String>,
    pub emoji: Emoji,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Reference {
    #[serde(default)]
    pub message_id: Option<String>,
    #[serde(default)]
    pub channel_id: Option<String>,
    #[serde(default)]
    pub guild_id: Option<String>,
    /// 0 is a reply, 1 a forward.
    #[serde(rename = "type", default)]
    pub kind: u8,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Sticker {
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// 1 PNG, 2 APNG, 3 Lottie, 4 GIF.
    #[serde(default)]
    pub format_type: u8,
}

#[derive(Deserialize, Clone, Debug)]
pub struct PollText {
    #[serde(default)]
    pub text: Option<String>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Poll {
    #[serde(default)]
    pub question: Option<PollText>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Snapshot {
    pub message: Box<Posted>,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct SelectOption {
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub emoji: Option<Emoji>,
    #[serde(default, deserialize_with = "nullable")]
    pub default: bool,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct UnfurledMedia {
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub proxy_url: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub width: u32,
    #[serde(default, deserialize_with = "nullable")]
    pub height: u32,
    #[serde(default)]
    pub content_type: Option<String>,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct MediaGalleryItem {
    #[serde(default)]
    pub media: Option<UnfurledMedia>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub spoiler: bool,
}

/// One Discord message or modal component (v1 action rows/buttons/selects/text inputs and v2 layout/display).
#[derive(Deserialize, Clone, Debug, Default)]
pub struct Component {
    #[serde(rename = "type", default)]
    pub kind: u8,
    #[serde(default)]
    pub id: Option<u64>,
    #[serde(default)]
    pub custom_id: Option<String>,
    #[serde(default)]
    pub style: Option<u8>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub emoji: Option<Emoji>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub disabled: bool,
    #[serde(default)]
    pub placeholder: Option<String>,
    #[serde(default)]
    pub min_values: Option<u8>,
    #[serde(default)]
    pub max_values: Option<u8>,
    #[serde(default, deserialize_with = "nullable")]
    pub options: Vec<SelectOption>,
    #[serde(default, deserialize_with = "nullable")]
    pub components: Vec<Component>,
    /// Type 10 Text Display markdown content.
    #[serde(default)]
    pub content: Option<String>,
    /// Type 9 Section accessory (button or thumbnail).
    #[serde(default)]
    pub accessory: Option<Box<Component>>,
    /// Type 12 Media Gallery items.
    #[serde(default, deserialize_with = "nullable")]
    pub items: Vec<MediaGalleryItem>,
    /// Type 11 Thumbnail media.
    #[serde(default)]
    pub media: Option<UnfurledMedia>,
    /// Type 13 File media.
    #[serde(default)]
    pub file: Option<UnfurledMedia>,
    /// Type 4 Text Input pre-filled value and bounds.
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub min_length: Option<u16>,
    #[serde(default)]
    pub max_length: Option<u16>,
    #[serde(default)]
    pub required: Option<bool>,
}

/// Message flags the row reads. The rest are left in the raw bit set on a control's block id.
pub const FLAG_IS_CROSSPOST: u64 = 1 << 1;
pub const FLAG_EPHEMERAL: u64 = 1 << 6;
pub const FLAG_LOADING: u64 = 1 << 7;
pub const FLAG_SUPPRESS_NOTIFICATIONS: u64 = 1 << 12;
pub const FLAG_IS_VOICE_MESSAGE: u64 = 1 << 13;

/// Who ran a command, from `interaction` or `interaction_metadata`. Extra fields are ignored.
#[derive(Deserialize, Clone, Debug, Default)]
pub struct InteractionMeta {
    /// 2 is an application command. Anything else is not a slash or context-menu command.
    #[serde(rename = "type", default)]
    pub kind: u8,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub user: Option<User>,
}

/// A message, whole or (in `MESSAGE_UPDATE`) only the fields that changed.
#[derive(Deserialize, Clone, Debug, Default)]
pub struct Posted {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub channel_id: String,
    #[serde(default)]
    pub guild_id: Option<String>,
    #[serde(default)]
    pub author: Option<User>,
    #[serde(default)]
    pub member: Option<Member>,
    #[serde(default)]
    pub webhook_id: Option<String>,
    #[serde(default)]
    pub application_id: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub edited_timestamp: Option<String>,
    #[serde(default)]
    pub attachments: Option<Vec<Attachment>>,
    #[serde(default)]
    pub embeds: Option<Vec<Embed>>,
    #[serde(default, deserialize_with = "nullable")]
    pub mentions: Vec<User>,
    #[serde(default, deserialize_with = "nullable")]
    pub mention_roles: Vec<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub mention_everyone: bool,
    #[serde(default)]
    pub reactions: Option<Vec<Reacted>>,
    #[serde(default)]
    pub pinned: Option<bool>,
    #[serde(rename = "type", default)]
    pub kind: Option<u8>,
    /// Absent on a partial update, which must not clear flags already stored.
    #[serde(default)]
    pub flags: Option<u64>,
    #[serde(default)]
    pub interaction: Option<InteractionMeta>,
    #[serde(default)]
    pub interaction_metadata: Option<InteractionMeta>,
    #[serde(default)]
    pub message_reference: Option<Reference>,
    #[serde(default)]
    pub referenced_message: Option<Box<Posted>>,
    #[serde(default)]
    pub thread: Option<Channel>,
    #[serde(default, deserialize_with = "nullable")]
    pub sticker_items: Vec<Sticker>,
    #[serde(default)]
    pub poll: Option<Poll>,
    #[serde(default)]
    pub components: Option<Vec<Component>>,
    #[serde(default, deserialize_with = "nullable")]
    pub message_snapshots: Vec<Snapshot>,
    /// A string from our own client, a number from some others.
    #[serde(default, deserialize_with = "nonce")]
    pub nonce: Option<String>,
    #[serde(default)]
    pub call: Option<CallPayload>,
    #[serde(default)]
    pub role_subscription_data: Option<RoleSubscriptionData>,
    #[serde(default)]
    pub purchase_notification: Option<PurchaseNotification>,
}

/// Discord's `role_subscription_data` on a type-25 role subscription purchase message.
#[derive(Deserialize, Clone, Debug, Default)]
pub struct RoleSubscriptionData {
    #[serde(default, deserialize_with = "nullable")]
    pub tier_name: String,
    #[serde(default, deserialize_with = "nullable")]
    pub total_months_subscribed: u32,
    #[serde(default, deserialize_with = "nullable")]
    pub is_renewal: bool,
}

/// Discord's `purchase_notification.guild_product_purchase` on a type-44 purchase message.
#[derive(Deserialize, Clone, Debug, Default)]
pub struct GuildProductPurchase {
    #[serde(default, deserialize_with = "nullable")]
    pub product_name: String,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct PurchaseNotification {
    #[serde(default)]
    pub guild_product_purchase: Option<GuildProductPurchase>,
}

/// Discord's `call` object on a type-3 call system message.
#[derive(Deserialize, Clone, Debug, Default)]
pub struct CallPayload {
    #[serde(default, deserialize_with = "nullable")]
    pub participants: Vec<String>,
    #[serde(default)]
    pub ended_timestamp: Option<String>,
}

fn nonce<'de, D: Deserializer<'de>>(de: D) -> Result<Option<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Either {
        Number(u64),
        Text(String),
    }
    Ok(match Option::<Either>::deserialize(de).map_err(de::Error::custom)? {
        Some(Either::Number(number)) => Some(number.to_string()),
        Some(Either::Text(text)) => Some(text),
        None => None,
    })
}

#[derive(Deserialize, Default)]
pub struct ProfileText {
    #[serde(default, deserialize_with = "nullable")]
    pub bio: String,
    #[serde(default, deserialize_with = "nullable")]
    pub pronouns: String,
    #[serde(default)]
    pub accent_color: Option<u32>,
    #[serde(default)]
    pub banner: Option<String>,
    #[serde(default)]
    pub theme_colors: Option<Vec<u32>>,
}

#[derive(Deserialize, Default)]
pub struct ProfileBadge {
    #[serde(default, deserialize_with = "nullable")]
    pub id: String,
    #[serde(default, deserialize_with = "nullable")]
    pub description: String,
    #[serde(default)]
    pub icon: Option<String>,
}

#[derive(Deserialize, Default)]
pub struct ConnectedAccount {
    #[serde(rename = "type", default, deserialize_with = "nullable")]
    pub kind: String,
    #[serde(default, deserialize_with = "nullable")]
    pub id: String,
    #[serde(default, deserialize_with = "nullable")]
    pub name: String,
    #[serde(default, deserialize_with = "nullable")]
    pub verified: bool,
}

/// Their membership of the server asked about (`guild_id` on the profile request).
#[derive(Deserialize, Default)]
pub struct ProfileMember {
    #[serde(default)]
    pub nick: Option<String>,
    #[serde(default)]
    pub avatar: Option<String>,
    #[serde(default)]
    pub banner: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub roles: Vec<String>,
    #[serde(default)]
    pub joined_at: Option<String>,
}

#[derive(Deserialize)]
pub struct MutualGuild {
    pub id: String,
    #[serde(default)]
    pub nick: Option<String>,
}

/// `GET /users/{id}/profile`: the parts a person's card shows.
#[derive(Deserialize)]
pub struct Profile {
    #[serde(default)]
    pub user: Option<User>,
    #[serde(default)]
    pub user_profile: Option<ProfileText>,
    #[serde(default)]
    pub guild_member_profile: Option<ProfileText>,
    #[serde(default, deserialize_with = "nullable")]
    pub mutual_guilds: Vec<MutualGuild>,
    #[serde(default, deserialize_with = "nullable")]
    pub mutual_friends: Vec<User>,
    #[serde(default)]
    pub mutual_friends_count: Option<u32>,
    #[serde(default)]
    pub guild_member: Option<ProfileMember>,
    #[serde(default, deserialize_with = "nullable")]
    pub badges: Vec<ProfileBadge>,
    #[serde(default, deserialize_with = "nullable")]
    pub guild_badges: Vec<ProfileBadge>,
    #[serde(default, deserialize_with = "nullable")]
    pub connected_accounts: Vec<ConnectedAccount>,
    #[serde(default)]
    pub premium_since: Option<String>,
    #[serde(default)]
    pub premium_guild_since: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

impl Profile {
    /// Their own words, cut to what a card holds.
    pub fn bio(&self) -> String {
        let mut bio = self.user_profile.as_ref().map_or_else(String::new, |profile| profile.bio.clone());
        protocol::clip(&mut bio, 1_000);
        bio
    }
}

/// Unix seconds from an ISO 8601 time as Discord writes it (`2026-10-13T08:15:00.123000+00:00`).
/// Discord always sends UTC, so the offset is not read.
pub fn iso_seconds(text: &str) -> Option<i64> {
    let part = |range: std::ops::Range<usize>| text.get(range)?.parse::<i64>().ok();
    let (year, month, day) = (part(0..4)?, part(5..7)?, part(8..10)?);
    let (hour, minute, second) = (part(11..13)?, part(14..16)?, part(17..19)?);
    // Days from the civil date (Howard Hinnant's algorithm).
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let of_era = year.rem_euclid(400);
    let of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let days = era * 146_097 + of_era * 365 + of_era / 4 - of_era / 100 + of_year - 719_468;
    Some(days * 86_400 + hour * 3600 + minute * 60 + second)
}

/// Formats Unix seconds as an ISO-8601 UTC timestamp (`2026-10-13T08:15:00Z`), the exact inverse
/// of [`iso_seconds`].
pub fn format_iso_utc(unix: i64) -> String {
    let days = unix.div_euclid(86_400);
    let secs = unix.rem_euclid(86_400);
    let hour = secs / 3600;
    let minute = (secs % 3600) / 60;
    let second = secs % 60;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    if month <= 2 {
        year += 1;
    }
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}
/// ISO 8601 UTC timestamp (`YYYY-MM-DDTHH:MM:SS.000Z`) from Unix seconds (inverse of [`iso_seconds`]).
pub fn seconds_iso(unix: i64) -> String {
    let days = unix.div_euclid(86_400);
    let secs = unix.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    let hour = secs / 3_600;
    let minute = (secs % 3_600) / 60;
    let second = secs % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.000Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_carry_their_time() {
        // The example id from Discord's documentation, created 2016-04-30 11:18:25 UTC.
        assert_eq!(seconds("175928847299117063"), 1_462_015_105);
        assert_eq!(iso_seconds("2016-04-30T11:18:25.796000+00:00"), Some(1_462_015_105));
        assert_eq!(seconds_iso(1_462_015_105), "2016-04-30T11:18:25.000Z");
        assert_eq!(iso_seconds(&seconds_iso(1_760_343_300)), Some(1_760_343_300));
    }

    #[test]
    fn a_voice_channel_keeps_its_bitrate() {
        let channel: Channel = serde_json::from_str(r#"{"id":"4201","type":2,"bitrate":32000}"#).unwrap();
        assert_eq!(channel.bitrate, 32_000);
        assert_eq!(Kept::of(&channel).channel().bitrate, 32_000);
        let missing: Channel = serde_json::from_str(r#"{"id":"1","type":0}"#).unwrap();
        assert_eq!(missing.bitrate, 0);
        let stored = serde_json::to_string(&Kept::of(&channel)).unwrap();
        let back: Kept = serde_json::from_str(&stored).unwrap();
        assert_eq!(back.bitrate, 32_000);
    }

    #[test]
    fn a_user_without_a_picture_gets_the_default_for_their_id() {
        let (file, url) = avatar("175928847299117063", None, "0");
        let index = (175_928_847_299_117_063u64 >> 22) % 6;
        assert_eq!(url, format!("{}/embed/avatars/{index}.png", cdn()));
        assert_eq!(file.id, format!("avatar:default:{index}"));
    }

    #[test]
    fn connection_urls_and_cdn_hashes_reject_injection() {
        assert_eq!(connection_url("github", "", "mara-dev"), "https://github.com/mara-dev");
        assert_eq!(connection_url("spotify", "sp99", "Spotify"), "https://open.spotify.com/user/sp99");
        assert_eq!(connection_url("domain", "", "example.com"), "https://example.com");
        assert_eq!(connection_url("github", "", "evil/../admin"), "");
        assert_eq!(connection_url("github", "", "name?x=1"), "");
        assert_eq!(connection_url("github", "", "name#frag"), "");
        assert_eq!(connection_url("spotify", "a/b", "x"), "");
        assert_eq!(connection_url("domain", "", "evil.com/foo?x=1"), "");
        assert_eq!(connection_url("domain", "", "https://evil.com"), "");
        assert_eq!(connection_url("domain", "", "user@evil.com"), "");
        assert_eq!(connection_url("domain", "", "evil.com:443"), "");
        assert_eq!(connection_url("domain", "", "localhost"), "");
        assert!(user_banner("2001", "userban1").is_some());
        assert!(user_banner("2001", "../x").is_none());
        assert!(user_banner("2001", "a/b").is_none());
        assert!(user_banner("2001", "").is_none());
        assert!(member_banner("4000", "2001", "guildban4000").is_some());
        assert!(member_banner("4000", "2001", "bad hash").is_none());
        assert!(badge_icon("2ba85e8026a8614b640c2837bcdfe21b").is_some());
        assert!(badge_icon("..").is_none());
        assert!(badge_icon("").is_none());
        assert!(!cdn_token("code/../admin"));
        assert!(cdn_token("fr1end"));
        let (file, url) = avatar("2001", Some("../x"), "0");
        assert!(url.contains("/embed/avatars/"), "{url}");
        assert!(file.id.starts_with("avatar:default:"));
        let (_, good) = avatar("2001", Some("a_goodhash"), "0");
        assert!(good.contains("/avatars/2001/a_goodhash.webp"), "{good}");
        assert!(avatar_variants("avatar:2001:../x").is_none());
        assert!(avatar_variants("avatar:2001:a_goodhash").is_some());
        assert!(member_avatar("4000", "2001", "../x").is_none());
        assert!(member_avatar("4000", "2001", "memhash").is_some());
        assert!(guild_icon("4000", "a/b").is_none());
        assert!(guild_icon("4000", "iconhash").is_some());
        assert!(group_icon("3001", "..").is_none());
        assert!(group_icon("3001", "ghash").is_some());
    }

    #[test]
    fn guild_banner_boosts_description_and_verified_mark_parse() {
        let guild: Guild = serde_json::from_str(
            r#"{
                "id": "4000",
                "properties": {
                    "name": "Rustaceans",
                    "banner": "a_banhash",
                    "description": "Friendly crabs",
                    "premium_tier": 2,
                    "premium_subscription_count": 9,
                    "features": ["COMMUNITY", "VERIFIED"],
                    "vanity_url_code": "rust"
                }
            }"#,
        )
        .unwrap();
        assert_eq!(guild.banner(), Some("a_banhash"));
        assert_eq!(guild.description(), Some("Friendly crabs"));
        assert_eq!(guild.premium_tier(), Some(2));
        assert_eq!(guild.premium_subscription_count(), Some(9));
        assert_eq!(guild.vanity_url_code(), Some("rust"));
        assert_eq!(guild_mark(guild.features()), Some(protocol::Mark::Verified));

        let (file, url) = guild_banner("4000", "a_banhash");
        assert_eq!(file.id, "banner:4000:a_banhash");
        assert_eq!(url, format!("{}/banners/4000/a_banhash.webp?size=512&animated=true", cdn()));
        assert_eq!(guild_mark(&["PARTNERED".to_owned()]), Some(protocol::Mark::Verified));
        assert_eq!(guild_mark(&["COMMUNITY".to_owned()]), None);
    }

    #[test]
    fn scheduled_event_and_cover_parse_cleanly() {
        let ev: ScheduledEvent = serde_json::from_str(
            r#"{
                "id": "9001",
                "guild_id": "4000",
                "channel_id": "4201",
                "name": "Game Night",
                "description": "Bring snacks",
                "scheduled_start_time": "2026-10-15T19:00:00+00:00",
                "scheduled_end_time": "2026-10-15T21:00:00+00:00",
                "status": 1,
                "entity_type": 2,
                "user_count": 12,
                "image": "covhash"
            }"#,
        )
        .unwrap();
        assert_eq!(ev.id, "9001");
        assert_eq!(ev.status, 1);
        assert_eq!(ev.entity_type, 2);
        assert_eq!(ev.user_count, Some(12));
        let (file, url) = event_cover(&ev.id, ev.image.as_deref().unwrap());
        assert_eq!(file.id, "event_cover:9001:covhash");
        assert_eq!(url, format!("{}/guild-events/9001/covhash.webp?size=512", cdn()));

        let settings: GuildSettings = serde_json::from_str(r#"{"guild_id":"4000","mute_scheduled_events":true}"#).unwrap();
        assert!(settings.mute_scheduled_events);
    }

    #[test]
    fn format_iso_utc_inverts_iso_seconds_and_guild_overview_fields_parse() {
        for ts in [1_462_015_105, 1_700_000_000, 1_760_356_800, 1_800_000_000] {
            let formatted = format_iso_utc(ts);
            assert_eq!(iso_seconds(&formatted), Some(ts), "round-trip failed for {ts} ({formatted})");
        }
        let guild: Guild = serde_json::from_str(
            r#"{
                "id": "4000",
                "properties": {
                    "name": "Rustaceans",
                    "system_channel_id": "4101",
                    "afk_channel_id": "4201",
                    "afk_timeout": 900,
                    "default_message_notifications": 1
                }
            }"#,
        )
        .unwrap();
        assert_eq!(guild.system_channel_id(), Some("4101"));
        assert_eq!(guild.afk_channel_id(), Some("4201"));
        assert_eq!(guild.afk_timeout(), Some(900));
        assert_eq!(guild.default_message_notifications(), Some(1));
    }

    #[test]
    fn role_fields_default_and_icon_urls_stay_on_the_cdn() {
        let role: Role = serde_json::from_str(
            r#"{"id":"4701","name":"Newbie","permissions":"2048","color":16711680,"hoist":true,"position":1,"mentionable":true,"managed":false,"icon":"a1b2c3d4","unicode_emoji":null}"#,
        )
        .unwrap();
        assert_eq!((role.color, role.position, role.permissions), (16_711_680, 1, 2048));
        assert!(role.hoist && role.mentionable && !role.managed);
        assert_eq!(role.icon.as_deref(), Some("a1b2c3d4"));
        assert!(role.unicode_emoji.is_none());
        let bare: Role = serde_json::from_str(r#"{"id":"1","name":"x","permissions":0,"color":null,"icon":null}"#).unwrap();
        assert_eq!(bare.color, 0);
        assert!(!bare.hoist && !bare.mentionable && !bare.managed);
        assert_eq!(bare.position, 0);
        assert!(bare.icon.is_none() && bare.unicode_emoji.is_none());

        let url = role_icon_url("4701", "a1b2c3d4").unwrap();
        assert_eq!(url, format!("{}/role-icons/4701/a1b2c3d4.png", cdn().trim_end_matches('/')));
        assert!(url.starts_with("https://") || url.starts_with("http://127.0.0.1") || url.starts_with("http://localhost") || url.starts_with("http://[::1]"));
        assert!(!url.contains('@'));
        assert!(url.contains("/role-icons/4701/a1b2c3d4.png"));
        assert!(role_icon_url("4701", "a1b2/../../evil").is_none());
        assert!(role_icon_url("4701", "https://evil.example/x").is_none());
        assert!(role_icon_url("4701", "a1b2c3d4?x=1").is_none());
        assert!(role_icon_url("nope", "a1b2c3d4").is_none());
        assert!(role_icon_hash(Some("a_ab12")).as_deref() == Some("a_ab12"));
        assert!(role_icon_hash(Some("not a hash")).is_none());
        assert!(role_emoji(Some("🌿")).as_deref() == Some("🌿"));
        assert!(role_emoji(Some("https://evil.example")).is_none());
        assert!(cdn_base_ok("https://cdn.discordapp.com"));
        assert!(cdn_base_ok("http://127.0.0.1:9/media"));
        assert!(cdn_base_ok("http://localhost:9/media"));
        assert!(cdn_base_ok("http://[::1]:9/media"));
        assert!(!cdn_base_ok("http://evil.example/media"));
        assert!(!cdn_base_ok("https://user:pass@cdn.discordapp.com"));
        assert!(!cdn_base_ok("http://127.0.0.1:9/media user"));
        assert!(cdn_base_ok(cdn().trim_end_matches('/')));
    }

    #[test]
    fn server_avatar_and_banner_yield_guild_cdn_urls_and_variants() {
        let (file, url) = member_avatar("4000", "2001", "srvhash").unwrap();
        assert_eq!(file.id, "mavatar:4000:2001:srvhash");
        assert_eq!(url, format!("{}/guilds/4000/users/2001/avatars/srvhash.webp?size=80", cdn()));
        let variants = avatar_variants(&file.id).expect("member avatar variants");
        assert_eq!(variants.len(), 2);
        assert_eq!(variants[0], (128, format!("{}/guilds/4000/users/2001/avatars/srvhash.webp?size=128", cdn())));
        assert_eq!(variants[1], (256, format!("{}/guilds/4000/users/2001/avatars/srvhash.webp?size=256", cdn())));

        let (anim, anim_url) = member_avatar("4000", "2001", "a_spin").unwrap();
        assert!(anim_url.ends_with("&animated=true"));
        assert!(avatar_variants(&anim.id).unwrap()[0].1.ends_with("&animated=true"));

        let (mban, mban_url) = member_banner("4000", "2001", "bhash").unwrap();
        assert_eq!(mban.id, "mbanner:4000:2001:bhash");
        assert_eq!(mban_url, format!("{}/guilds/4000/users/2001/banners/bhash.webp?size=600", cdn()));

        let (uban, uban_url) = user_banner("2001", "ubhash").unwrap();
        assert_eq!(uban.id, "banner:2001:ubhash");
        assert_eq!(uban_url, format!("{}/banners/2001/ubhash.webp?size=600", cdn()));

        let member: Member = serde_json::from_str(
            r#"{"user":{"id":"2001","username":"mara","avatar":"ghash"},"avatar":"srvhash","banner":"bhash"}"#,
        )
        .unwrap();
        assert_eq!(member.avatar(Some("4000")).unwrap().0.id, "mavatar:4000:2001:srvhash");
        assert_eq!(member.avatar(None).unwrap().0.id, "avatar:2001:ghash");
        assert_eq!(member.banner.as_deref(), Some("bhash"));
    }

    #[test]
    fn presence_parses_activities_details_timestamps_art_urls_and_custom_emoji() {
        let p: PresenceUpdate = serde_json::from_value(serde_json::json!({
            "user": {"id": "2001"},
            "status": "online",
            "activities": [
                {"type": 4, "state": "vibing", "emoji": {"id": "9901", "name": "ferris", "animated": true}},
                {
                    "type": 2,
                    "name": "Spotify",
                    "details": "Clair de Lune",
                    "state": "Debussy",
                    "timestamps": {"start": 1_700_000_000_000i64},
                    "assets": {"large_image": "spotify:ab67616d0000b2731234"}
                }
            ]
        }))
        .unwrap();
        assert_eq!(p.status(), "vibing");
        assert_eq!(p.emoji(), "");
        let (ef, eurl) = p.emoji_file().expect("custom status emoji");
        assert_eq!(ef.id, "emoji:9901");
        assert!(eurl.contains("/emojis/9901.webp") && eurl.contains("animated=true"));

        let (doing, art) = p.doing_with_art().expect("spotify activity");
        assert_eq!(doing.kind, protocol::ActivityKind::Listening);
        assert_eq!(doing.name, "Spotify");
        assert_eq!(doing.details, "Clair de Lune");
        assert_eq!(doing.state, "Debussy");
        assert_eq!(doing.started, Some(1_700_000_000));
        let (art_file, art_url) = art.expect("spotify album art");
        assert_eq!(art_file.id, "actart:spotify:ab67616d0000b2731234");
        assert_eq!(art_url, "https://i.scdn.co/image/ab67616d0000b2731234");

        // Streaming URL validation: accepts https://, rejects javascript:.
        let valid_stream: PresenceUpdate = serde_json::from_value(serde_json::json!({
            "user": {"id": "2002"},
            "status": "online",
            "activities": [{"type": 1, "name": "Hades", "url": "https://twitch.tv/abc"}]
        }))
        .unwrap();
        assert_eq!(valid_stream.doing().unwrap().url, "https://twitch.tv/abc");

        let bad_stream: PresenceUpdate = serde_json::from_value(serde_json::json!({
            "user": {"id": "2003"},
            "status": "online",
            "activities": [{"type": 1, "name": "Hades", "url": "javascript:alert(1)"}]
        }))
        .unwrap();
        assert_eq!(bad_stream.doing().unwrap().url, "");

        let plain_http: PresenceUpdate = serde_json::from_value(serde_json::json!({
            "user": {"id": "2004"},
            "status": "online",
            "activities": [{"type": 1, "name": "Hades", "url": "http://twitch.tv/abc"}]
        }))
        .unwrap();
        assert_eq!(plain_http.doing().unwrap().url, "");
    }
}
