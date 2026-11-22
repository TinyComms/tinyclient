//! Three apps in one room, the server's part played by `tests/support/mls.rs` as tinyserver
//! plays it: external sender, proposals at the group's epoch, the first commit wins.

use super::*;

#[allow(dead_code)]
mod mls {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/mls.rs"));
}

const ROOM: &str = "0b6f2c4e-0000-4000-8000-000000000001";
pub(crate) const ADA: &str = "5a0c0000-0000-4000-8000-0000000000a1";
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

/// A commit holds the lock the tick seals under. The tick is told at once and does not wait, the
/// frame is not sent in the meantime, and when it goes it goes under the new epoch's key, which
/// who stayed can open and who left cannot.
#[test]
fn a_frame_waits_out_a_commit_without_waiting_on_it_and_seals_under_the_new_epoch() {
    use crate::session::Controls;
    use std::sync::mpsc::channel;
    use std::sync::Arc;

    let (server, mut ada, mut bea, mut cy) = three();
    let remove = [server.remove_message(2, group_id(ROOM), 2)];
    proposals(&mut ada, &remove, 2);
    let commit = bytes(&proposals(&mut bea, &remove, 2), "data");
    deliver(&mut bea, "commit", 2, &commit);
    let controls = Arc::new(Controls::default());
    *controls.tiny_e2ee.lock().unwrap() = ada;

    let (held, holding) = channel();
    let (done, finish) = channel::<()>();
    let mls = controls.clone();
    let applying = std::thread::spawn(move || {
        mls.e2ee(|e2ee| {
            held.send(()).unwrap();
            finish.recv().unwrap();
            e2ee.frame(&json!({"type": "call.e2ee", "op": "commit", "epoch": 2, "data": b64(&commit)}).to_string());
        });
    });
    holding.recv().unwrap();
    assert!(controls.try_e2ee(|e2ee| e2ee.seal(TONE)).is_none(), "the tick is not kept waiting");
    done.send(()).unwrap();
    applying.join().unwrap();

    let sealed = controls.try_e2ee(|e2ee| e2ee.seal(TONE)).flatten().expect("sealed once the commit is in");
    assert!(!sealed.windows(TONE.len()).any(|window| window == TONE), "never in clear");
    assert_eq!(controls.tiny_e2ee.lock().unwrap().security().epoch, 3);
    assert_eq!(bea.open(ADA, &sealed).as_deref(), Some(TONE));
    assert_eq!(cy.open(ADA, &sealed), None, "who left has no key for it");
}

