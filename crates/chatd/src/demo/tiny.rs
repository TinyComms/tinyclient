//! The demo's Tiny server, "Night Shift": a small voice community with an icon, a welcome,
//! roles in colour, three categories of voice channels, and people seated in two calls with
//! every state a seat can be in (speaking, muted, deafened, server-muted, suppressed, holding
//! priority). We are Robin Vale, an admin: everything a moderator can do shows, and the owner
//! is above us, so what the hierarchy hides shows too.
//!
//! Server Settings has something on every page: roles with permissions, channel overrides,
//! open invites (the setup's owner invite among them), bans, and a few weeks of audit with the
//! setup's rows at the start.
//!
//! A real Tiny server is read over HTTP (`adapters::tiny`); there is none behind the demo, so
//! the server answers the Tiny requests for this account from here ([`answer`]). Each change
//! is applied to the page kept here and told to the shell as `Change::Tiny`, so the window can
//! be driven by hand.

use super::{Fallible, Seeder, Who};
use crate::hub::Hub;
use protocol::{
    AccountKey, Change, MediaKind, Network, Request, Response, TinyAccess, TinyAudit, TinyBan,
    TinyCapabilities, TinyCategory, TinyCode, TinyCommunity, TinyInvite, TinyMember, TinyOverride,
    TinyRole, TinyRoleEdit, TinyRoom, TinySeat, TinyServerInfo, TINY_ALL, TINY_BAN_MEMBERS,
    TINY_CREATE_INVITES, TINY_DEAFEN_MEMBERS, TINY_DISCONNECT_MEMBERS, TINY_EVERYONE_DEFAULT,
    TINY_KICK_MEMBERS, TINY_MOVE_MEMBERS, TINY_MUTE_MEMBERS, TINY_PRIORITY_SPEAKER, TINY_SPEAK,
    TINY_TIMEOUT_MEMBERS, TINY_VIEW_AUDIT_LOG, TINY_VOICE_ACTIVITY,
};
use std::sync::Mutex;

/// Our user id on the server, which is also the account's id.
pub const ME: &str = "tiny-robin";
const ORIGIN: &str = "https://nightshift.example";

/// `hours` from now, as the server writes a time: what runs out (an invite, a timeout, a ban)
/// has not, whenever the demo is seeded.
fn from_now(hours: i64) -> String {
    protocol::tiny_time(crate::hub::now() + hours * 3_600)
}

pub fn account() -> AccountKey {
    AccountKey {
        network: Network::Tiny,
        account: ME.to_owned(),
    }
}

/// The page as the demo keeps it. Set by [`seed`], changed by [`answer`].
static PAGE: Mutex<Option<TinyCommunity>> = Mutex::new(None);

const OWNER: &str = "tiny-mara";
const DEV: &str = "tiny-dev";
const LEILA: &str = "tiny-leila";
const THEO: &str = "tiny-theo";
const SAM: &str = "tiny-sam";
const LENA: &str = "tiny-lena";
const KOFI: &str = "tiny-kofi";
const INES: &str = "tiny-ines";
const PRIYA: &str = "tiny-priya";
const PAVEL: &str = "tiny-pavel";

const ADMIN: &str = "role-admin";
const MODERATOR: &str = "role-moderator";
const RAID: &str = "role-raid";
const EVERYONE: &str = "role-everyone";

fn member(
    user_id: &str,
    name: &str,
    email: &str,
    joined: &str,
    roles: &[&str],
    owner: bool,
    last_seen: Option<&str>,
) -> TinyMember {
    let role = if owner {
        "owner"
    } else if roles.contains(&ADMIN) {
        "admin"
    } else {
        "member"
    };
    TinyMember {
        user_id: user_id.to_owned(),
        email: email.to_owned(),
        display_name: name.to_owned(),
        role: role.to_owned(),
        created_at: joined.to_owned(),
        owner,
        roles: roles.iter().map(|role| (*role).to_owned()).collect(),
        last_seen_at: last_seen.map(str::to_owned),
        timeout_until: None,
        joined_via: Some("invite".to_owned()),
        avatar: None,
        avatar_path: String::new(),
    }
}

fn room(
    id: &str,
    name: &str,
    category: &str,
    position: i32,
    topic: &str,
    limit: u32,
    bitrate: u32,
    ptt: bool,
) -> TinyRoom {
    TinyRoom {
        category_id: Some(category.to_owned()),
        position,
        topic: topic.to_owned(),
        user_limit: limit,
        bitrate_kbps: bitrate,
        ptt_only: ptt,
        ..TinyRoom::new(id, name)
    }
}

fn seat(user_id: &str) -> TinySeat {
    TinySeat {
        user_id: user_id.to_owned(),
        ..TinySeat::default()
    }
}

