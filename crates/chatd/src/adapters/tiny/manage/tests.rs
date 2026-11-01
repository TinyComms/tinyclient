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