#[test]
fn with_no_keys_a_frame_that_does_not_wait_is_still_held_back() {
    use crate::session::Controls;

    let controls = Controls::default();
    *controls.tiny_e2ee.lock().unwrap() = app(ADA);
    assert_eq!(controls.try_e2ee(|e2ee| e2ee.seal(TONE)), Some(None), "waiting for the server: nothing goes in clear");
    assert_eq!(controls.try_e2ee(|e2ee| e2ee.open(BEA, TONE)), Some(None));
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

/// Helper for cross-module proof tests (e.g. `mf_encoder::tests`).
pub(crate) fn pair_for_video_proof() -> (E2ee, E2ee) {
    let (_, ada, bea, _) = three();
    (ada, bea)
}

/// Real `str0m::Rtc` loopback pair (ICE + DTLS + SRTP + H264Packetizer + H264Depacketizer)
/// proving sealed H.264 frames survive the exact WebRTC RTP pipeline used in Tiny calls.
pub(crate) struct Str0mVideoPipe {
    tx: str0m::Rtc,
    rx: str0m::Rtc,
    tx_addr: std::net::SocketAddr,
    rx_addr: std::net::SocketAddr,
    mid: str0m::media::Mid,
    pt: str0m::media::Pt,
    now: Instant,
}

impl Str0mVideoPipe {
    pub(crate) fn new() -> Self {
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};
        use str0m::media::{Direction, MediaKind, Pt};
        use str0m::{Candidate, RtcConfig};
        let now = Instant::now();
        let make = |ice_lite: bool| {
            let mut cfg = RtcConfig::new().clear_codecs();
            if ice_lite {
                cfg = cfg.set_ice_lite(true);
            }
            cfg.codec_config().add_h264(Pt::new_with_value(108), None, true, 0x42e033);
            cfg.build(now)
        };
        let mut tx = make(false);
        let mut rx = make(true);
        let tx_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 40001);
        let rx_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 40002);
        tx.add_local_candidate(Candidate::host(tx_addr, "udp").unwrap());
        rx.add_local_candidate(Candidate::host(rx_addr, "udp").unwrap());
        let mut api = tx.sdp_api();
        let mid = api.add_media(MediaKind::Video, Direction::SendOnly, None, None, None);
        let (offer, pending) = api.apply().unwrap();
        let answer = rx.sdp_api().accept_offer(offer).unwrap();
        tx.sdp_api().accept_answer(pending, answer).unwrap();
        let mut pipe = Self { tx, rx, tx_addr, rx_addr, mid, pt: Pt::new_with_value(108), now };
        pipe.pump_until_connected();
        pipe
    }

    fn pump_until_connected(&mut self) {
        for _ in 0..200 {
            if self.tx.is_connected() && self.rx.is_connected() {
                return;
            }
            let _ = self.pump_once();
        }
        panic!("str0m loopback pipe did not connect");
    }

    fn pump_once(&mut self) -> Vec<Vec<u8>> {
        use str0m::net::{Protocol, Receive};
        use str0m::{Event, Input, Output};
        let mut out = Vec::new();
        self.now += Duration::from_millis(5);
        let _ = self.tx.handle_input(Input::Timeout(self.now));
        let _ = self.rx.handle_input(Input::Timeout(self.now));
        let mut to_rx = Vec::new();
        let mut to_tx = Vec::new();
        loop {
            match self.tx.poll_output().unwrap() {
                Output::Timeout(_) => break,
                Output::Transmit(t) => to_rx.push(t.contents),
                Output::Event(_) => {}
            }
        }
        for pkt in to_rx {
            let r = Receive { proto: Protocol::Udp, source: self.tx_addr, destination: self.rx_addr, contents: (&*pkt).try_into().unwrap() };
            let _ = self.rx.handle_input(Input::Receive(self.now, r));
        }
        loop {
            match self.rx.poll_output().unwrap() {
                Output::Timeout(_) => break,
                Output::Transmit(t) => to_tx.push(t.contents),
                Output::Event(Event::MediaData(data)) => out.push(data.data.to_vec()),
                Output::Event(_) => {}
            }
        }
        for pkt in to_tx {
            let r = Receive { proto: Protocol::Udp, source: self.rx_addr, destination: self.tx_addr, contents: (&*pkt).try_into().unwrap() };
            let _ = self.tx.handle_input(Input::Receive(self.now, r));
        }
        out
    }

    pub(crate) fn send_and_receive(&mut self, frame: Vec<u8>, rtp_ts: u64) -> Vec<u8> {
        use str0m::media::{Frequency, MediaTime};
        let writer = self.tx.writer(self.mid).expect("video writer");
        writer.write(self.pt, self.now, MediaTime::new(rtp_ts, Frequency::NINETY_KHZ), frame).expect("str0m write");
        for _ in 0..40 {
            let got = self.pump_once();
            if let Some(first) = got.into_iter().next() {
                return first;
            }
        }
        panic!("str0m loopback did not deliver video frame");
    }
}