fn role(
    id: &str,
    name: &str,
    color: Option<&str>,
    position: i32,
    permissions: u64,
    hoist: bool,
    count: u32,
) -> TinyRole {
    TinyRole {
        id: id.to_owned(),
        name: name.to_owned(),
        color: color.map(str::to_owned),
        position,
        permissions,
        hoist,
        default: id == EVERYONE,
        member_count: count,
    }
}

fn detailed(row: TinyAudit, detail: &str) -> TinyAudit {
    TinyAudit {
        detail: detail.to_owned(),
        ..row
    }
}

/// The server's own row: no actor.
fn by_setup(id: i64, action: &str, at: &str) -> TinyAudit {
    TinyAudit {
        actor_id: None,
        actor_name: String::new(),
        ..audit(id, "", "", action, None, at, "")
    }
}

fn audit(
    id: i64,
    actor: &str,
    actor_name: &str,
    action: &str,
    subject: Option<(&str, &str)>,
    at: &str,
    reason: &str,
) -> TinyAudit {
    TinyAudit {
        id,
        actor_id: Some(actor.to_owned()),
        action: action.to_owned(),
        subject: String::new(),
        detail: String::new(),
        created_at: at.to_owned(),
        actor_name: actor_name.to_owned(),
        subject_user_id: subject.map(|(id, _)| id.to_owned()),
        subject_name: subject.map(|(_, name)| name.to_owned()).unwrap_or_default(),
        reason: reason.to_owned(),
    }
}

