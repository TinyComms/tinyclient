//! Three apps in one room, the server's part played by `tests/support/mls.rs` as tinyserver
//! plays it: external sender, proposals at the group's epoch, the first commit wins.

use super::*;

#[allow(dead_code)]
mod mls {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/mls.rs"));
}

const ROOM: &str = "0b6f2c4e-0000-4000-8000-000000000001";
const ADA: &str = "5a0c0000-0000-4000-8000-0000000000a1";
const BEA: &str = "9d1e0000-0000-4000-8000-0000000000b2";
const CY: &str = "c0de0000-0000-4000-8000-0000000000c3";
const TONE: &[u8] = &[0x78, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];

fn b64(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

/// One app, with a key of its own and everyone in the roster.
fn app(user: &str) -> E2ee {
    let mut e2ee = E2ee::default();
    e2ee.with_identity(SigningKeyPair::generate());
    e2ee.offer(user, Some(&TinyE2ee { room_id: ROOM.into(), identity: String::new(), required: false }));
    e2ee.roster(vec![ADA.into(), BEA.into(), CY.into()]);
    e2ee
}

fn sent(e2ee: &mut E2ee) -> Vec<Value> {
    e2ee.take_outbox().iter().map(|text| serde_json::from_str(text).unwrap()).collect()
}

fn only(e2ee: &mut E2ee, op: &str) -> Value {
    let mut out = sent(e2ee);
    assert_eq!(out.len(), 1, "{out:?}");
    let frame = out.remove(0);
    assert_eq!(frame["type"], "call.e2ee");
    assert_eq!(frame["op"], op, "{frame}");
    frame
}

fn bytes(frame: &Value, field: &str) -> Vec<u8> {
    STANDARD.decode(frame[field].as_str().unwrap()).unwrap()
}

/// The server's `start`, and the key package it gets back.
fn start(server: &mls::Server, e2ee: &mut E2ee) -> Vec<u8> {
    let frame = json!({"type": "call.e2ee", "room_id": ROOM, "op": "start", "version": 1, "external_sender": b64(&server.package())});
    e2ee.frame(&frame.to_string());
    bytes(&only(e2ee, "key_package"), "data")
}

fn proposals(e2ee: &mut E2ee, messages: &[Vec<u8>], epoch: u64) -> Value {
    let frame = json!({"type": "call.e2ee", "op": "proposals", "epoch": epoch, "data": b64(&mls::append(messages))});
    e2ee.frame(&frame.to_string());
    only(e2ee, "commit")
}

fn deliver(e2ee: &mut E2ee, op: &str, epoch: u64, data: &[u8]) {
    let frame = json!({"type": "call.e2ee", "op": op, "epoch": epoch, "data": b64(data)});
    e2ee.frame(&frame.to_string());
    assert!(sent(e2ee).is_empty(), "applied without complaint");
}

/// Ada makes the group and adds Bea, then both add Cy and Ada's commit wins.
fn three() -> (mls::Server, E2ee, E2ee, E2ee) {
    let server = mls::Server::new();
    let group = group_id(ROOM);
    let (mut ada, mut bea, mut cy) = (app(ADA), app(BEA), app(CY));
    start(&server, &mut ada);
    let bea_package = start(&server, &mut bea);
    assert_eq!(ada.security().mode, SecurityMode::Pending, "alone, Ada holds her microphone back");
    assert_eq!(ada.seal(TONE), None);

    let commit = proposals(&mut ada, &[server.add_message(&bea_package, group, 0)], 0);
    deliver(&mut ada, "commit", 0, &bytes(&commit, "data"));
    deliver(&mut bea, "welcome", 1, &bytes(&commit, "welcome"));
    assert_eq!(ada.security().mode, SecurityMode::EndToEnd);
    assert_eq!(ada.security().epoch, 1);

    let cy_package = start(&server, &mut cy);
    let add = [server.add_message(&cy_package, group, 1)];
    let won = proposals(&mut ada, &add, 1);
    let lost = proposals(&mut bea, &add, 1);
    assert!(lost.get("welcome").is_some());
    deliver(&mut ada, "commit", 1, &bytes(&won, "data"));
    deliver(&mut bea, "commit", 1, &bytes(&won, "data"));
    deliver(&mut cy, "welcome", 2, &bytes(&won, "welcome"));
    (server, ada, bea, cy)
}

#[test]
fn three_members_share_one_code_and_hear_each_other() {
    let (_, mut ada, mut bea, mut cy) = three();
    let codes: Vec<String> = [&ada, &bea, &cy].iter().map(|e2ee| e2ee.security().code).collect();
    assert_eq!(codes[0].len(), 30);
    assert!(codes.iter().all(|code| *code == codes[0]), "{codes:?}");
    assert!([&ada, &bea, &cy].iter().all(|e2ee| e2ee.security().epoch == 2));

    let sealed = ada.seal(TONE).unwrap();
    assert_ne!(sealed, TONE);
    assert!(!sealed.windows(TONE.len()).any(|w| w == TONE), "the Opus frame is not in what the server forwards");
    assert_eq!(bea.open(ADA, &sealed).as_deref(), Some(TONE));
    assert_eq!(cy.open(ADA, &sealed).as_deref(), Some(TONE));
    // The server naming the wrong sender for a slot gets the frame dropped, not misattributed.
    assert_eq!(cy.open(BEA, &sealed), None);
    let back = cy.seal(TONE).unwrap();
    assert_eq!(ada.open(CY, &back).as_deref(), Some(TONE));
}

#[test]
fn members_and_their_safety_numbers_are_listed_by_who_the_roster_says_they_are() {
    let (_, mut ada, mut bea, _) = three();
    for e2ee in [&mut ada, &mut bea] {
        for (id, key, me, mine) in e2ee.safety_wanted() {
            let number = safety_number(id, &key, me, &mine);
            e2ee.add_safety(id, key, number);
        }
    }
    let (ada_sees, bea_sees) = (ada.security(), bea.security());
    let mut users: Vec<&str> = ada_sees.members.iter().map(|m| m.user_id.as_str()).collect();
    users.sort();
    assert_eq!(users, [BEA, CY], "everyone but Ada");
    let ada_bea = ada_sees.members.iter().find(|m| m.user_id == BEA).unwrap();
    let bea_ada = bea_sees.members.iter().find(|m| m.user_id == ADA).unwrap();
    assert_eq!(ada_bea.safety.len(), 45);
    assert_eq!(ada_bea.safety, bea_ada.safety, "both see the same safety number");
    assert!(ada.safety_wanted().is_empty(), "made once");

    // Someone the server does not list in the call is nobody we know.
    ada.roster(vec![ADA.into(), BEA.into()]);
    assert!(ada.security().members.iter().any(|m| m.user_id.is_empty()));
}

#[test]
fn a_leave_moves_the_epoch_and_the_leaver_hears_nothing_after() {
    let (server, mut ada, mut bea, mut cy) = three();
    let before = ada.security().code;
    // Cy is in leaf 2; the server proposes removing it to everyone who stays, and Bea's commit
    // wins this time.
    let remove = [server.remove_message(2, group_id(ROOM), 2)];
    proposals(&mut ada, &remove, 2);
    let commit = proposals(&mut bea, &remove, 2);
    assert!(commit.get("welcome").is_none(), "a removal welcomes nobody");
    deliver(&mut ada, "commit", 2, &bytes(&commit, "data"));
    deliver(&mut bea, "commit", 2, &bytes(&commit, "data"));
    assert_eq!(ada.security().epoch, 3);
    assert_eq!(ada.security().code, bea.security().code);
    assert_ne!(ada.security().code, before, "a new epoch, a new code");

    let after = ada.seal(TONE).unwrap();
    assert_eq!(bea.open(ADA, &after).as_deref(), Some(TONE));
    assert_eq!(cy.open(ADA, &after), None, "who left has no key for what is said after");
}

#[test]
fn a_joiner_cannot_open_what_was_said_before_it_joined() {
    let server = mls::Server::new();
    let group = group_id(ROOM);
    let (mut ada, mut bea, mut cy) = (app(ADA), app(BEA), app(CY));
    start(&server, &mut ada);
    let bea_package = start(&server, &mut bea);
    let commit = proposals(&mut ada, &[server.add_message(&bea_package, group, 0)], 0);
    deliver(&mut ada, "commit", 0, &bytes(&commit, "data"));
    deliver(&mut bea, "welcome", 1, &bytes(&commit, "welcome"));
    let earlier = ada.seal(TONE).unwrap();

    let cy_package = start(&server, &mut cy);
    let add = [server.add_message(&cy_package, group, 1)];
    let commit = proposals(&mut ada, &add, 1);
    proposals(&mut bea, &add, 1);
    deliver(&mut ada, "commit", 1, &bytes(&commit, "data"));
    deliver(&mut bea, "commit", 1, &bytes(&commit, "data"));
    deliver(&mut cy, "welcome", 2, &bytes(&commit, "welcome"));
    assert_eq!(cy.open(ADA, &earlier), None);
    assert_eq!(bea.open(ADA, &earlier).as_deref(), Some(TONE), "Bea was there");
    let now = ada.seal(TONE).unwrap();
    assert_eq!(cy.open(ADA, &now).as_deref(), Some(TONE));
}

#[test]
fn clear_audio_is_never_played_in_an_end_to_end_call_and_silence_passes() {
    let (_, _, mut bea, _) = three();
    assert_eq!(bea.open(ADA, TONE), None, "the server cannot inject audio");
    assert_eq!(bea.open(ADA, &SILENCE).as_deref(), Some(&SILENCE[..]));
    assert_eq!(bea.seal(&SILENCE).as_deref(), Some(&SILENCE[..]), "silence is never sealed, as in DAVE");
    assert_eq!(bea.open("", &[1, 2, 0xfa, 0xfa]), None, "a slot nobody is named for yet");
}

#[test]
fn a_proposal_for_someone_not_in_the_call_is_not_committed() {
    let server = mls::Server::new();
    let (mut ada, mut ghost) = (app(ADA), app("6666aaaa-0000-4000-8000-000000000666"));
    start(&server, &mut ada);
    let package = start(&server, &mut ghost);
    let frame = json!({"type": "call.e2ee", "op": "proposals", "epoch": 0, "data": b64(&mls::append(&[server.add_message(&package, group_id(ROOM), 0)]))});
    ada.frame(&frame.to_string());
    assert!(sent(&mut ada).is_empty());
}

#[test]
fn a_commit_that_does_not_apply_is_reported_and_the_app_comes_back_with_a_new_key_package() {
    let (_, mut ada, _, _) = three();
    ada.frame(&json!({"type": "call.e2ee", "op": "commit", "epoch": 2, "data": b64(&[0, 1, 2, 3])}).to_string());
    let out = sent(&mut ada);
    let ops: Vec<&str> = out.iter().map(|frame| frame["op"].as_str().unwrap()).collect();
    assert_eq!(ops, ["invalid", "key_package"]);
    assert_eq!(ada.security().mode, SecurityMode::Pending, "out of the group until added again");
    assert_eq!(ada.seal(TONE), None);
}

#[test]
fn off_is_never_obeyed_silently_and_says_who_cannot() {
    let (_, mut ada, _, _) = three();
    ada.frame(&json!({"type": "call.e2ee", "op": "off", "user_ids": [CY]}).to_string());
    let security = ada.security();
    assert_eq!(security.mode, SecurityMode::Blocked, "the server's word alone does not turn encryption off");
    assert_eq!(security.unsupported, [CY]);
    assert!(!security.server_lacks && !security.required);
    assert!(security.code.is_empty() && security.members.is_empty(), "nothing claimed that does not hold");
    assert_eq!(ada.seal(TONE), None, "nothing in clear until the person chooses");
    assert_eq!(ada.open(BEA, TONE), None);
    // The person chooses to carry on: then, and only then, it is transport only.
    assert_eq!(ada.carry_on_unencrypted(), Ok(()));
    assert_eq!(ada.security().mode, SecurityMode::Transport);
    assert_eq!(ada.seal(TONE).as_deref(), Some(TONE));
    // The server saying it again for the same seat is not a new downgrade to ask about.
    ada.frame(&json!({"type": "call.e2ee", "op": "off", "user_ids": [CY, BEA]}).to_string());
    assert_eq!(ada.security().mode, SecurityMode::Transport);
    assert_eq!(ada.security().unsupported, [CY, BEA]);
    assert_eq!(ada.open(BEA, TONE).as_deref(), Some(TONE));
    assert_eq!(ada.open(BEA, &[1, 2, 0xfa, 0xfa]), None, "a frame sealed for the old group is not Opus");
}


/// One app that only joins end-to-end encrypted calls.
fn requiring(user: &str) -> E2ee {
    let mut e2ee = E2ee::default();
    e2ee.with_identity(SigningKeyPair::generate());
    e2ee.offer(user, Some(&TinyE2ee { room_id: ROOM.into(), identity: String::new(), required: true }));
    e2ee.roster(vec![ADA.into(), BEA.into(), CY.into()]);
    e2ee
}

#[test]
fn off_blocks_a_call_that_must_be_end_to_end() {
    let server = mls::Server::new();
    let mut ada = requiring(ADA);
    start(&server, &mut ada);
    ada.frame(&json!({"type": "call.e2ee", "op": "off", "user_ids": [CY]}).to_string());
    let security = ada.security();
    assert_eq!(security.mode, SecurityMode::Blocked);
    assert_eq!(security.unsupported, [CY], "who cannot is still said");
    assert!(!security.server_lacks);
    assert!(security.required, "the panel offers no way on");
    assert!(security.code.is_empty() && security.members.is_empty(), "nothing claimed that does not hold");
    // Nothing goes out and nothing is played, audio or video, clear or sealed.
    assert_eq!(ada.seal(TONE), None);
    assert_eq!(ada.seal(&[0xf8, 0xff, 0xfe]), None, "not even silence");
    assert_eq!(ada.seal_video(&[1, 2, 3, 4]), None);
    assert_eq!(ada.open(BEA, TONE), None, "clear audio is not played");
    assert_eq!(ada.open(BEA, &[1, 2, 0xfa, 0xfa]), None);
    assert_eq!(ada.open_video(BEA, &[1, 2, 3, 4]), None);
    // And the person cannot choose their way past it.
    assert!(ada.carry_on_unencrypted().is_err());
    assert_eq!(ada.security().mode, SecurityMode::Blocked);
    assert_eq!(ada.seal(TONE), None);
}

#[test]
fn off_without_the_requirement_carries_on_only_once_the_person_chooses() {
    let server = mls::Server::new();
    let mut ada = app(ADA);
    start(&server, &mut ada);
    ada.frame(&json!({"type": "call.e2ee", "op": "off", "user_ids": [CY]}).to_string());
    assert_eq!(ada.security().mode, SecurityMode::Blocked);
    assert_eq!(ada.seal_video(&[1, 2, 3, 4]), None);
    ada.carry_on_unencrypted().unwrap();
    assert_eq!(ada.security().mode, SecurityMode::Transport);
    assert_eq!(ada.seal(TONE).as_deref(), Some(TONE));
    assert_eq!(ada.seal_video(&[1, 2, 3, 4]).as_deref(), Some(&[1u8, 2, 3, 4][..]));
    assert_eq!(ada.open(BEA, TONE).as_deref(), Some(TONE));
}

#[test]
fn a_blocked_call_recovers_when_the_server_starts_the_group_again() {
    let server = mls::Server::new();
    let mut ada = requiring(ADA);
    start(&server, &mut ada);
    ada.frame(&json!({"type": "call.e2ee", "op": "off", "user_ids": [CY]}).to_string());
    assert_eq!(ada.security().mode, SecurityMode::Blocked);
    // The person who could not do it left: the server starts the group again.
    start(&server, &mut ada);
    let security = ada.security();
    assert_eq!(security.mode, SecurityMode::Pending, "a group again, with no keys until it is committed");
    assert!(security.unsupported.is_empty());
    assert_eq!(ada.seal(TONE), None, "still nothing in clear");
    // And a later `off` blocks again: the requirement is the person's, not the call's.
    ada.frame(&json!({"type": "call.e2ee", "op": "off", "user_ids": [BEA]}).to_string());
    assert_eq!(ada.security().mode, SecurityMode::Blocked);
}

#[test]
fn a_fresh_offer_takes_the_requirement_from_the_offer() {
    let mut ada = requiring(ADA);
    ada.frame(&json!({"type": "call.e2ee", "op": "off"}).to_string());
    assert_eq!(ada.security().mode, SecurityMode::Blocked);
    assert!(ada.carry_on_unencrypted().is_err());
    // Joining again with the setting off is a call that may carry on, once the person says so.
    ada.offer(ADA, Some(&TinyE2ee { room_id: ROOM.into(), identity: String::new(), required: false }));
    ada.frame(&json!({"type": "call.e2ee", "op": "off"}).to_string());
    assert_eq!(ada.security().mode, SecurityMode::Blocked);
    ada.carry_on_unencrypted().unwrap();
    assert_eq!(ada.security().mode, SecurityMode::Transport);
    // The choice was for that seat: a fresh offer and another `off` ask again.
    ada.offer(ADA, Some(&TinyE2ee { room_id: ROOM.into(), identity: String::new(), required: false }));
    ada.frame(&json!({"type": "call.e2ee", "op": "off"}).to_string());
    assert_eq!(ada.security().mode, SecurityMode::Blocked);
    // A server with no end-to-end encryption at all is transport, with the lack said.
    ada.offer(ADA, None);
    let security = ada.security();
    assert_eq!((security.mode, security.server_lacks), (SecurityMode::Transport, true));
}
#[test]
fn carrying_on_unencrypted_is_refused_when_nothing_was_turned_off() {
    let server = mls::Server::new();
    let mut ada = app(ADA);
    assert!(ada.carry_on_unencrypted().is_err(), "waiting for the server is not a downgrade to accept");
    start(&server, &mut ada);
    assert!(ada.carry_on_unencrypted().is_err(), "nor is a group being made");
    assert_eq!(ada.seal(TONE), None);
}

#[test]
fn a_server_without_end_to_end_encryption_is_said_to_lack_it() {
    let mut e2ee = E2ee::default();
    e2ee.offer(ADA, None);
    let security = e2ee.security();
    assert_eq!(security.mode, SecurityMode::Transport);
    assert!(security.server_lacks);
    assert_eq!(e2ee.seal(TONE).as_deref(), Some(TONE));
    // Nothing the server says turns on what was never offered.
    let server = mls::Server::new();
    e2ee.frame(&json!({"type": "call.e2ee", "op": "start", "version": 1, "external_sender": b64(&server.package())}).to_string());
    assert!(e2ee.take_outbox().is_empty());
    assert_eq!(e2ee.security().mode, SecurityMode::Transport);
}

#[test]
fn until_the_server_says_which_call_this_is_nothing_goes_out() {
    let mut e2ee = app(ADA);
    assert_eq!(e2ee.security().mode, SecurityMode::Pending);
    assert_eq!(e2ee.seal(TONE), None);
    assert_eq!(e2ee.open(BEA, TONE), None);
}

#[test]
fn a_rejoin_starts_afresh() {
    let (server, mut ada, _, _) = three();
    ada.offer(ADA, Some(&TinyE2ee { room_id: ROOM.into(), identity: String::new(), required: false }));
    assert_eq!(ada.security().mode, SecurityMode::Pending);
    start(&server, &mut ada);
    assert_eq!(ada.seal(TONE), None, "a member again only once a commit adds it");
}

#[test]
fn the_identity_key_is_kept_and_read_back() {
    let dir = std::env::temp_dir().join(format!("mediad-e2ee-{}", std::process::id()));
    let path = dir.join("identity.key");
    let (made, kept) = identity(&path);
    assert!(kept);
    let (read, _) = identity(&path);
    assert!(read == made, "the same key the next call");
    // A key an older build left readable by others is narrowed when it is read.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let (again, _) = identity(&path);
        assert!(again == made);
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!((mode(&path), mode(&dir)), (0o600, 0o700));
    }
    std::fs::write(&path, "garbage").unwrap();
    let (fresh, _) = identity(&path);
    assert!(fresh != made, "an unreadable file gives a new key");
    // The damaged file is kept, not destroyed with its replacement.
    assert_eq!(
        std::fs::read_to_string(path.with_extension("key.unreadable")).unwrap(),
        "garbage"
    );
    // The replacement is read back as itself.
    let (again, _) = identity(&path);
    assert!(again == fresh);
    // Writing leaves nothing but the key and the moved-aside file: no temp file.
    let mut names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["identity.key", "identity.key.unreadable"]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600, "only the owner reads the key");
        assert_eq!(mode(&dir), 0o700);
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn ids_are_the_contract_s_hashes() {
    assert_eq!(user_id(ADA), user_id(ADA));
    assert_ne!(user_id(ADA), user_id(BEA));
    assert_ne!(user_id(ROOM), group_id(ROOM), "users and rooms never share an id");
    let digest = Sha256::digest(format!("tiny-e2ee-v1 user {ADA}"));
    assert_eq!(user_id(ADA).to_be_bytes(), digest[..8]);
    // The values tinyserver's own tests pin: both ends must name a person and a room alike.
    assert_eq!(user_id("00000000-0000-4000-8000-000000000001"), 0x4b7e_2001_4471_bb39);
    assert_eq!(group_id("0b6f3d0e-1c2a-4d5e-8f90-123456789abc"), 0xff5f_ff38_49bf_c73d);
}