/// Windows hardware H.264 encoders commonly emit AUD (NAL type 9) and CBR Filler (NAL type 12).
/// str0m's `H264Packetizer` silently drops NAL types 9 and 12; if `seal_video` did not strip them
/// before `davey` encrypted the frame, `open_video` on the receiver would fail AES-GCM on every
/// frame. This proves the sanitized sealed frame survives real str0m WebRTC RTP round-trip.
#[test]
fn a_sealed_picture_with_aud_and_filler_survives_str0m_rtp_round_trip() {
    let (_, mut ada, mut bea, _) = three();
    let frame_with_aud_and_filler = [
        0, 0, 0, 1, 0x09, 0x10,                         // AUD (NAL 9)
        0, 0, 0, 1, 0x06, 0x05, 0x02, 0xaa, 0xbb, 0x80, // SEI (NAL 6)
        0, 0, 0, 1, 0x67, 0x42, 0xe0, 0x1f,             // SPS (NAL 7)
        0, 0, 0, 1, 0x68, 0xce, 0x38, 0x80,             // PPS (NAL 8)
        0, 0, 0, 1, 0x0c, 0xff, 0xff, 0x80,             // Filler (NAL 12)
        0, 0, 0, 1, 0x65, 0x88, 0x84, 0x21, 0xff, 0x10, 0x22, 0x33, 0x44, 0x55, // IDR (NAL 5)
    ];
    let sealed = ada.seal_video(&frame_with_aud_and_filler).expect("sealed");
    let mut pipe = Str0mVideoPipe::new();
    let reassembled = pipe.send_and_receive(sealed, 90_000);
    let opened = bea.open_video(ADA, &reassembled).expect("receiver must decrypt frame after real str0m WebRTC RTP round-trip");
    assert!(opened.windows(5).any(|w| w == [0, 0, 0, 1, 0x67]), "SPS preserved");
    assert!(opened.windows(5).any(|w| w == [0, 0, 0, 1, 0x68]), "PPS preserved");
    assert!(opened.windows(5).any(|w| w == [0, 0, 0, 1, 0x65]), "IDR preserved");
    assert!(!opened.windows(5).any(|w| w == [0, 0, 0, 1, 0x09]), "AUD stripped");
    assert!(!opened.windows(5).any(|w| w == [0, 0, 0, 1, 0x0c]), "Filler stripped");
}

/// Multi-stream video: one sender's camera and screen share are sealed by one encryptor, so they
/// share one nonce counter. After the first picture of theirs has opened, the rest open in any
/// arrival order (DAVE keeps the nonces it skipped), none opens twice, and a frame arriving after
/// a mid changes owner fails to open under the new owner's identity.
#[test]
fn interleaved_camera_and_share_from_one_sender_both_open_and_wrong_owner_is_dropped() {
    let (_, mut ada, mut bea, _) = three();
    let cam_a = [0, 0, 0, 1, 0x67, 0x42, 0xe0, 0x1f, 0, 0, 0, 1, 0x65, 0x11, 0x22, 0x33, 0x44, 0x55];
    let share_a = [0, 0, 0, 1, 0x67, 0x42, 0xe0, 0x28, 0, 0, 0, 1, 0x65, 0xaa, 0xbb, 0xcc, 0xdd, 0xee];
    let cam_b = [0, 0, 0, 1, 0x41, 0x9a, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06];
    let share_b = [0, 0, 0, 1, 0x41, 0x9a, 0x10, 0x20, 0x30, 0x40, 0x50, 0x60];

    let s_cam_a = ada.seal_video(&cam_a).expect("seal cam_a");
    let s_share_a = ada.seal_video(&share_a).expect("seal share_a");
    let s_cam_b = ada.seal_video(&cam_b).expect("seal cam_b");
    let s_share_b = ada.seal_video(&share_b).expect("seal share_b");

    // Arrive out of order across the two concurrent streams from Ada.
    assert_eq!(bea.open_video(ADA, &s_cam_a).as_deref(), Some(&cam_a[..]));
    assert_eq!(bea.open_video(ADA, &s_share_b).as_deref(), Some(&share_b[..]));
    assert_eq!(bea.open_video(ADA, &s_cam_b).as_deref(), Some(&cam_b[..]));
    assert_eq!(bea.open_video(ADA, &s_share_a).as_deref(), Some(&share_a[..]));
    // Replay protection is whole: what opened once does not open again.
    assert_eq!(bea.open_video(ADA, &s_cam_b), None);
    assert_eq!(bea.open_video(ADA, &s_share_a), None);

    // Slot changes owner to Cy: a stale frame sealed by Ada is rejected under Cy's identity.
    assert_eq!(bea.open_video(CY, &s_share_a), None);
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

/// A panic while the lock was held (`seal_video` on the capture thread) leaves encryption in an
/// unknown state. The tick ends the call and says so. `fail` ends the process, so this happens
/// in a copy of this test run on its own.
#[test]
fn a_poisoned_lock_fails_the_call_in_words() {
    const ALONE: &str = "MEDIAD_TEST_POISON";
    if std::env::var_os(ALONE).is_some() {
        let controls = std::sync::Arc::new(crate::session::Controls::default());
        let holder = controls.clone();
        let _ = std::thread::spawn(move || {
            let _held = holder.tiny_e2ee.lock().unwrap();
            panic!("seal_video");
        })
        .join();
        let _ = controls.try_e2ee(|e2ee| e2ee.seal(TONE));
        return;
    }
    let test = "e2ee::tests::a_poisoned_lock_fails_the_call_in_words";
    let alone = std::process::Command::new(std::env::current_exe().unwrap()).args([test, "--exact"]).env(ALONE, "1").output().unwrap();
    assert_eq!(alone.status.code(), Some(2), "{alone:?}");
    assert!(String::from_utf8_lossy(&alone.stdout).contains(BROKEN), "{alone:?}");
}

fn ops(e2ee: &mut E2ee) -> Vec<String> {
    sent(e2ee).iter().map(|frame| frame["op"].as_str().unwrap().to_owned()).collect()
}

/// Sealed by nobody: it ends as a sealed frame does and opens with no key.
const JUNK: [u8; 14] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 0xfa, 0xfa];