/// The page, its icon on disk, and us.
pub fn seed(s: &mut Seeder) -> Fallible<Who> {
    let account = account();
    s.store.set_account(&account, "Night Shift", ME)?;
    let look = s.look(super::png::Art::Badge);
    let icon = s.photo(
        &account,
        "tiny-night-shift-icon",
        MediaKind::Avatar,
        "icon-night-shift.webp",
        look,
        128,
        128,
    )?;
    let icon_path = crate::blobs::path_of(&s.media, &account, &icon.id)
        .to_string_lossy()
        .into_owned();
    let me = Who {
        id: ME.to_owned(),
        name: "Robin Vale".to_owned(),
        avatar: None,
        bot: false,
        me: true,
    };
    s.myself(&account, &me)?;

    let members = vec![
        member(
            OWNER,
            "Mara Okafor",
            "mara@nightshift.example",
            "2025-11-02T18:00:00Z",
            &[],
            true,
            None,
        ),
        member(
            ME,
            "Robin Vale",
            "robin@nightshift.example",
            "2025-11-02T18:20:00Z",
            &[ADMIN],
            false,
            None,
        ),
        member(
            DEV,
            "Dev Patel",
            "dev@nightshift.example",
            "2025-11-03T20:11:00Z",
            &[MODERATOR, RAID],
            false,
            None,
        ),
        member(
            LEILA,
            "Leila Haddad",
            "leila@nightshift.example",
            "2025-12-14T09:30:00Z",
            &[MODERATOR],
            false,
            None,
        ),
        member(
            THEO,
            "Theo Marchetti",
            "theo@nightshift.example",
            "2026-01-08T21:45:00Z",
            &[RAID],
            false,
            None,
        ),
        member(
            SAM,
            "Sam Achterberg",
            "sam@nightshift.example",
            "2026-02-19T19:05:00Z",
            &[RAID],
            false,
            None,
        ),
        member(
            LENA,
            "Lena Brandt",
            "lena@nightshift.example",
            "2026-04-02T17:40:00Z",
            &[],
            false,
            None,
        ),
        TinyMember {
            joined_via: Some("groups".to_owned()),
            ..member(
                KOFI,
                "Kofi Asante",
                "kofi@nightshift.example",
                "2026-06-21T22:10:00Z",
                &[],
                false,
                None,
            )
        },
        member(
            INES,
            "Ines Marchetti",
            "ines@nightshift.example",
            "2026-07-30T16:00:00Z",
            &[RAID],
            false,
            Some("2026-10-27T23:52:00Z"),
        ),
        TinyMember {
            joined_via: Some("groups".to_owned()),
            ..member(
                PRIYA,
                "Priya Raman",
                "priya@nightshift.example",
                "2026-08-12T12:30:00Z",
                &[],
                false,
                Some("2026-10-21T19:15:00Z"),
            )
        },
        TinyMember {
            timeout_until: Some(from_now(6 * 24)),
            ..member(
                PAVEL,
                "Pavel Orlov",
                "pavel@nightshift.example",
                "2026-09-03T20:20:00Z",
                &[],
                false,
                Some("2026-10-28T08:03:00Z"),
            )
        },
    ];

    let moderation = TINY_KICK_MEMBERS
        | TINY_TIMEOUT_MEMBERS
        | TINY_MUTE_MEMBERS
        | TINY_DEAFEN_MEMBERS
        | TINY_MOVE_MEMBERS
        | TINY_DISCONNECT_MEMBERS
        | TINY_VIEW_AUDIT_LOG
        | TINY_CREATE_INVITES
        | TINY_PRIORITY_SPEAKER;
    let roles = vec![
        role(ADMIN, "Admin", Some("#E5484D"), 3, TINY_ALL, true, 1),
        role(
            MODERATOR,
            "Moderator",
            Some("#3FBF7A"),
            2,
            TINY_EVERYONE_DEFAULT | moderation | TINY_BAN_MEMBERS,
            true,
            2,
        ),
        role(
            RAID,
            "Raid crew",
            Some("#A97CF8"),
            1,
            TINY_EVERYONE_DEFAULT | TINY_PRIORITY_SPEAKER,
            false,
            4,
        ),
        role(
            EVERYONE,
            "@everyone",
            None,
            0,
            TINY_EVERYONE_DEFAULT,
            false,
            11,
        ),
    ];
    let categories = vec![
        TinyCategory {
            id: "cat-hangout".into(),
            name: "Hangout".into(),
            position: 0,
        },
        TinyCategory {
            id: "cat-games".into(),
            name: "Games".into(),
            position: 1,
        },
        TinyCategory {
            id: "cat-away".into(),
            name: "Away".into(),
            position: 2,
        },
    ];
    let mut lounge = room(
        "room-lounge",
        "Lounge",
        "cat-hangout",
        0,
        "Come say hi. Music on Fridays.",
        12,
        64,
        false,
    );
    lounge.participants = vec![
        TinySeat {
            speaking: true,
            ..seat(OWNER)
        },
        seat(ME),
        TinySeat {
            muted: true,
            ..seat(THEO)
        },
        TinySeat {
            muted: true,
            deafened: true,
            ..seat(LENA)
        },
    ];
    let mut music = room(
        "room-music",
        "Music room",
        "cat-hangout",
        1,
        "High bitrate for listening together.",
        0,
        128,
        false,
    );
    music.participants = vec![seat(LEILA)];
    let mut raid = room(
        "room-raid",
        "Raid night",
        "cat-games",
        0,
        "Tuesday raid. Push to talk, please.",
        8,
        96,
        true,
    );
    raid.participants = vec![
        TinySeat {
            speaking: true,
            priority: true,
            ..seat(DEV)
        },
        TinySeat {
            server_muted: true,
            ..seat(SAM)
        },
        TinySeat {
            suppressed: true,
            ..seat(KOFI)
        },
    ];
    raid.overrides = vec![
        TinyOverride {
            role_id: EVERYONE.into(),
            allow: 0,
            deny: TINY_VOICE_ACTIVITY,
        },
        TinyOverride {
            role_id: RAID.into(),
            allow: TINY_PRIORITY_SPEAKER,
            deny: 0,
        },
    ];
    music.overrides = vec![
        TinyOverride {
            role_id: EVERYONE.into(),
            allow: 0,
            deny: TINY_SPEAK,
        },
        TinyOverride {
            role_id: MODERATOR.into(),
            allow: TINY_SPEAK,
            deny: 0,
        },
    ];
    let squad = room("room-squad", "Squad 2", "cat-games", 1, "", 4, 64, false);
    let mut afk = room(
        "room-afk",
        "AFK",
        "cat-away",
        0,
        "Quiet. People who stepped away land here.",
        0,
        32,
        false,
    );
    afk.overrides = vec![TinyOverride {
        role_id: EVERYONE.into(),
        allow: 0,
        deny: TINY_SPEAK,
    }];

    let community = TinyCommunity {
        role: "admin".to_owned(),
        members,
        invites: vec![
            TinyInvite {
                id: "inv-1".into(),
                role: "member".into(),
                created_by: ME.into(),
                created_by_name: "Robin Vale".into(),
                expires_at: from_now(5 * 24),
                label: "For Kofi's brother".into(),
                max_uses: Some(1),
                ..TinyInvite::default()
            },
            TinyInvite {
                id: "inv-2".into(),
                role: "member".into(),
                created_by: OWNER.into(),
                created_by_name: "Mara Okafor".into(),
                expires_at: from_now(32 * 24),
                label: "Raid recruits".into(),
                max_uses: Some(10),
                uses: 3,
                role_ids: vec![RAID.into()],
                ..TinyInvite::default()
            },
            TinyInvite {
                id: "inv-3".into(),
                role: "member".into(),
                created_by: LEILA.into(),
                created_by_name: "Leila Haddad".into(),
                expires_at: from_now(11),
                label: "Friday music night".into(),
                max_uses: Some(25),
                uses: 7,
                accepted_at: Some("2026-10-27T20:41:00Z".into()),
                ..TinyInvite::default()
            },
            TinyInvite {
                id: "inv-setup".into(),
                role: "owner".into(),
                expires_at: from_now(11 * 24),
                label: "Owner".into(),
                max_uses: Some(1),
                ..TinyInvite::default()
            },
        ],
        audit: audit_rows(),
        origin: ORIGIN.to_owned(),
        rooms: vec![lounge, music, raid, squad, afk],
        online: vec![OWNER, ME, DEV, LEILA, THEO, SAM, LENA, KOFI].into_iter().map(str::to_owned).collect(),
        media: "up".to_owned(),
        capabilities: TinyCapabilities {
            version: "0.4.0".to_owned(),
            features: ["server", "roles", "categories", "bans", "timeouts", "invite_options", "audit_query", "voice_moderation", "upload_limits"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
        },
        access: Some(TinyAccess { owner: false, roles: vec![ADMIN.into()], permissions: TINY_ALL, rank: 3 }),
        me: ME.to_owned(),
        server: Some(TinyServerInfo {
            name: "Night Shift".to_owned(),
            description: "A late-night voice hangout for a few friends who game, make music and stay up too late.".to_owned(),
            welcome: "Welcome in. Grab a seat in the Lounge, say hi, and use push to talk in Raid night. Be kind; the mods are friendly.".to_owned(),
            icon: Some("d3m0".to_owned()),
            default_room_id: Some("room-lounge".to_owned()),
            afk_room_id: Some("room-afk".to_owned()),
            afk_timeout_secs: 900,
            image_max_bytes: protocol::TINY_IMAGE_MAX_DEFAULT,
            file_max_bytes: protocol::TINY_FILE_MAX_DEFAULT,
            created_at: "2025-11-02T18:00:00Z".to_owned(),
            icon_path,
        }),
        roles,
        categories,
        bans: vec![
            TinyBan {
                user_id: "tiny-gone".into(),
                display_name: "Rook".into(),
                email: "rook@elsewhere.example".into(),
                reason: "Harassment in voice".into(),
                banned_by: Some(OWNER.into()),
                banned_by_name: "Mara Okafor".into(),
                created_at: "2026-09-30T22:00:00Z".into(),
                expires_at: None,
            },
            TinyBan {
                user_id: "tiny-spam".into(),
                display_name: "freecoins_4u".into(),
                email: "promo@spam.example".into(),
                reason: "Posted the same link in every channel".into(),
                banned_by: Some(DEV.into()),
                banned_by_name: "Dev Patel".into(),
                created_at: "2026-10-12T03:15:00Z".into(),
                expires_at: None,
            },
            TinyBan {
                user_id: "tiny-hot".into(),
                display_name: "Jonas Weber".into(),
                email: "jonas@elsewhere.example".into(),
                reason: "Cooling off after the raid argument".into(),
                banned_by: Some(ME.into()),
                banned_by_name: "Robin Vale".into(),
                created_at: "2026-10-25T23:40:00Z".into(),
                expires_at: Some(from_now(3 * 24)),
            },
        ],
        provider: "Pocket ID".to_owned(),
    };
    *PAGE.lock().unwrap() = Some(community);
    Ok(me)
}

/// The audit as the demo's server kept it, newest first: the setup's three rows (the owner
/// invite was printed three times), then weeks of a server being run.
fn audit_rows() -> Vec<TinyAudit> {
    vec![
        audit(
            30,
            ME,
            "Robin Vale",
            "member.timeout",
            Some((PAVEL, "Pavel Orlov")),
            "2026-10-28T08:04:00Z",
            "Spamming the raid channel",
        ),
        audit(
            29,
            DEV,
            "Dev Patel",
            "call.server_mute",
            Some((SAM, "Sam Achterberg")),
            "2026-10-27T21:12:00Z",
            "Open mic",
        ),
        detailed(
            audit(
                28,
                DEV,
                "Dev Patel",
                "call.move",
                Some((THEO, "Theo Marchetti")),
                "2026-10-27T20:58:00Z",
                "",
            ),
            r#"{"room_id":"room-raid"}"#,
        ),
        detailed(
            audit(
                27,
                LEILA,
                "Leila Haddad",
                "invite.create",
                None,
                "2026-10-26T19:02:00Z",
                "",
            ),
            r#"{"label":"Friday music night"}"#,
        ),
        audit(
            26,
            ME,
            "Robin Vale",
            "member.ban",
            Some(("tiny-hot", "Jonas Weber")),
            "2026-10-25T23:40:00Z",
            "Cooling off after the raid argument",
        ),
        detailed(
            audit(
                25,
                OWNER,
                "Mara Okafor",
                "room.override",
                None,
                "2026-10-24T18:30:00Z",
                "",
            ),
            r#"{"name":"Music room"}"#,
        ),
        detailed(
            audit(
                24,
                OWNER,
                "Mara Okafor",
                "room.update",
                None,
                "2026-10-24T18:28:00Z",
                "",
            ),
            r#"{"name":"Music room"}"#,
        ),
        audit(
            23,
            OWNER,
            "Mara Okafor",
            "member.roles",
            Some((LEILA, "Leila Haddad")),
            "2026-10-20T17:00:00Z",
            "",
        ),
        audit(
            22,
            OWNER,
            "Mara Okafor",
            "server.update",
            None,
            "2026-10-19T15:22:00Z",
            "",
        ),
        audit(
            21,
            OWNER,
            "Mara Okafor",
            "server.icon",
            None,
            "2026-10-19T15:20:00Z",
            "",
        ),
        audit(
            20,
            DEV,
            "Dev Patel",
            "member.ban",
            Some(("tiny-spam", "freecoins_4u")),
            "2026-10-12T03:15:00Z",
            "Posted the same link in every channel",
        ),
        audit(
            19,
            OWNER,
            "Mara Okafor",
            "member.ban",
            Some(("tiny-gone", "Rook")),
            "2026-09-30T22:00:00Z",
            "Harassment in voice",
        ),
        audit(
            18,
            PAVEL,
            "Pavel Orlov",
            "invite.accept",
            Some((PAVEL, "Pavel Orlov")),
            "2026-09-03T20:20:00Z",
            "",
        ),
        audit(
            17,
            OWNER,
            "Mara Okafor",
            "layout.update",
            None,
            "2026-08-20T21:00:00Z",
            "",
        ),
        detailed(
            audit(
                16,
                OWNER,
                "Mara Okafor",
                "category.create",
                None,
                "2026-08-20T20:58:00Z",
                "",
            ),
            r#"{"name":"Away"}"#,
        ),
        detailed(
            audit(
                15,
                OWNER,
                "Mara Okafor",
                "role.update",
                None,
                "2026-08-02T19:10:00Z",
                "",
            ),
            r#"{"name":"Moderator"}"#,
        ),
        audit(
            14,
            OWNER,
            "Mara Okafor",
            "member.roles",
            Some((DEV, "Dev Patel")),
            "2026-07-01T18:00:00Z",
            "",
        ),
        audit(
            13,
            OWNER,
            "Mara Okafor",
            "role.order",
            None,
            "2026-06-30T22:14:00Z",
            "",
        ),
        detailed(
            audit(
                12,
                OWNER,
                "Mara Okafor",
                "role.create",
                None,
                "2026-06-30T22:12:00Z",
                "",
            ),
            r#"{"name":"Raid crew"}"#,
        ),
        detailed(
            audit(
                11,
                OWNER,
                "Mara Okafor",
                "role.create",
                None,
                "2026-06-30T22:10:00Z",
                "",
            ),
            r#"{"name":"Moderator"}"#,
        ),
        detailed(
            audit(
                10,
                OWNER,
                "Mara Okafor",
                "room.create",
                None,
                "2026-01-08T20:00:00Z",
                "",
            ),
            r#"{"name":"Raid night"}"#,
        ),
        detailed(
            audit(
                9,
                OWNER,
                "Mara Okafor",
                "room.create",
                None,
                "2025-11-02T18:40:00Z",
                "",
            ),
            r#"{"name":"Music room"}"#,
        ),
        detailed(
            audit(
                8,
                OWNER,
                "Mara Okafor",
                "room.create",
                None,
                "2025-11-02T18:31:00Z",
                "",
            ),
            r#"{"name":"Lounge"}"#,
        ),
        audit(
            7,
            OWNER,
            "Mara Okafor",
            "member.roles",
            Some((ME, "Robin Vale")),
            "2025-11-02T18:25:00Z",
            "",
        ),
        audit(
            6,
            ME,
            "Robin Vale",
            "invite.accept",
            Some((ME, "Robin Vale")),
            "2025-11-02T18:20:00Z",
            "",
        ),
        detailed(
            audit(
                5,
                OWNER,
                "Mara Okafor",
                "invite.create",
                None,
                "2025-11-02T18:10:00Z",
                "",
            ),
            r#"{"label":"Robin"}"#,
        ),
        audit(
            4,
            OWNER,
            "Mara Okafor",
            "invite.accept",
            Some((OWNER, "Mara Okafor")),
            "2025-11-02T18:00:00Z",
            "",
        ),
        by_setup(3, "bootstrap", "2025-11-02T17:58:00Z"),
        by_setup(2, "bootstrap", "2025-11-02T17:55:00Z"),
        by_setup(1, "bootstrap", "2025-11-02T17:50:00Z"),
    ]
}

/// One page of the audit as the server reads it: newest first, after `before`, by `actor`, of
/// actions starting with `action`, at most `limit`.
fn audit_page(rows: &[TinyAudit], filter: &protocol::TinyAuditFilter) -> Vec<TinyAudit> {
    let limit = match filter.limit {
        0 => 50,
        n => n.min(200),
    } as usize;
    rows.iter()
        .filter(|row| filter.before.is_none_or(|before| row.id < before))
        .filter(|row| filter.actor.is_none() || row.actor_id == filter.actor)
        .filter(|row| row.action.starts_with(&filter.action))
        .take(limit)
        .cloned()
        .collect()
}

fn apply_role(role: &mut TinyRole, edit: &TinyRoleEdit) {
    if let Some(name) = &edit.name {
        role.name = name.clone();
    }
    if let Some(color) = &edit.color {
        role.color = Some(color.clone()).filter(|color| !color.is_empty());
    }
    if let Some(bits) = edit.permissions {
        role.permissions = bits;
    }
    if let Some(hoist) = edit.hoist {
        role.hoist = hoist;
    }
}

/// What the demo says to a Tiny request for its own account. `None` for anything else, which
/// goes on to the real server's code.
pub fn answer(hub: &Hub, request: &Request) -> Option<Response> {
    let mut page = PAGE.lock().unwrap();
    let community = page.as_mut()?;
    let account = account();
    let ours = |key: &AccountKey| *key == account;
    let changed = |_: &mut TinyCommunity| {
        hub.changed(Change::Tiny {
            account: account.clone(),
        });
        Some(Response::Ok)
    };
    match request {
        Request::TinyCommunity { account } if ours(account) => Some(Response::TinyCommunity {
            community: community.clone(),
        }),
        Request::TinyInvite { account, role } if ours(account) => Some(code(role)),
        Request::TinyInviteWith { account, .. } if ours(account) => Some(code("member")),
        Request::TinyCallJoin { account, room_id } if ours(account) => {
            for room in &mut community.rooms {
                room.participants.retain(|seat| seat.user_id != ME);
            }
            if let Some(room) = community.rooms.iter_mut().find(|room| room.id == *room_id) {
                room.participants.push(seat(ME));
            }
            changed(community)
        }
        Request::TinyCallLeave { account } if ours(account) => {
            for room in &mut community.rooms {
                room.participants.retain(|seat| seat.user_id != ME);
            }
            changed(community)
        }
        Request::TinyCallKick {
            account, user_id, ..
        } if ours(account) => {
            for room in &mut community.rooms {
                room.participants.retain(|seat| seat.user_id != *user_id);
            }
            changed(community)
        }
        Request::TinyCallModerate {
            account,
            room_id,
            user_id,
            server_muted,
            server_deafened,
        } if ours(account) => {
            let seat = community
                .rooms
                .iter_mut()
                .filter(|room| room.id == *room_id)
                .flat_map(|room| room.participants.iter_mut())
                .find(|seat| seat.user_id == *user_id);
            if let Some(seat) = seat {
                seat.server_muted = server_muted.unwrap_or(seat.server_muted);
                seat.server_deafened = server_deafened.unwrap_or(seat.server_deafened);
            }
            changed(community)
        }
        Request::TinyCallMove {
            account,
            user_id,
            room_id,
        } if ours(account) => {
            let mut moved = None;
            for room in &mut community.rooms {
                if let Some(at) = room
                    .participants
                    .iter()
                    .position(|seat| seat.user_id == *user_id)
                {
                    moved = Some(room.participants.remove(at));
                }
            }
            if let (Some(seat), Some(room)) = (
                moved,
                community.rooms.iter_mut().find(|room| room.id == *room_id),
            ) {
                room.participants.push(seat);
            }
            changed(community)
        }
        Request::TinyServerUpdate { account, edit } if ours(account) => {
            if let Some(server) = community.server.as_mut() {
                if let Some(name) = &edit.name {
                    server.name = name.clone();
                }
                if let Some(description) = &edit.description {
                    server.description = description.clone();
                }
                if let Some(welcome) = &edit.welcome {
                    server.welcome = welcome.clone();
                }
                if let Some(room) = &edit.default_room_id {
                    server.default_room_id = Some(room.clone()).filter(|room| !room.is_empty());
                }
                if let Some(room) = &edit.afk_room_id {
                    server.afk_room_id = Some(room.clone()).filter(|room| !room.is_empty());
                }
                if let Some(secs) = edit.afk_timeout_secs {
                    server.afk_timeout_secs = secs;
                }
                if let Some(bytes) = edit.image_max_bytes {
                    server.image_max_bytes = bytes;
                }
                if let Some(bytes) = edit.file_max_bytes {
                    server.file_max_bytes = bytes;
                }
            }
            changed(community)
        }
        Request::TinyMemberRoles {
            account,
            user_id,
            role_ids,
        } if ours(account) => {
            if let Some(member) = community
                .members
                .iter_mut()
                .find(|member| member.user_id == *user_id)
            {
                member.roles = role_ids.clone();
            }
            changed(community)
        }
        Request::TinyTimeout {
            account,
            user_id,
            until,
            ..
        } if ours(account) => {
            if let Some(member) = community
                .members
                .iter_mut()
                .find(|member| member.user_id == *user_id)
            {
                member.timeout_until = until.clone();
            }
            changed(community)
        }
        Request::TinyKick {
            account, user_id, ..
        }
        | Request::TinyBan {
            account, user_id, ..
        }
        | Request::TinyRemove { account, user_id }
            if ours(account) =>
        {
            community
                .members
                .retain(|member| member.user_id != *user_id);
            for room in &mut community.rooms {
                room.participants.retain(|seat| seat.user_id != *user_id);
            }
            changed(community)
        }
        Request::TinyLayout { account, groups } if ours(account) => {
            for group in groups {
                for (position, room_id) in group.room_ids.iter().enumerate() {
                    if let Some(room) = community.rooms.iter_mut().find(|room| room.id == *room_id)
                    {
                        room.category_id = group.category_id.clone();
                        room.position = position as i32;
                    }
                }
            }
            for (position, group) in groups
                .iter()
                .filter_map(|group| group.category_id.as_ref())
                .enumerate()
            {
                if let Some(category) = community
                    .categories
                    .iter_mut()
                    .find(|category| category.id == *group)
                {
                    category.position = position as i32;
                }
            }
            changed(community)
        }
        Request::TinyCategoryCreate { account, name } if ours(account) => {
            let id = format!("cat-{}", community.categories.len() + 1);
            let position = community.categories.len() as i32;
            community.categories.push(TinyCategory {
                id,
                name: name.clone(),
                position,
            });
            changed(community)
        }
        Request::TinyRoomCreate { account, name } if ours(account) => {
            let id = format!("room-{}", community.rooms.len() + 1);
            community.rooms.push(TinyRoom::new(&id, name));
            changed(community)
        }
        Request::TinyRoomCreateWith { account, room } if ours(account) => {
            let id = format!("room-{}", community.rooms.len() + 1);
            community.rooms.push(TinyRoom {
                category_id: room.category_id.clone(),
                ..TinyRoom::new(&id, room.name.as_deref().unwrap_or("New channel"))
            });
            changed(community)
        }
        Request::TinyRoomDelete { account, room_id } if ours(account) => {
            community.rooms.retain(|room| room.id != *room_id);
            changed(community)
        }
        Request::TinyRoomRename {
            account,
            room_id,
            name,
        } if ours(account) => {
            if let Some(room) = community.rooms.iter_mut().find(|room| room.id == *room_id) {
                room.name = name.clone();
            }
            changed(community)
        }
        Request::TinyRoomUpdate {
            account,
            room_id,
            room: edit,
        } if ours(account) => {
            if let Some(room) = community.rooms.iter_mut().find(|room| room.id == *room_id) {
                if let Some(name) = &edit.name {
                    room.name = name.clone();
                }
                if let Some(topic) = &edit.topic {
                    room.topic = topic.clone();
                }
                if let Some(limit) = edit.user_limit {
                    room.user_limit = limit;
                }
                if let Some(kbps) = edit.bitrate_kbps {
                    room.bitrate_kbps = kbps;
                }
                if let Some(ptt) = edit.ptt_only {
                    room.ptt_only = ptt;
                }
                if let Some(video) = edit.video_enabled {
                    room.video_enabled = video;
                }
                if let Some(e2ee) = edit.e2ee_required {
                    room.e2ee_required = e2ee;
                }
            }
            changed(community)
        }
        Request::TinyOverrideSet {
            account,
            room_id,
            role_id,
            allow,
            deny,
        } if ours(account) => {
            if let Some(room) = community.rooms.iter_mut().find(|room| room.id == *room_id) {
                room.overrides.retain(|over| over.role_id != *role_id);
                room.overrides.push(TinyOverride {
                    role_id: role_id.clone(),
                    allow: *allow,
                    deny: *deny,
                });
            }
            changed(community)
        }
        Request::TinyOverrideClear {
            account,
            room_id,
            role_id,
        } if ours(account) => {
            if let Some(room) = community.rooms.iter_mut().find(|room| room.id == *room_id) {
                room.overrides.retain(|over| over.role_id != *role_id);
            }
            changed(community)
        }
        Request::TinyCategoryRename {
            account,
            category_id,
            name,
        } if ours(account) => {
            if let Some(category) = community
                .categories
                .iter_mut()
                .find(|category| category.id == *category_id)
            {
                category.name = name.clone();
            }
            changed(community)
        }
        Request::TinyCategoryDelete {
            account,
            category_id,
        } if ours(account) => {
            community
                .categories
                .retain(|category| category.id != *category_id);
            for room in &mut community.rooms {
                if room.category_id.as_deref() == Some(category_id.as_str()) {
                    room.category_id = None;
                }
            }
            changed(community)
        }
        Request::TinyRoleCreate { account, role } if ours(account) => {
            let id = format!("role-{}", community.roles.len() + 1);
            // Just below our own rank, as the server places it.
            let rank = community.access.as_ref().map_or(1, |access| access.rank);
            for other in &mut community.roles {
                if !other.default && other.position >= rank {
                    other.position += 1;
                }
            }
            let mut made = TinyRole {
                id,
                position: rank,
                ..TinyRole::default()
            };
            apply_role(&mut made, role);
            community.roles.push(made);
            if let Some(access) = community.access.as_mut() {
                access.rank += 1;
            }
            changed(community)
        }
        Request::TinyRoleUpdate {
            account,
            role_id,
            role,
        } if ours(account) => {
            if let Some(known) = community
                .roles
                .iter_mut()
                .find(|known| known.id == *role_id)
            {
                apply_role(known, role);
            }
            changed(community)
        }
        Request::TinyRoleDelete { account, role_id } if ours(account) => {
            community
                .roles
                .retain(|role| role.id != *role_id || role.default);
            for member in &mut community.members {
                member.roles.retain(|id| id != role_id);
            }
            changed(community)
        }
        Request::TinyRoleOrder { account, role_ids } if ours(account) => {
            let top = role_ids.len() as i32;
            for (ix, id) in role_ids.iter().enumerate() {
                if let Some(role) = community.roles.iter_mut().find(|role| role.id == *id) {
                    role.position = top - ix as i32;
                }
            }
            changed(community)
        }
        Request::TinyMemberOwner {
            account,
            user_id,
            owner,
        } if ours(account) => {
            if let Some(member) = community
                .members
                .iter_mut()
                .find(|member| member.user_id == *user_id)
            {
                member.owner = *owner;
            }
            if user_id == ME {
                if let Some(access) = community.access.as_mut() {
                    access.owner = *owner;
                }
            }
            changed(community)
        }
        Request::TinyUnban { account, user_id } if ours(account) => {
            community.bans.retain(|ban| ban.user_id != *user_id);
            changed(community)
        }
        Request::TinyRevoke { account, invite_id } if ours(account) => {
            community.invites.retain(|invite| invite.id != *invite_id);
            changed(community)
        }
        // Anything else asked of the demo server is taken and changes nothing.
        Request::TinyRole { account, .. }
        | Request::TinyRename { account, .. }
        | Request::TinyServerIcon { account, .. }
        | Request::TinyAvatar { account, .. }
            if ours(account) =>
        {
            Some(Response::Ok)
        }
        Request::TinyAudit { account, filter } if ours(account) => Some(Response::TinyAudit {
            audit: audit_page(&community.audit, filter),
        }),
        _ => None,
    }
}

fn code(role: &str) -> Response {
    Response::TinyCode {
        code: TinyCode {
            id: "inv-demo".to_owned(),
            token: "Nf3kQ8xLw2pZ7rT1".to_owned(),
            role: role.to_owned(),
            expires_at: from_now(7 * 24),
            origin: ORIGIN.to_owned(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_audit_pages_as_the_server_does() {
        let rows = audit_rows();
        let filter = |before, actor: Option<&str>, action: &str, limit| protocol::TinyAuditFilter {
            limit,
            before,
            actor: actor.map(str::to_owned),
            action: action.to_owned(),
        };
        let first = audit_page(&rows, &filter(None, None, "", 10));
        assert_eq!(first.len(), 10);
        let next = audit_page(&rows, &filter(first.last().map(|row| row.id), None, "", 10));
        assert!(next.iter().all(|row| row.id < first[9].id));
        assert!(audit_page(&rows, &filter(None, Some(DEV), "", 50))
            .iter()
            .all(|row| row.actor_id.as_deref() == Some(DEV)));
        assert!(audit_page(&rows, &filter(None, None, "role.", 50))
            .iter()
            .all(|row| row.action.starts_with("role.")));
    }
}
