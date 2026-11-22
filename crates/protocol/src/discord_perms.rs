//! Shared Discord permission bit definitions and overwrite helpers (#397, shared with #428).
//!
//! Bits outside [`known_permission_mask`] are preserved untouched on round-trips via
//! [`merge_overwrite_bits`].

use crate::Key;
use serde::{Deserialize, Serialize};

pub const PERM_ADMINISTRATOR: u64 = 1 << 3;
pub const PERM_VIEW_CHANNEL: u64 = 1 << 10;
pub const PERM_MANAGE_ROLES: u64 = 1 << 28;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PermissionSection {
    General,
    Text,
    Voice,
}

impl PermissionSection {
    pub const ALL: [PermissionSection; 3] = [
        PermissionSection::General,
        PermissionSection::Text,
        PermissionSection::Voice,
    ];

    pub fn label(self) -> &'static str {
        match self {
            PermissionSection::General => "General",
            PermissionSection::Text => "Text",
            PermissionSection::Voice => "Voice",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PermissionDef {
    pub bit: u64,
    pub key: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub section: PermissionSection,
}

/// Shared Discord channel/role permission table.
pub const DISCORD_PERMISSIONS: &[PermissionDef] = &[
    // General
    PermissionDef {
        bit: 1 << 10,
        key: "VIEW_CHANNEL",
        label: "View Channel",
        description: "See this channel in the server channel list.",
        section: PermissionSection::General,
    },
    PermissionDef {
        bit: 1 << 4,
        key: "MANAGE_CHANNELS",
        label: "Manage Channel",
        description: "Edit the channel's name, topic, slow mode, and delete it.",
        section: PermissionSection::General,
    },
    PermissionDef {
        bit: 1 << 28,
        key: "MANAGE_ROLES",
        label: "Manage Permissions",
        description: "Edit permission overwrites for roles and members on this channel.",
        section: PermissionSection::General,
    },
    PermissionDef {
        bit: 1 << 0,
        key: "CREATE_INSTANT_INVITE",
        label: "Create Invite",
        description: "Create invite links to this channel.",
        section: PermissionSection::General,
    },
    PermissionDef {
        bit: 1 << 29,
        key: "MANAGE_WEBHOOKS",
        label: "Manage Webhooks",
        description: "Create, edit, and delete webhooks in this channel.",
        section: PermissionSection::General,
    },
    // Text
    PermissionDef {
        bit: 1 << 11,
        key: "SEND_MESSAGES",
        label: "Send Messages",
        description: "Send messages in text channels or create posts in forums.",
        section: PermissionSection::Text,
    },
    PermissionDef {
        bit: 1 << 38,
        key: "SEND_MESSAGES_IN_THREADS",
        label: "Send Messages in Threads",
        description: "Send messages inside threads under this channel.",
        section: PermissionSection::Text,
    },
    PermissionDef {
        bit: 1 << 35,
        key: "CREATE_PUBLIC_THREADS",
        label: "Create Public Threads",
        description: "Start threads visible to everyone who can view the channel.",
        section: PermissionSection::Text,
    },
    PermissionDef {
        bit: 1 << 36,
        key: "CREATE_PRIVATE_THREADS",
        label: "Create Private Threads",
        description: "Start invite-only threads under this channel.",
        section: PermissionSection::Text,
    },
    PermissionDef {
        bit: 1 << 14,
        key: "EMBED_LINKS",
        label: "Embed Links",
        description: "Show rich link previews on links posted in messages.",
        section: PermissionSection::Text,
    },
    PermissionDef {
        bit: 1 << 15,
        key: "ATTACH_FILES",
        label: "Attach Files",
        description: "Upload images, videos, and files.",
        section: PermissionSection::Text,
    },
    PermissionDef {
        bit: 1 << 6,
        key: "ADD_REACTIONS",
        label: "Add Reactions",
        description: "Add new emoji reactions to messages.",
        section: PermissionSection::Text,
    },
    PermissionDef {
        bit: 1 << 18,
        key: "USE_EXTERNAL_EMOJIS",
        label: "Use External Emoji",
        description: "Use custom emoji from other servers.",
        section: PermissionSection::Text,
    },
    PermissionDef {
        bit: 1 << 37,
        key: "USE_EXTERNAL_STICKERS",
        label: "Use External Stickers",
        description: "Send custom stickers from other servers.",
        section: PermissionSection::Text,
    },
    PermissionDef {
        bit: 1 << 17,
        key: "MENTION_EVERYONE",
        label: "Mention @everyone, @here, and All Roles",
        description: "Ping everyone in the channel or any role.",
        section: PermissionSection::Text,
    },
    PermissionDef {
        bit: 1 << 13,
        key: "MANAGE_MESSAGES",
        label: "Manage Messages",
        description: "Delete other members' messages and remove reactions.",
        section: PermissionSection::Text,
    },
    PermissionDef {
        bit: 1 << 51,
        key: "PIN_MESSAGES",
        label: "Pin Messages",
        description: "Pin or unpin messages in this channel.",
        section: PermissionSection::Text,
    },
    PermissionDef {
        bit: 1 << 34,
        key: "MANAGE_THREADS",
        label: "Manage Threads",
        description: "Rename, archive, lock, or delete threads.",
        section: PermissionSection::Text,
    },
    PermissionDef {
        bit: 1 << 16,
        key: "READ_MESSAGE_HISTORY",
        label: "Read Message History",
        description: "Read messages sent before opening the channel.",
        section: PermissionSection::Text,
    },
    PermissionDef {
        bit: 1 << 12,
        key: "SEND_TTS_MESSAGES",
        label: "Send Text-to-Speech Messages",
        description: "Send /tts messages read aloud to listeners.",
        section: PermissionSection::Text,
    },
    PermissionDef {
        bit: 1 << 46,
        key: "SEND_VOICE_MESSAGES",
        label: "Send Voice Messages",
        description: "Record and send voice messages.",
        section: PermissionSection::Text,
    },
    // Voice
    PermissionDef {
        bit: 1 << 20,
        key: "CONNECT",
        label: "Connect",
        description: "Join voice channels and hear others.",
        section: PermissionSection::Voice,
    },
    PermissionDef {
        bit: 1 << 21,
        key: "SPEAK",
        label: "Speak",
        description: "Talk in voice channels.",
        section: PermissionSection::Voice,
    },
    PermissionDef {
        bit: 1 << 9,
        key: "STREAM",
        label: "Video / Go Live",
        description: "Share camera video or screen in voice channels.",
        section: PermissionSection::Voice,
    },
    PermissionDef {
        bit: 1 << 25,
        key: "USE_VAD",
        label: "Use Voice Activity",
        description: "Speak without holding Push-to-Talk.",
        section: PermissionSection::Voice,
    },
    PermissionDef {
        bit: 1 << 8,
        key: "PRIORITY_SPEAKER",
        label: "Priority Speaker",
        description: "Lower others' volume while holding Priority Speak.",
        section: PermissionSection::Voice,
    },
    PermissionDef {
        bit: 1 << 22,
        key: "MUTE_MEMBERS",
        label: "Mute Members",
        description: "Server-mute other members in voice channels.",
        section: PermissionSection::Voice,
    },
    PermissionDef {
        bit: 1 << 23,
        key: "DEAFEN_MEMBERS",
        label: "Deafen Members",
        description: "Server-deafen other members in voice channels.",
        section: PermissionSection::Voice,
    },
    PermissionDef {
        bit: 1 << 24,
        key: "MOVE_MEMBERS",
        label: "Move Members",
        description: "Move or disconnect members in voice channels.",
        section: PermissionSection::Voice,
    },
    PermissionDef {
        bit: 1 << 42,
        key: "USE_SOUNDBOARD",
        label: "Use Soundboard",
        description: "Play soundboard clips in voice channels.",
        section: PermissionSection::Voice,
    },
    PermissionDef {
        bit: 1 << 32,
        key: "REQUEST_TO_SPEAK",
        label: "Request to Speak",
        description: "Raise a hand to speak in Stage channels.",
        section: PermissionSection::Voice,
    },
];

/// Bitmask of every permission bit present in [`DISCORD_PERMISSIONS`].
pub fn known_permission_mask() -> u64 {
    DISCORD_PERMISSIONS.iter().fold(0u64, |acc, p| acc | p.bit)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum OverwriteTargetKind {
    #[default]
    Role,
    Member,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ChannelOverwrite {
    pub id: String,
    #[serde(default)]
    pub kind: OverwriteTargetKind,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub allow: u64,
    #[serde(default)]
    pub deny: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SpaceRoleSummary {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub permissions: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelPermissionsInfo {
    pub conversation: Key,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub is_category: bool,
    #[serde(default)]
    pub private: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synced_with_category: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_name: Option<String>,
    #[serde(default)]
    pub my_permissions: u64,
    #[serde(default)]
    pub is_owner: bool,
    #[serde(default)]
    pub guild_permissions: u64,
    #[serde(default)]
    pub my_roles: Vec<String>,
    #[serde(default)]
    pub me_id: String,
    #[serde(default)]
    pub everyone_role_id: String,
    #[serde(default)]
    pub overwrites: Vec<ChannelOverwrite>,
    #[serde(default)]
    pub roles: Vec<SpaceRoleSummary>,
}

/// Merges edited known bits with an existing overwrite's unknown bits so unrecognized Discord
/// permission bits survive round-trips untouched. A bit cannot be both allowed and denied.
pub fn merge_overwrite_bits(
    existing_allow: u64,
    existing_deny: u64,
    known_mask: u64,
    edited_allow: u64,
    edited_deny: u64,
) -> (u64, u64) {
    let unknown_allow = existing_allow & !known_mask;
    let unknown_deny = existing_deny & !known_mask;
    let known_deny = edited_deny & known_mask;
    let known_allow = (edited_allow & known_mask) & !known_deny;
    (unknown_allow | known_allow, unknown_deny | known_deny)
}

/// Computes effective channel permissions using Discord's overwrite precedence:
/// base guild permissions -> `@everyone` overwrite -> union of held role overwrites -> member overwrite.
pub fn effective_permissions(
    is_owner: bool,
    guild_perms: u64,
    me: &str,
    everyone_role: &str,
    my_roles: &[String],
    overwrites: &[ChannelOverwrite],
) -> u64 {
    if is_owner || (guild_perms & PERM_ADMINISTRATOR != 0) {
        return u64::MAX;
    }
    let mut allowed = guild_perms;
    if let Some(all) = overwrites.iter().find(|o| o.kind == OverwriteTargetKind::Role && o.id == everyone_role) {
        allowed = (allowed & !all.deny) | all.allow;
    }
    let (role_allow, role_deny) = overwrites
        .iter()
        .filter(|o| o.kind == OverwriteTargetKind::Role && o.id != everyone_role && my_roles.iter().any(|r| r == &o.id))
        .fold((0u64, 0u64), |(a, d), o| (a | o.allow, d | o.deny));
    allowed = (allowed & !role_deny) | role_allow;
    if let Some(own) = overwrites.iter().find(|o| o.kind == OverwriteTargetKind::Member && o.id == me) {
        allowed = (allowed & !own.deny) | own.allow;
    }
    allowed
}

/// Returns `true` if applying the proposed overwrite (or removing it when `proposed == None`)
/// would strip the current user of `VIEW_CHANNEL` or `MANAGE_ROLES` on this channel.
/// Server owners and `ADMINISTRATOR`s can never lock themselves out.
#[allow(clippy::too_many_arguments)]
pub fn would_lock_self_out(
    is_owner: bool,
    guild_perms: u64,
    me: &str,
    everyone_role: &str,
    my_roles: &[String],
    overwrites: &[ChannelOverwrite],
    target_id: &str,
    target_kind: OverwriteTargetKind,
    proposed: Option<(u64, u64)>,
) -> bool {
    if is_owner || (guild_perms & PERM_ADMINISTRATOR != 0) {
        return false;
    }
    let mut next: Vec<ChannelOverwrite> = overwrites
        .iter()
        .filter(|o| !(o.id == target_id && o.kind == target_kind))
        .cloned()
        .collect();
    if let Some((allow, deny)) = proposed {
        next.push(ChannelOverwrite {
            id: target_id.to_owned(),
            kind: target_kind,
            name: String::new(),
            allow,
            deny,
        });
    }
    let after = effective_permissions(is_owner, guild_perms, me, everyone_role, my_roles, &next);
    (after & PERM_VIEW_CHANNEL == 0) || (after & PERM_MANAGE_ROLES == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_overwrite_bits_preserves_unknown_bits_untouched() {
        let mask = known_permission_mask();
        let unknown_a = 1u64 << 60;
        let unknown_d = 1u64 << 61;
        assert_eq!(mask & (unknown_a | unknown_d), 0);

        let existing_allow = PERM_VIEW_CHANNEL | unknown_a;
        let existing_deny = (1u64 << 11) | unknown_d;
        // Toggle SEND_MESSAGES (1<<11) to allow and VIEW_CHANNEL to inherit (0).
        let (allow, deny) = merge_overwrite_bits(existing_allow, existing_deny, mask, 1u64 << 11, 0);
        assert_eq!(allow, (1u64 << 11) | unknown_a, "unknown allow bit must survive");
        assert_eq!(deny, unknown_d, "unknown deny bit must survive");
        // Contradictory known bit (both allow and deny) resolves to deny.
        let (allow2, deny2) = merge_overwrite_bits(0, 0, mask, PERM_VIEW_CHANNEL, PERM_VIEW_CHANNEL);
        assert_eq!(allow2 & PERM_VIEW_CHANNEL, 0);
        assert_ne!(deny2 & PERM_VIEW_CHANNEL, 0);
    }

    #[test]
    fn self_lockout_guard_blocks_non_admin_and_allows_owner_or_admin() {
        let guild_perms = PERM_VIEW_CHANNEL | PERM_MANAGE_ROLES;
        let roles = vec!["role-mod".to_owned()];
        // Denying VIEW_CHANNEL on @everyone without a compensating role/member allow locks self out.
        assert!(would_lock_self_out(
            false,
            guild_perms,
            "me",
            "guild",
            &roles,
            &[],
            "guild",
            OverwriteTargetKind::Role,
            Some((0, PERM_VIEW_CHANNEL)),
        ));
        // Denying MANAGE_ROLES on our own member overwrite locks self out.
        assert!(would_lock_self_out(
            false,
            guild_perms,
            "me",
            "guild",
            &roles,
            &[],
            "me",
            OverwriteTargetKind::Member,
            Some((0, PERM_MANAGE_ROLES)),
        ));
        // With our role already allowing VIEW_CHANNEL, denying @everyone VIEW_CHANNEL is safe!
        let existing = vec![ChannelOverwrite {
            id: "role-mod".into(),
            kind: OverwriteTargetKind::Role,
            name: "Mods".into(),
            allow: PERM_VIEW_CHANNEL,
            deny: 0,
        }];
        assert!(!would_lock_self_out(
            false,
            guild_perms,
            "me",
            "guild",
            &roles,
            &existing,
            "guild",
            OverwriteTargetKind::Role,
            Some((0, PERM_VIEW_CHANNEL)),
        ));
        // Owner and ADMINISTRATOR are never locked out.
        assert!(!would_lock_self_out(
            true,
            0,
            "me",
            "guild",
            &roles,
            &[],
            "guild",
            OverwriteTargetKind::Role,
            Some((0, PERM_VIEW_CHANNEL | PERM_MANAGE_ROLES)),
        ));
        assert!(!would_lock_self_out(
            false,
            PERM_ADMINISTRATOR,
            "me",
            "guild",
            &roles,
            &[],
            "me",
            OverwriteTargetKind::Member,
            Some((0, PERM_VIEW_CHANNEL | PERM_MANAGE_ROLES)),
        ));
    }
}