#[test]
fn pictures_that_do_not_open_never_rebuild_the_group() {
    let (_, mut ada, mut bea, _) = three();
    let junk = [0, 0, 0, 1, 0x65, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 0xfa, 0xfa];
    for _ in 0..TOLERANCE * 2 {
        assert_eq!(bea.open_video(ADA, &junk), None);
    }
    assert!(sent(&mut bea).is_empty(), "a picture that will not open is not a broken group");
    assert_eq!(bea.security().mode, SecurityMode::EndToEnd);
    let sealed = ada.seal(TONE).unwrap();
    assert_eq!(bea.open(ADA, &sealed).as_deref(), Some(TONE));
}

/// The tolerance is each sender's own: Cy being heard does not hide that Ada is not.
#[test]
fn one_voice_that_stops_opening_rebuilds_the_group_whoever_else_is_heard() {
    let (_, _, mut bea, mut cy) = three();
    for _ in 0..TOLERANCE {
        assert_eq!(bea.open(ADA, &JUNK), None);
        let fine = cy.seal(TONE).unwrap();
        assert_eq!(bea.open(CY, &fine).as_deref(), Some(TONE));
    }
    assert!(sent(&mut bea).is_empty(), "within the tolerance");
    assert_eq!(bea.open(ADA, &JUNK), None);
    assert_eq!(ops(&mut bea), ["invalid", "key_package"]);
}

