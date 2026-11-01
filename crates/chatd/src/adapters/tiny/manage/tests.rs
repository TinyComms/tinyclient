//! Server management against a local fake tinyserver: what the page reads from a newer server
//! and an older one, what each change sends, and what the server's refusals say.

use super::super::tests::{http, json_body, serve, test_hub, with_session};
use super::super::{community, may};
use super::*;
use protocol::{ErrorCode, TinyInviteOptions, TINY_EVERYONE_DEFAULT, TINY_KICK_MEMBERS};
use std::sync::{Arc, Mutex};

const ME: &str = "11111111-1111-1111-1111-111111111111";
const BEA: &str = "22222222-2222-2222-2222-222222222222";
const LOBBY: &str = "33333333-3333-3333-3333-333333333333";
const MODS: &str = "55555555-5555-5555-5555-555555555555";
const VOICE: &str = "66666666-6666-6666-6666-666666666666";

fn said(detail: &str) -> Response {
    Response::Error { code: ErrorCode::Refused, detail: detail.to_owned() }
}

fn png() -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    bytes.extend_from_slice(b"not really a picture, but it starts like one");
    bytes
}

fn members() -> String {
    format!(
        r#"[{{"user_id":"{ME}","email":"ada@example.com","display_name":"Ada","role":"owner","created_at":"t","owner":true,"roles":[],"last_seen_at":"2026-10-28T10:00:00Z","joined_via":"invite"}},{{"user_id":"{BEA}","email":null,"display_name":"Bea","role":"member","created_at":"t","owner":false,"roles":["{MODS}"],"last_seen_at":null,"timeout_until":"2026-11-01T00:00:00Z","joined_via":"groups"}}]"#
    )
}

