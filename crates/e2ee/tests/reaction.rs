//! The reaction's authenticated data after a repair: the ciphertext rides in the group the
//! binding named, while its authenticated data still names the conversation it was posted to.
//! The server checks both words at once (`docs/e2ee-text.md`, section 8); after a repair the
//! two ids differ, and a reaction can then never be replayed onto another conversation that
//! shares the group. The receiver's refusal of a wrong conversation is covered with a real
//! receiver in `groups.rs` (`edits_are_bound_to_their_kind_target_and_author`).

use e2ee::engine::Engine;
use e2ee::envelope::{Envelope, Kind};
use e2ee::store::{FileStore, StateKey};
use e2ee::{ConversationId, UserId};
use openmls::prelude::tls_codec::DeserializeBytes as _;
use openmls::prelude::{MlsMessageIn, ProtocolMessage};
use tempfile::tempdir;

fn words(conversation: &ConversationId) -> Vec<u8> {
    let mut aad = b"tiny-e2ee-v1 msg".to_vec();
    aad.push(0);
    aad.extend_from_slice(b"reaction");
    aad.push(0);
    aad.push(0);
    aad.extend_from_slice(conversation.to_string().as_bytes());
    aad
}

/// What a ciphertext says in the clear: which group it rides in, and what its sender signed.
fn said(ciphertext: &[u8]) -> ([u8; 16], Vec<u8>) {
    let message = MlsMessageIn::tls_deserialize_exact_bytes(ciphertext).unwrap();
    let protocol = message.try_into_protocol_message().unwrap();
    let ProtocolMessage::PrivateMessage(private) = &protocol else {
        panic!("a reaction ciphertext is a PrivateMessage");
    };
    let mut group = [0u8; 16];
    group.copy_from_slice(protocol.group_id().as_slice());
    (group, private.aad().to_vec())
}

#[test]
fn a_reaction_names_its_conversation_whatever_group_it_rides_in() {
    let dir = tempdir().unwrap();
    let user = UserId::random().unwrap();
    let mut alice = Engine::create(
        user,
        Box::new(FileStore::new(dir.path())),
        StateKey::generate().unwrap(),
        "e2ee-reaction-binding",
    )
    .unwrap();
    // The conversation as everyone knows it, and the group a repair moved it to: two groups of
    // one device (this one), like the generation-zero group and a binding's group.
    let conversation = ConversationId::random().unwrap();
    let repaired = ConversationId::random().unwrap();
    let key = alice.signature_key();
    alice.create_group(conversation, &key).unwrap();
    alice.create_group(repaired, &key).unwrap();

    let envelope = Envelope {
        text: "\u{1f44d}".into(),
        reply_to: Some("m-1".into()),
        on: true,
        ..Envelope::default()
    };

    // What a reaction after a repair must say: the group it rides in is the binding's, the
    // conversation it names is the one everyone knows.
    let sealed = alice
        .encrypt_in(repaired, &conversation, Kind::Reaction, None, &envelope)
        .unwrap();
    let (group, aad) = said(&sealed.ciphertext);
    assert_eq!(&group, repaired.as_bytes(), "the ciphertext rides in the repaired group");
    assert_eq!(aad, words(&conversation), "the authenticated data names the conversation");

    // The generation-zero words still hold: a plain encrypt binds the group's own conversation.
    let plain = alice.encrypt(repaired, Kind::Reaction, None, &envelope).unwrap();
    let (group, aad) = said(&plain.ciphertext);
    assert_eq!(&group, repaired.as_bytes());
    assert_eq!(aad, words(&repaired));
}