/// The server's proposals and chatd's roster come by two roads. Proposals that get here first
/// wait for the roster that names who they add, and no longer.
#[test]
fn proposals_that_come_before_the_roster_are_committed_when_it_names_who_they_add() {
    let server = mls::Server::new();
    let group = group_id(ROOM);
    let (mut ada, mut bea, mut cy) = (app(ADA), app(BEA), app(CY));
    ada.roster(vec![ADA.into()]);
    start(&server, &mut ada);
    let (bea_package, cy_package) = (start(&server, &mut bea), start(&server, &mut cy));
    let batch = [server.add_message(&bea_package, group, 0), server.add_message(&cy_package, group, 0)];
    ada.frame(&json!({"type": "call.e2ee", "op": "proposals", "epoch": 0, "data": b64(&mls::append(&batch))}).to_string());
    assert!(sent(&mut ada).is_empty(), "nobody the call does not have is committed");
    ada.roster(vec![ADA.into(), BEA.into()]);
    assert!(sent(&mut ada).is_empty(), "Cy is still not in the call");
    ada.roster(vec![ADA.into(), BEA.into(), CY.into()]);
    let commit = only(&mut ada, "commit");
    assert_eq!(commit["epoch"], 0);
    deliver(&mut ada, "commit", 0, &bytes(&commit, "data"));
    deliver(&mut bea, "welcome", 1, &bytes(&commit, "welcome"));
    deliver(&mut cy, "welcome", 1, &bytes(&commit, "welcome"));
    let sealed = ada.seal(TONE).unwrap();
    assert_eq!(bea.open(ADA, &sealed).as_deref(), Some(TONE));
    assert_eq!(cy.open(ADA, &sealed).as_deref(), Some(TONE));
    ada.roster(vec![ADA.into(), BEA.into(), CY.into()]);
    assert!(sent(&mut ada).is_empty(), "taken once");
}

/// Cy is being removed, and Ada has applied the commit Bea has not yet: what Ada says now is
/// sealed under an epoch Bea reaches when `commit` gets to her.
fn ahead() -> (E2ee, E2ee, Vec<u8>) {
    let (server, mut ada, mut bea, _) = three();
    let remove = [server.remove_message(2, group_id(ROOM), 2)];
    let commit = bytes(&proposals(&mut ada, &remove, 2), "data");
    proposals(&mut bea, &remove, 2);
    deliver(&mut ada, "commit", 2, &commit);
    (ada, bea, commit)
}

#[test]
fn a_frame_sealed_under_the_next_epoch_is_played_once_its_commit_is_applied() {
    let (mut ada, mut bea, commit) = ahead();
    let frames: Vec<Vec<u8>> = (0..3).map(|_| ada.seal(TONE).unwrap()).collect();
    let arrived = Instant::now();
    for (at, sealed) in frames.iter().enumerate() {
        assert_eq!(bea.open_or_hold(slot_id(ADA), sealed, 960 * at as u64, arrived), None, "no key for it yet");
    }
    assert!(bea.take_late().is_empty(), "nothing opens before the commit");
    deliver(&mut bea, "commit", 2, &commit);
    let late = bea.take_late();
    let times: Vec<u64> = late.iter().map(|late| late.time).collect();
    assert_eq!(times, [0, 960, 1920], "each with its own RTP time, in order");
    assert!(late.iter().all(|late| late.id == user_id(ADA) && late.arrived == arrived && late.opus == TONE));
    assert!(bea.take_late().is_empty(), "played once");
    // What comes after opens as ever, and the wait was not held against the group.
    let next = ada.seal(TONE).unwrap();
    assert_eq!(bea.open_or_hold(slot_id(ADA), &next, 2880, Instant::now()).as_deref(), Some(TONE));
    assert!(bea.failures.is_empty());
}

#[test]
fn a_held_frame_is_dropped_when_its_commit_comes_after_the_hold() {
    let (mut ada, mut bea, commit) = ahead();
    let sealed = ada.seal(TONE).unwrap();
    let long_ago = Instant::now().checked_sub(HOLD + Duration::from_millis(50)).unwrap();
    assert_eq!(bea.open_or_hold(slot_id(ADA), &sealed, 0, long_ago), None);
    deliver(&mut bea, "commit", 2, &commit);
    assert!(bea.take_late().is_empty(), "too old to play");
    assert!(bea.held.is_empty(), "and not kept");
}