/// An H.264 picture is sealed with the same group as voice. The NAL header stays clear so
/// the SFU can still see a keyframe, and the other side opens the same frame.
#[test]
fn a_sealed_picture_keeps_its_nal_header_and_opens() {
    let (_, mut ada, mut bea, _) = three();
    let frame = [
        0, 0, 0, 1, 0x67, 0x42, 0xe0, 0x1f, 0, 0, 0, 1, 0x65, 0x88, 0x84, 0x21, 0xff, 0x10, 0x22, 0x33, 0x44, 0x55,
    ];
    let sealed = ada.seal_video(&frame).expect("sealed");
    assert_ne!(sealed, frame);
    assert_eq!(sealed[4], 0x67, "the SPS NAL header stays clear");
    let idr = sealed.windows(5).position(|window| window == [0, 0, 0, 1, 0x65]).expect("idr start");
    assert_eq!(sealed[idr + 4], 0x65, "the IDR NAL header stays clear");
    assert_ne!(&sealed[idr + 5..], &frame[13..], "the slice body is not in clear");
    assert_eq!(bea.open_video(ADA, &sealed).as_deref(), Some(&frame[..]));
    assert_eq!(bea.open_video(ADA, &frame), None, "a clear picture is dropped in an end-to-end call");
}

#[test]
fn what_is_told_is_told_once() {
    let mut e2ee = app(ADA);
    assert!(e2ee.changed().is_some());
    assert!(e2ee.changed().is_none());
    e2ee.frame(&json!({"type": "call.e2ee", "op": "off", "user_ids": []}).to_string());
    assert_eq!(e2ee.changed().map(|s| s.mode), Some(SecurityMode::Blocked));
    e2ee.carry_on_unencrypted().unwrap();
    assert_eq!(e2ee.changed().map(|s| s.mode), Some(SecurityMode::Transport));
}