/// A newer tinyserver. `me` is what `/v1/me` says of this account. Every request is logged.
fn newer(me: &'static str, log: Arc<Mutex<Vec<String>>>) -> String {
    serve(move |request| {
        log.lock().unwrap().push(request.to_owned());
        let first = request.lines().next().unwrap_or("");
        let body = request.split("\r\n\r\n").nth(1).unwrap_or("");
        let route = first.split(' ').take(2).collect::<Vec<_>>().join(" ");
        match route.as_str() {
            "GET /v1/capabilities" => http(200, "OK", r#"{"version":"0.3.0","features":["server","roles","categories","bans","timeouts","invite_options","audit_query","voice_moderation","later"]}"#),
            "GET /v1/me" => http(200, "OK", me),
            "GET /v1/members" => http(200, "OK", &members()),
            "GET /v1/invites" => http(200, "OK", r#"[{"id":"i1","role":"owner","created_by":null,"created_by_name":null,"label":null,"max_uses":1,"uses":0,"role_ids":[],"expires_at":"t"}]"#),
            "GET /v1/rooms" => http(200, "OK", &format!(r#"[{{"id":"{LOBBY}","name":"Lobby","category_id":"{VOICE}","position":0,"topic":"Hi","user_limit":12,"bitrate_kbps":96,"ptt_only":true,"video_enabled":true,"overrides":[],"my_permissions":3}}]"#)),
            "GET /v1/server" => http(200, "OK", r#"{"name":"AtomicStudio","description":"","welcome":"Hello","icon":"ab12","default_room_id":null,"afk_room_id":null,"afk_timeout_secs":0,"created_at":"t"}"#),
            "GET /v1/server/icon" => {
                let picture = png();
                let mut answer = format!("HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nETag: \"ab12\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", picture.len()).into_bytes();
                answer.extend_from_slice(&picture);
                answer
            }
            "GET /v1/roles" => http(200, "OK", &format!(r##"[{{"id":"e","name":"@everyone","color":null,"position":0,"permissions":{TINY_EVERYONE_DEFAULT},"hoist":false,"default":true,"member_count":2}},{{"id":"{MODS}","name":"Mods","color":"#3B82F6","position":1,"permissions":{TINY_KICK_MEMBERS},"hoist":true,"default":false,"member_count":1}}]"##)),
            "GET /v1/categories" => http(200, "OK", &format!(r#"[{{"id":"later","name":"Later","position":2}},{{"id":"{VOICE}","name":"Voice","position":1}}]"#)),
            "GET /v1/bans" => http(200, "OK", r#"[{"user_id":"x","display_name":"Cy","email":null,"reason":"spam","banned_by":null,"banned_by_name":null,"created_at":"t","expires_at":null}]"#),
            "GET /health" => http(200, "OK", r#"{"ok":true,"media":"up"}"#),
            _ if first.starts_with("GET /v1/audit?") => http(200, "OK", r#"[{"id":9,"actor_id":null,"action":"server.update","subject":"server","detail":{"name":"x"},"created_at":"t","actor_name":"","subject_user_id":null,"subject_name":"","reason":null}]"#),
            _ if first.starts_with("DELETE /v1/members/") && first.contains(BEA) => http(403, "Forbidden", r#"{"error":"hierarchy"}"#),
            _ if body.contains("\"Taken\"") => http(409, "Conflict", r#"{"error":"default_role"}"#),
            _ if first.starts_with("POST /v1/invites ") => http(201, "Created", r#"{"id":"i2","token":"tok","role":"member","expires_at":"t"}"#),
            _ if first.starts_with("DELETE /v1/roles/gone ") => http(404, "Not Found", r#"{"error":"not_found"}"#),
            _ if first.starts_with("PUT /v1/server/icon ") && body.len() > 300 * 1024 => http(413, "Payload Too Large", r#"{"error":"too_large"}"#),
            _ if first.starts_with("DELETE ") => http(204, "No Content", ""),
            _ if first.starts_with("POST ") => http(201, "Created", "{}"),
            _ => http(200, "OK", "{}"),
        }
    })
}

const OWNER: &str = r#"{"user_id":"11111111-1111-1111-1111-111111111111","display_name":"Ada","role":"owner","owner":true,"roles":[],"permissions":1048575,"rank":2147483647}"#;

fn sent(log: &Arc<Mutex<Vec<String>>>, start: &str) -> Vec<String> {
    log.lock().unwrap().iter().filter(|request| request.starts_with(start)).cloned().collect()
}

fn last(log: &Arc<Mutex<Vec<String>>>, start: &str) -> String {
    sent(log, start).pop().unwrap_or_else(|| panic!("nothing sent like {start}"))
}

#[test]
fn a_newer_server_fills_the_page_and_its_icon_lands_on_disk() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let base = newer(OWNER, log.clone());
    with_session(&base, "owner", |account| match community(&test_hub(), account) {
        Response::TinyCommunity { community } => {
            assert!(community.has(protocol::TINY_FEATURE_BANS) && community.has("later"));
            assert!(community.access.as_ref().is_some_and(|access| access.owner));
            assert_eq!(community.me, ME);
            let server = community.server.clone().expect("the server's settings");
            assert_eq!((server.name.as_str(), server.welcome.as_str()), ("AtomicStudio", "Hello"));
            assert!(server.icon_path.contains("media"), "{}", server.icon_path);
            assert_eq!(std::fs::read(&server.icon_path).unwrap(), png());
            assert_eq!(community.roles.iter().map(|role| role.name.as_str()).collect::<Vec<_>>(), ["Mods", "@everyone"], "highest first");
            assert_eq!(community.categories.iter().map(|category| category.name.as_str()).collect::<Vec<_>>(), ["Voice", "Later"], "top first");
            assert_eq!(community.bans[0].reason, "spam");
            let bea = community.member(BEA).unwrap();
            assert_eq!((bea.roles.as_slice(), bea.joined_via.as_deref()), ([MODS.to_owned()].as_slice(), Some("groups")));
            assert!(community.outranks(bea));
            let lobby = &community.rooms[0];
            assert_eq!((lobby.bitrate(), lobby.ptt_only, lobby.category_id.as_deref()), (96, true, Some(VOICE)));
            assert_eq!(community.invites[0].max_uses, Some(1));
            assert!(community.allowed("community.server.edit") && community.allowed("community.members.owner"));
        }
        other => panic!("load: {other:?}"),
    });
    // The icon is fetched once; a second load finds it on disk. (Another test may have
    // fetched it first: they share the scratch directory.)
    let fetched = sent(&log, "GET /v1/server/icon ").len();
    assert!(fetched <= 1);
    with_session(&base, "owner", |account| drop(community(&test_hub(), account)));
    assert_eq!(sent(&log, "GET /v1/server/icon ").len(), fetched);
    let room = known(&Saved { base_url: base.clone(), token: "t".into(), user_id: ME.into(), email: None, display_name: "Ada".into(), role: "owner".into(), plain_http_ok: false, e2ee: None });
    assert_eq!(room_audio(&Saved { base_url: base, token: "t".into(), user_id: ME.into(), email: None, display_name: "Ada".into(), role: "owner".into(), plain_http_ok: false, e2ee: None }, LOBBY), (96, true));
    assert_eq!(room.rooms.len(), 1);
}

#[test]
fn every_change_goes_to_its_route_with_its_body() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let base = newer(OWNER, log.clone());
    with_session(&base, "owner", |account| {
        let edit = TinyServerEdit { name: Some("  Atomic  ".into()), default_room_id: Some(String::new()), afk_timeout_secs: Some(300), ..TinyServerEdit::default() };
        assert_eq!(server_update(account.clone(), edit), Response::Ok);
        let body = json_body(&last(&log, "PATCH /v1/server "));
        assert_eq!((body["name"].as_str(), body["default_room_id"].is_null(), body["afk_timeout_secs"].as_i64()), (Some("Atomic"), true, Some(300)));
        assert!(body.get("welcome").is_none(), "only what changed is sent");

        let role = TinyRoleEdit { name: Some("DJ".into()), color: Some("#22C55E".into()), permissions: Some(TINY_EVERYONE_DEFAULT), hoist: Some(true) };
        assert_eq!(role_create(account.clone(), role), Response::Ok);
        assert_eq!(json_body(&last(&log, "POST /v1/roles "))["color"], "#22C55E");
        assert_eq!(role_update(account.clone(), MODS.into(), TinyRoleEdit { color: Some(String::new()), ..TinyRoleEdit::default() }), Response::Ok);
        assert!(json_body(&last(&log, &format!("PATCH /v1/roles/{MODS} ")))["color"].is_null());
        assert_eq!(role_delete(account.clone(), "gone".into()), said("That role is gone."));
        assert_eq!(role_order(account.clone(), vec![MODS.into()]), Response::Ok);
        assert_eq!(json_body(&last(&log, "PUT /v1/roles/order "))["ids"][0], MODS);
        assert_eq!(role_update(account.clone(), "e".into(), TinyRoleEdit { name: Some("Taken".into()), ..TinyRoleEdit::default() }), said("@everyone can't be deleted or moved."));

        assert_eq!(member_roles(account.clone(), BEA.into(), vec![MODS.into()]), Response::Ok);
        assert_eq!(json_body(&last(&log, &format!("PUT /v1/members/{BEA}/roles ")))["role_ids"][0], MODS);
        assert_eq!(member_owner(account.clone(), BEA.into(), true), Response::Ok);
        assert_eq!(timeout(account.clone(), BEA.into(), Some("2026-11-01T00:00:00Z".into()), " noise ".into()), Response::Ok);
        let body = json_body(&last(&log, &format!("PUT /v1/members/{BEA}/timeout ")));
        assert_eq!((body["until"].as_str(), body["reason"].as_str()), (Some("2026-11-01T00:00:00Z"), Some("noise")));
        assert_eq!(timeout(account.clone(), BEA.into(), None, String::new()), Response::Ok);
        assert!(json_body(&last(&log, &format!("PUT /v1/members/{BEA}/timeout ")))["until"].is_null());
        assert_eq!(kick(account.clone(), "44444444".into(), "too loud & rude".into()), Response::Ok);
        assert!(last(&log, "DELETE /v1/members/").starts_with("DELETE /v1/members/44444444?reason=too%20loud%20%26%20rude "));
        assert_eq!(kick(account.clone(), BEA.into(), String::new()), said(crate::adapters::tiny::HIERARCHY));

        assert_eq!(ban(account.clone(), BEA.into(), "spam".into(), None), Response::Ok);
        let body = json_body(&last(&log, "POST /v1/bans "));
        assert_eq!((body["user_id"].as_str(), body["reason"].as_str(), body.get("expires_at")), (Some(BEA), Some("spam"), None));
        assert_eq!(unban(account.clone(), BEA.into()), Response::Ok);
        assert!(!sent(&log, &format!("DELETE /v1/bans/{BEA} ")).is_empty());

        assert_eq!(category_create(account.clone(), " Voice ".into()), Response::Ok);
        assert_eq!(json_body(&last(&log, "POST /v1/categories "))["name"], "Voice");
        assert_eq!(category_rename(account.clone(), VOICE.into(), "Talk".into()), Response::Ok);
        assert_eq!(category_delete(account.clone(), VOICE.into()), Response::Ok);
        let groups = vec![TinyLayoutGroup { category_id: None, room_ids: vec![LOBBY.into()] }, TinyLayoutGroup { category_id: Some(VOICE.into()), room_ids: vec![] }];
        assert_eq!(layout(account.clone(), groups), Response::Ok);
        let body = json_body(&last(&log, "PUT /v1/layout "));
        assert!(body["categories"][0]["id"].is_null() && body["categories"][1]["id"] == VOICE);

        let room = TinyRoomEdit { name: Some("Gaming".into()), bitrate_kbps: Some(128), ptt_only: Some(true), category_id: Some(VOICE.into()), ..TinyRoomEdit::default() };
        assert_eq!(room_create(account.clone(), room), Response::Ok);
        let body = json_body(&last(&log, "POST /v1/rooms "));
        assert_eq!((body["bitrate_kbps"].as_u64(), body["ptt_only"].as_bool()), (Some(128), Some(true)));
        assert_eq!(room_update(account.clone(), LOBBY.into(), TinyRoomEdit { category_id: Some(String::new()), user_limit: Some(5), ..TinyRoomEdit::default() }), Response::Ok);
        assert!(json_body(&last(&log, &format!("PATCH /v1/rooms/{LOBBY} ")))["category_id"].is_null());
        assert_eq!(override_set(account.clone(), LOBBY.into(), MODS.into(), 4, 1), Response::Ok);
        assert_eq!(json_body(&last(&log, &format!("PUT /v1/rooms/{LOBBY}/overrides/{MODS} ")))["deny"], 1);
        assert_eq!(override_clear(account.clone(), LOBBY.into(), MODS.into()), Response::Ok);

        let options = TinyInviteOptions { role_ids: vec![MODS.into()], label: "for Karlis".into(), max_uses: Some(5), expires_in_secs: None, default_expiry: false };
        match invite_with(account.clone(), options) {
            Response::TinyCode { code } => assert_eq!((code.token.as_str(), code.origin.as_str()), ("tok", base.as_str())),
            other => panic!("invite: {other:?}"),
        }
        let body = json_body(&last(&log, "POST /v1/invites "));
        assert!(body["expires_in_secs"].is_null(), "never expires: sent as null");
        assert_eq!((body["label"].as_str(), body["max_uses"].as_u64()), (Some("for Karlis"), Some(5)));
        assert!(invite_with(account.clone(), TinyInviteOptions { default_expiry: true, ..TinyInviteOptions::default() }) != said(CHECK));
        assert!(json_body(&last(&log, "POST /v1/invites ")).get("expires_in_secs").is_none(), "the server's default");

        let filter = TinyAuditFilter { limit: 500, before: Some(40), actor: Some(BEA.into()), action: "member.".into() };
        match audit(account.clone(), filter) {
            Response::TinyAudit { audit } => assert_eq!((audit[0].id, audit[0].detail.as_str(), audit[0].reason.as_str()), (9, r#"{"name":"x"}"#, "")),
            other => panic!("audit: {other:?}"),
        }
        assert!(last(&log, "GET /v1/audit?").starts_with(&format!("GET /v1/audit?limit=200&before=40&actor={BEA}&action=member. ")));
    });
}

#[test]
fn bad_shapes_are_refused_before_anything_is_sent() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let base = newer(OWNER, log.clone());
    with_session(&base, "owner", |account| {
        drop(community(&test_hub(), account.clone()));
        let before = log.lock().unwrap().len();
        assert_eq!(server_update(account.clone(), TinyServerEdit { name: Some("  ".into()), ..TinyServerEdit::default() }), said(SERVER_NAME_SIZE));
        assert_eq!(server_update(account.clone(), TinyServerEdit { welcome: Some("w".repeat(2001)), ..TinyServerEdit::default() }), said(WELCOME_SIZE));
        assert_eq!(role_create(account.clone(), TinyRoleEdit { name: Some("DJ".into()), color: Some("blue".into()), ..TinyRoleEdit::default() }), said(COLOR));
        assert_eq!(role_create(account.clone(), TinyRoleEdit::default()), said(ROLE_NAME_SIZE));
        assert_eq!(role_update(account.clone(), MODS.into(), TinyRoleEdit { permissions: Some(1 << 40), ..TinyRoleEdit::default() }), said(CHECK));
        assert_eq!(room_update(account.clone(), LOBBY.into(), TinyRoomEdit { bitrate_kbps: Some(8), ..TinyRoomEdit::default() }), said(BITRATE));
        assert_eq!(member_roles(account.clone(), "../x".into(), vec![]), said(CHECK));
        assert_eq!(kick(account.clone(), BEA.into(), "r".repeat(501)), said(REASON_SIZE));
        assert_eq!(invite_with(account.clone(), TinyInviteOptions { max_uses: Some(0), ..TinyInviteOptions::default() }), said(USES));
        assert_eq!(invite_with(account.clone(), TinyInviteOptions { expires_in_secs: Some(5), ..TinyInviteOptions::default() }), said(EXPIRY));
        assert_eq!(override_set(account.clone(), LOBBY.into(), MODS.into(), 1, 1), said(CHECK), "a bit both allowed and denied");
        assert_eq!(log.lock().unwrap().len(), before);
        // An icon: too big, or not a picture.
        let big = std::env::temp_dir().join(format!("tinyclient-icon-big-{}.png", std::process::id()));
        let mut bytes = png();
        bytes.resize(TINY_ICON_MAX + 1, 0);
        std::fs::write(&big, &bytes).unwrap();
        assert_eq!(server_icon(account.clone(), Some(big.to_string_lossy().into_owned())), said(ICON_SIZE));
        let text = std::env::temp_dir().join(format!("tinyclient-icon-text-{}.png", std::process::id()));
        std::fs::write(&text, b"hello").unwrap();
        assert_eq!(server_icon(account.clone(), Some(text.to_string_lossy().into_owned())), said(ICON_KIND));
        assert_eq!(log.lock().unwrap().len(), before);
        let good = std::env::temp_dir().join(format!("tinyclient-icon-good-{}.png", std::process::id()));
        std::fs::write(&good, png()).unwrap();
        assert_eq!(server_icon(account.clone(), Some(good.to_string_lossy().into_owned())), Response::Ok);
        let put = last(&log, "PUT /v1/server/icon ");
        assert!(put.contains("image/png") && put.contains("not really a picture"), "{put}");
        assert_eq!(server_icon(account.clone(), None), Response::Ok);
        assert!(!sent(&log, "DELETE /v1/server/icon ").is_empty());
        for path in [big, text, good] {
            let _ = std::fs::remove_file(path);
        }
    });
}

/// A custom role that may kick but not ban, on a server whose legacy role for it is `member`.
#[test]
fn the_permissions_decide_what_is_sent_and_not_the_legacy_role() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let me = r#"{"owner":false,"roles":["55555555-5555-5555-5555-555555555555"],"permissions":2111,"rank":1,"role":"member"}"#;
    let base = newer(me, log.clone());
    with_session(&base, "member", |account| {
        match community(&test_hub(), account.clone()) {
            Response::TinyCommunity { community } => {
                assert!(community.bans.is_empty(), "no ban permission: the bans are not asked for");
                assert!(community.invites.is_empty() && community.audit.is_empty());
            }
            other => panic!("load: {other:?}"),
        }
        assert!(sent(&log, "GET /v1/bans ").is_empty() && sent(&log, "GET /v1/invites ").is_empty());
        assert_eq!(kick(account.clone(), "44444444".into(), String::new()), Response::Ok, "kick_members, though the legacy role is member");
        let before = log.lock().unwrap().len();
        assert_eq!(ban(account.clone(), "44444444".into(), String::new(), None), said(NOT_YOURS));
        assert_eq!(server_update(account.clone(), TinyServerEdit { name: Some("x".into()), ..TinyServerEdit::default() }), said(NOT_YOURS));
        assert!(may(&account, "community.rooms.create").is_err());
        assert!(may(&account, "community.calls.join").is_ok());
        assert_eq!(log.lock().unwrap().len(), before);
    });
}

/// An older server answers the capabilities route with 404: the page is as it always was and
/// nothing new is sent.
#[test]
fn an_older_server_has_no_features_and_nothing_new_is_sent() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let seen = log.clone();
    let base = serve(move |request| {
        seen.lock().unwrap().push(request.to_owned());
        let first = request.lines().next().unwrap_or("");
        if first.starts_with("GET /v1/members ") {
            return http(200, "OK", &format!(r#"[{{"user_id":"{ME}","email":"a@b.co","display_name":"Ada","role":"admin","created_at":"t"}}]"#));
        }
        if first.starts_with("GET /v1/invites ") || first.starts_with("GET /v1/rooms ") {
            return http(200, "OK", "[]");
        }
        if first.starts_with("GET /v1/audit") {
            return http(200, "OK", r#"[{"id":1,"actor_id":null,"action":"member.join","subject":"x","detail":null,"created_at":"t"}]"#);
        }
        http(404, "Not Found", r#"{"error":"not_found"}"#)
    });
    with_session(&base, "admin", |account| {
        match community(&test_hub(), account.clone()) {
            Response::TinyCommunity { community } => {
                assert!(community.capabilities.is_empty() && community.access.is_none() && community.server.is_none());
                assert!(community.roles.is_empty() && community.categories.is_empty() && community.bans.is_empty());
                assert_eq!(community.audit.len(), 1, "an admin reads the audit, as before");
                assert!(community.allowed("community.invites.create") && !community.allowed("community.members.owner"));
            }
            other => panic!("load: {other:?}"),
        }
        let before = log.lock().unwrap().len();
        assert_eq!(server_update(account.clone(), TinyServerEdit { name: Some("x".into()), ..TinyServerEdit::default() }), said(OLDER));
        assert_eq!(ban(account.clone(), BEA.into(), String::new(), None), said(OLDER));
        assert_eq!(category_create(account.clone(), "Voice".into()), said(OLDER));
        assert_eq!(invite_with(account.clone(), TinyInviteOptions::default()), said(OLDER));
        assert_eq!(room_update(account.clone(), LOBBY.into(), TinyRoomEdit { topic: Some("t".into()), ..TinyRoomEdit::default() }), said(OLDER));
        match audit(account.clone(), TinyAuditFilter { before: Some(1), ..TinyAuditFilter::default() }) {
            Response::TinyAudit { audit } => assert!(audit.is_empty(), "no paging on an older server"),
            other => panic!("audit: {other:?}"),
        }
        assert_eq!(log.lock().unwrap().len(), before);
        // What an older server does take still goes: a room by name, a kick, the first audit page.
        assert_eq!(room_create(account.clone(), TinyRoomEdit { name: Some("Den".into()), topic: Some("t".into()), ..TinyRoomEdit::default() }), said("That room is gone."));
        assert_eq!(json_body(&log.lock().unwrap().last().cloned().unwrap()), serde_json::json!({ "name": "Den" }));
        assert_eq!(room_update(account.clone(), LOBBY.into(), TinyRoomEdit { name: Some("Den".into()), ..TinyRoomEdit::default() }), said("That room is gone."));
        assert!(matches!(audit(account.clone(), TinyAuditFilter::default()), Response::TinyAudit { audit } if audit.len() == 1));
        assert!(log.lock().unwrap().last().unwrap().starts_with("GET /v1/audit?limit=50 "));
    });
}

// First-run setup: `docs/server-setup.md` in tinyserver, against a local fake.

/// `GET /v1/setup` as the contract shows it, for an owner on a server with passkeys and one
/// member without a passkey.
const SETUP: &str = r#"{"needed":true,"completed_at":null,
    "current":{"provider_profile":"full","activity_visibility":"everyone","sign_in_methods":"any"},
    "deployment":{"passkeys":true,"sso":false},
    "counts":{"members":3,"members_without_passkey":2,"emails_stored":3,"owner_has_passkey":false},
    "presets":[{"id":"convenience","title":"Convenience","summary":"Easiest to run.","choices":{"provider_profile":"full","activity_visibility":"everyone","sign_in_methods":"any"},"available":true},
               {"id":"privacy","title":"Privacy","summary":"Keeps the least.","choices":{"provider_profile":"none","activity_visibility":"nobody","sign_in_methods":"passkey"},"available":true,"unavailable":null}],
    "choices":[{"id":"provider_profile","title":"Provider","question":"Keep what?","relevant":false,"options":[{"id":"full","title":"Both","effect":"Keeps both."},{"id":"none","title":"Nothing","effect":"Keeps neither."}]}],
    "always":["There are no usernames."]}"#;

/// What `PUT /v1/setup` answers: the whole settings, and the two counts.
fn applied(erased: u32, cleared: u32) -> String {
    format!(
        r#"{{"settings":{{"name":"AtomicStudio","description":"","welcome":"","icon":null,"default_room_id":null,"afk_room_id":null,"afk_timeout_secs":0,"image_max_bytes":8388608,"file_max_bytes":26214400,"allow_archives":false,"provider_profile":"none","activity_visibility":"nobody","sign_in_methods":"passkey","setup_completed_at":"2026-11-01T10:00:00Z","created_at":"t"}},"emails_erased":{erased},"last_seen_cleared":{cleared}}}"#
    )
}

/// A tinyserver with setup. `features` is the capabilities list's inside. What `PUT /v1/setup`
/// answers depends on what it is sent, so each test names its case in the body.
fn with_setup(me: &'static str, features: &'static str, log: Arc<Mutex<Vec<String>>>) -> String {
    serve(move |request| {
        log.lock().unwrap().push(request.to_owned());
        let first = request.lines().next().unwrap_or("");
        let body = request.split("\r\n\r\n").nth(1).unwrap_or("");
        let route = first.split(' ').take(2).collect::<Vec<_>>().join(" ");
        match route.as_str() {
            "GET /v1/capabilities" => http(200, "OK", &format!(r#"{{"version":"0.5.0","features":[{features}]}}"#)),
            "GET /v1/me" => http(200, "OK", me),
            "GET /v1/setup" => http(200, "OK", SETUP),
            "GET /v1/server" => http(
                200,
                "OK",
                r#"{"name":"AtomicStudio","description":"","welcome":"","icon":null,"default_room_id":null,"afk_room_id":null,"afk_timeout_secs":0,"provider_profile":"name","activity_visibility":"admins","sign_in_methods":"any","setup_completed_at":"2026-11-01T10:00:00Z","created_at":"t"}"#,
            ),
            "PUT /v1/setup" => {
                let put: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
                let is = |field: &str, value: &str| put[field] == value;
                match () {
                    _ if put["allow_lockout"] == true => http(200, "OK", &applied(0, 0)),
                    _ if is("activity_visibility", "admins") => http(409, "Conflict", r#"{"error":"members_without_passkey","count":3}"#),
                    _ if is("provider_profile", "name") => http(409, "Conflict", r#"{"error":"passkeys_unavailable"}"#),
                    _ if is("provider_profile", "none") => http(409, "Conflict", r#"{"error":"owner_needs_passkey"}"#),
                    _ if is("sign_in_methods", "any") => http(409, "Conflict", r#"{"error":"something_new"}"#),
                    _ if is("provider_profile", "full") => http(400, "Bad Request", r#"{"error":"invalid"}"#),
                    _ if put["erase_emails"] == true => http(200, "OK", &applied(3, 5)),
                    _ => http(200, "OK", &applied(0, 0)),
                }
            }
            "POST /v1/me/passkeys/link" => http(200, "OK", r#"{"url":"http://127.0.0.1:9/v1/auth/passkey/claim#claim-token"}"#),
            // What the page reads around it: nobody else, nothing to list.
            "GET /v1/members" | "GET /v1/invites" | "GET /v1/rooms" | "GET /v1/roles" | "GET /v1/categories" | "GET /v1/bans" => http(200, "OK", "[]"),
            _ if first.starts_with("GET /v1/audit") => http(200, "OK", "[]"),
            _ => http(404, "Not Found", r#"{"error":"not_found"}"#),
        }
    })
}

const SETUP_FEATURES: &str = r#""server","roles","first_run_setup""#;
const NOT_OWNER: &str = r#"{"user_id":"11111111-1111-1111-1111-111111111111","display_name":"Ada","role":"admin","owner":false,"roles":[],"permissions":1048575,"rank":5}"#;

fn applied_to(apply: TinySetupApply, account: &AccountKey) -> Response {
    setup_apply(account.clone(), apply)
}

#[test]
fn an_owner_reads_the_outline_in_the_servers_words() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let base = with_setup(OWNER, SETUP_FEATURES, log.clone());
    with_session(&base, "owner", |account| match setup_read(account) {
        Response::TinySetup { setup } => {
            assert!(setup.needed);
            assert_eq!(setup.counts.emails_stored, 3);
            assert_eq!(setup.presets[1].summary, "Keeps the least.");
            assert_eq!(setup.presets[1].unavailable, "");
            assert!(!setup.choices[0].relevant);
            assert_eq!(setup.choices[0].options[1].effect, "Keeps neither.");
            assert_eq!(setup.always, ["There are no usernames."]);
        }
        other => panic!("setup: {other:?}"),
    });
    assert_eq!(sent(&log, "GET /v1/setup ").len(), 1);
}

#[test]
fn nothing_is_asked_of_anyone_but_an_owner_of_a_server_with_setup() {
    // An admin with every permission is not an owner.
    let log = Arc::new(Mutex::new(Vec::new()));
    let base = with_setup(NOT_OWNER, SETUP_FEATURES, log.clone());
    with_session(&base, "admin", |account| {
        assert_eq!(setup_read(account.clone()), said(NOT_YOURS));
        assert_eq!(applied_to(TinySetupApply::default(), &account), said(NOT_YOURS));
    });
    assert!(sent(&log, "GET /v1/setup ").is_empty() && sent(&log, "PUT /v1/setup ").is_empty());
    // An owner on a server that does not list the feature: it says so, and nothing is sent.
    let log = Arc::new(Mutex::new(Vec::new()));
    let base = with_setup(OWNER, r#""server","roles""#, log.clone());
    with_session(&base, "owner", |account| {
        assert_eq!(setup_read(account.clone()), said(OLDER));
        assert_eq!(applied_to(TinySetupApply::default(), &account), said(OLDER));
    });
    assert!(sent(&log, "GET /v1/setup ").is_empty() && sent(&log, "PUT /v1/setup ").is_empty());
    // A server with no capabilities route at all, as before it had features.
    let base = serve(|_| http(404, "Not Found", r#"{"error":"not_found"}"#));
    with_session(&base, "owner", |account| assert_eq!(setup_read(account), said(OLDER)));
}

#[test]
fn each_way_of_answering_goes_to_the_servers_route_with_only_what_is_set() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let base = with_setup(OWNER, SETUP_FEATURES, log.clone());
    with_session(&base, "owner", |account| {
        // Keep what I have.
        match applied_to(TinySetupApply::default(), &account) {
            Response::TinySetupApplied { applied } => assert_eq!(applied.settings.setup_completed_at, "2026-11-01T10:00:00Z"),
            other => panic!("keep: {other:?}"),
        }
        assert_eq!(json_body(&last(&log, "PUT /v1/setup ")), serde_json::json!({}));
        // A preset alone.
        let preset = TinySetupApply { preset: Some("privacy".into()), ..TinySetupApply::default() };
        assert!(matches!(applied_to(preset, &account), Response::TinySetupApplied { .. }));
        assert_eq!(json_body(&last(&log, "PUT /v1/setup ")), serde_json::json!({"preset": "privacy"}));
        // A preset with the email addresses erased: the server's counts come back.
        let erase = TinySetupApply { preset: Some("privacy".into()), erase_emails: true, ..TinySetupApply::default() };
        match applied_to(erase, &account) {
            Response::TinySetupApplied { applied } => assert_eq!((applied.emails_erased, applied.last_seen_cleared), (3, 5)),
            other => panic!("erase: {other:?}"),
        }
        assert_eq!(json_body(&last(&log, "PUT /v1/setup ")), serde_json::json!({"preset": "privacy", "erase_emails": true}));
        // Each choice by itself, and the permission to lock people out, which goes through.
        let lockout = TinySetupApply { activity_visibility: Some("admins".into()), allow_lockout: true, ..TinySetupApply::default() };
        assert!(matches!(applied_to(lockout, &account), Response::TinySetupApplied { .. }));
        assert_eq!(json_body(&last(&log, "PUT /v1/setup ")), serde_json::json!({"activity_visibility": "admins", "allow_lockout": true}));
    });
    // Every request carried the session.
    assert!(sent(&log, "PUT /v1/setup ").iter().all(|put| put.to_ascii_lowercase().contains("authorization: bearer t")));
}

#[test]
fn a_refusal_the_owner_can_act_on_is_its_own_answer_and_the_rest_are_words() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let base = with_setup(OWNER, SETUP_FEATURES, log.clone());
    with_session(&base, "owner", |account| {
        let sends = |apply: TinySetupApply| applied_to(apply, &account);
        let lockout = TinySetupApply { activity_visibility: Some("admins".into()), ..TinySetupApply::default() };
        assert_eq!(sends(lockout), Response::TinySetupRefused { refusal: TinySetupRefusal::MembersWithoutPasskey { count: 3 } });
        let off = TinySetupApply { provider_profile: Some("name".into()), ..TinySetupApply::default() };
        assert_eq!(sends(off), Response::TinySetupRefused { refusal: TinySetupRefusal::PasskeysUnavailable });
        let needs = TinySetupApply { provider_profile: Some("none".into()), ..TinySetupApply::default() };
        assert_eq!(sends(needs), Response::TinySetupRefused { refusal: TinySetupRefusal::OwnerNeedsPasskey });
        // A 409 this build does not know, a 400 and a 403 are said in words.
        let newer = TinySetupApply { sign_in_methods: Some("any".into()), ..TinySetupApply::default() };
        assert_eq!(sends(newer), said(CHECK));
        let invalid = TinySetupApply { provider_profile: Some("full".into()), ..TinySetupApply::default() };
        assert_eq!(sends(invalid), said(CHECK));
    });
    // A count the server leaves out or sends too large still reads.
    assert_eq!(setup_refusal(&serde_json::json!({"error": "members_without_passkey"})), Some(TinySetupRefusal::MembersWithoutPasskey { count: 0 }));
    assert_eq!(setup_refusal(&serde_json::json!({"error": "members_without_passkey", "count": 1u64 << 40})), Some(TinySetupRefusal::MembersWithoutPasskey { count: u32::MAX }));
    assert_eq!(setup_refusal(&serde_json::json!({"error": "x"})), None);
    assert_eq!(setup_refusal(&serde_json::Value::Null), None);
}

#[test]
fn a_bad_shape_is_refused_before_anything_is_sent() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let base = with_setup(OWNER, SETUP_FEATURES, log.clone());
    with_session(&base, "owner", |account| {
        drop(setup_read(account.clone()));
        let before = log.lock().unwrap().len();
        for odd in ["", "Privacy", "../x", "privacy\r\nX: y", &"a".repeat(33)] {
            for apply in [
                TinySetupApply { preset: Some(odd.into()), ..TinySetupApply::default() },
                TinySetupApply { provider_profile: Some(odd.into()), ..TinySetupApply::default() },
                TinySetupApply { activity_visibility: Some(odd.into()), ..TinySetupApply::default() },
                TinySetupApply { sign_in_methods: Some(odd.into()), ..TinySetupApply::default() },
            ] {
                assert_eq!(applied_to(apply, &account), said(CHECK), "{odd:?}");
            }
        }
        assert_eq!(log.lock().unwrap().len(), before);
    });
}

#[test]
fn the_outline_is_held_to_a_size_a_window_can_draw() {
    let long = "w".repeat(SETUP_TEXT_MAX + 50);
    let option = serde_json::json!({"id": "x", "title": long, "effect": long, "needs": vec!["n"; 40]});
    let setup = serde_json::json!({
        "needed": true,
        "presets": (0..40).map(|n| serde_json::json!({"id": format!("p{n}"), "title": long, "summary": long, "unavailable": long})).collect::<Vec<_>>(),
        "choices": (0..40).map(|n| serde_json::json!({"id": format!("c{n}"), "question": long, "options": vec![option.clone(); 40]})).collect::<Vec<_>>(),
        "always": vec![long.clone(); 40],
    });
    let bounded = setup_bounded(serde_json::from_value(setup).unwrap());
    assert_eq!((bounded.presets.len(), bounded.choices.len(), bounded.always.len()), (SETUP_ROWS_MAX, SETUP_ROWS_MAX, SETUP_ROWS_MAX));
    assert_eq!(bounded.choices[0].options.len(), SETUP_ROWS_MAX);
    assert_eq!(bounded.choices[0].options[0].needs.len(), SETUP_ROWS_MAX);
    assert_eq!(bounded.presets[0].summary.chars().count(), SETUP_TEXT_MAX);
    assert_eq!(bounded.choices[0].options[0].effect.chars().count(), SETUP_TEXT_MAX);
    assert_eq!(bounded.always[0].chars().count(), SETUP_TEXT_MAX);
}

#[test]
fn the_page_carries_what_the_server_keeps_through_a_read_of_the_server() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let base = with_setup(OWNER, SETUP_FEATURES, log.clone());
    with_session(&base, "owner", |account| match community(&test_hub(), account) {
        Response::TinyCommunity { community } => {
            let server = community.server.expect("the server's settings");
            assert_eq!((server.provider_profile.as_str(), server.activity_visibility.as_str(), server.sign_in_methods.as_str()), ("name", "admins", "any"));
            assert_eq!(server.setup_completed_at, "2026-11-01T10:00:00Z");
            assert!(community.capabilities.has(TINY_FEATURE_FIRST_RUN_SETUP));
        }
        other => panic!("load: {other:?}"),
    });
}

#[test]
fn a_passkey_link_opens_in_the_system_browser_and_goes_nowhere_else() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let base = with_setup(OWNER, SETUP_FEATURES, log.clone());
    let opened = Arc::new(Mutex::new(Vec::new()));
    let seen = opened.clone();
    let browser: crate::adapters::tiny::browser::Opener = Arc::new(move |url: &str| {
        seen.lock().unwrap().push(url.to_owned());
        Ok(())
    });
    with_session(&base, "owner", |account| {
        assert_eq!(passkey_link_with(account, &browser), Response::Ok);
    });
    assert_eq!(opened.lock().unwrap().as_slice(), ["http://127.0.0.1:9/v1/auth/passkey/claim#claim-token"]);
    assert_eq!(sent(&log, "POST /v1/me/passkeys/link ").len(), 1);
}

#[test]
fn a_passkey_link_the_server_could_not_make_is_said_in_words_and_nothing_is_opened() {
    let nothing: crate::adapters::tiny::browser::Opener = Arc::new(|_: &str| panic!("the browser was opened"));
    // The sign-in is too old.
    let base = serve(|_| http(403, "Forbidden", r#"{"error":"recent_sign_in_required"}"#));
    with_session(&base, "owner", |account| assert_eq!(passkey_link_with(account, &nothing), said(SIGN_IN_AGAIN)));
    assert!(SIGN_IN_AGAIN.contains("sign in again"), "{SIGN_IN_AGAIN}");
    // A server from before passkeys.
    let base = serve(|_| http(404, "Not Found", r#"{"error":"not_found"}"#));
    with_session(&base, "owner", |account| assert_eq!(passkey_link_with(account, &nothing), said(OLDER)));
    // A bot, or anyone the server says no to.
    let base = serve(|_| http(403, "Forbidden", r#"{"error":"forbidden"}"#));
    with_session(&base, "owner", |account| assert_eq!(passkey_link_with(account, &nothing), said("You can't do that.")));
    // An answer with no link in it.
    let base = serve(|_| http(200, "OK", "{}"));
    with_session(&base, "owner", |account| assert_eq!(passkey_link_with(account, &nothing), said(UNREADABLE)));
    // A link that is not a web page never reaches the system's opener.
    let base = serve(|_| http(200, "OK", r#"{"url":"file:///etc/passwd"}"#));
    with_session(&base, "owner", |account| assert_eq!(passkey_link(account), said("That isn't a web page.")));
}