#[test]
fn held_frames_are_bounded_for_each_sender_and_never_played_in_clear_or_for_another() {
    let (mut ada, mut bea, commit) = ahead();
    let now = Instant::now();
    for at in 0..HELD as u64 * 3 {
        let sealed = ada.seal(TONE).unwrap();
        assert_eq!(bea.open_or_hold(slot_id(ADA), &sealed, 960 * at, now), None);
        // Named for the wrong sender, in clear, or for nobody: held or not, never played.
        assert_eq!(bea.open_or_hold(slot_id(CY), &sealed, 960 * at, now), None);
        assert_eq!(bea.open_or_hold(slot_id(ADA), TONE, 960 * at, now), None);
        assert_eq!(bea.open_or_hold(None, &sealed, 960 * at, now), None);
    }
    assert_eq!(bea.held.iter().filter(|held| held.id == user_id(ADA)).count(), HELD);
    assert!(bea.held.len() <= HELD * 2, "{}", bea.held.len());
    deliver(&mut bea, "commit", 2, &commit);
    let late = bea.take_late();
    let times: Vec<u64> = late.iter().map(|late| late.time).collect();
    let newest: Vec<u64> = (HELD as u64 * 2..HELD as u64 * 3).map(|at| 960 * at).collect();
    assert_eq!(times, newest, "the newest of Ada's, and nothing under Cy's name");
    assert!(late.iter().all(|late| late.id == user_id(ADA)));
}

/// A joiner hears what was said between the commit that added it and its welcome, and the wait
/// for keys is counted: what it could not send, and what it could not play.
#[test]
fn the_wait_for_keys_is_counted_each_way_and_ends_when_the_group_is_ready() {
    let server = mls::Server::new();
    let group = group_id(ROOM);
    let (mut ada, mut bea) = (app(ADA), app(BEA));
    assert_eq!(bea.seal(TONE), None, "waiting for the server");
    start(&server, &mut ada);
    let bea_package = start(&server, &mut bea);
    assert_eq!(bea.seal(TONE), None, "a group with no keys yet");
    assert_eq!(bea.seal(&SILENCE).as_deref(), Some(&SILENCE[..]));
    let commit = proposals(&mut ada, &[server.add_message(&bea_package, group, 0)], 0);
    deliver(&mut ada, "commit", 0, &bytes(&commit, "data"));
    let (first, second) = (ada.seal(TONE).unwrap(), ada.seal(TONE).unwrap());
    assert_eq!(bea.open(ADA, &first), None, "not kept: dropped");
    assert_eq!(bea.open_or_hold(slot_id(ADA), &second, 960, Instant::now()), None);
    assert_eq!(bea.keyless, (2, 2));
    assert!(!bea.keyed);
    deliver(&mut bea, "welcome", 1, &bytes(&commit, "welcome"));
    assert!(bea.keyed, "said once the group has keys");
    assert_eq!(bea.keyless, (0, 0), "and the count starts again");
    let late = bea.take_late();
    assert_eq!((late.len(), late[0].time, &late[0].opus[..]), (1, 960, TONE));
    assert!(ada.keyed && ada.keyless == (0, 0));
}

#[test]
fn proposals_wait_for_the_roster_only_so_long_and_only_so_many() {
    let server = mls::Server::new();
    let (mut ada, mut bea) = (app(ADA), app(BEA));
    ada.roster(vec![ADA.into()]);
    start(&server, &mut ada);
    let add = mls::append(&[server.add_message(&start(&server, &mut bea), group_id(ROOM), 0)]);
    let frame = json!({"type": "call.e2ee", "op": "proposals", "epoch": 0, "data": b64(&add)}).to_string();
    for _ in 0..KEPT * 2 {
        ada.frame(&frame);
    }
    assert_eq!(ada.kept.len(), KEPT);
    ada.kept.truncate(1);
    ada.kept[0].2 = Instant::now().checked_sub(KEEP + Duration::from_secs(1)).unwrap();
    ada.roster(vec![ADA.into(), BEA.into()]);
    assert!(sent(&mut ada).is_empty(), "too late: the server has built the group again by now");
    assert!(ada.kept.is_empty());
    // A new group forgets what waited for the old one.
    ada.roster(vec![ADA.into()]);
    ada.frame(&frame);
    assert_eq!(ada.kept.len(), 1);
    start(&server, &mut ada);
    assert!(ada.kept.is_empty());
}
