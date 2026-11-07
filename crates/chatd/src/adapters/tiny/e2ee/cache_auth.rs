//! Authentication retained with sealed plaintext, independently of later server row claims.

use super::*;

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Authentication {
    pub conversation_id: ConversationId,
    pub group_id: ConversationId,
    pub generation: u64,
    pub epoch: u64,
    pub author: UserId,
    pub device: DeviceId,
    pub signature_key: String,
    pub kind: String,
    pub target_id: Option<String>,
}

impl Authentication {
    pub(super) fn new(
        conversation: ConversationId,
        group: ConversationId,
        generation: u64,
        epoch: u64,
        sender: &e2ee::DeviceRef,
        kind: Kind,
        target: Option<&str>,
    ) -> Self {
        Self {
            conversation_id: conversation,
            group_id: group,
            generation,
            epoch,
            author: sender.user,
            device: sender.device,
            signature_key: b64(&sender.signature_key),
            kind: kind.as_str().to_owned(),
            target_id: target.map(str::to_owned),
        }
    }

    pub(super) fn matches(&self, conversation: &str, row: &Row) -> bool {
        ConversationId::parse(conversation).ok() == Some(self.conversation_id)
            && UserId::parse(row.author_id).ok() == Some(self.author)
            && DeviceId::parse(row.device_id).ok() == Some(self.device)
            && row.generation.unwrap_or(0) == self.generation
            && row.epoch == self.epoch
            && self.kind == row.kind
            && self.target_id.as_deref() == row.target_id
            && (self.generation == 0
                || row.group_id.and_then(|id| ConversationId::parse(id).ok())
                    == Some(self.group_id))
            && unb64(&self.signature_key)
                .is_some_and(|key| key.len() == 65 && key.first() == Some(&4))
    }
}

/// Readable local history may retain a previously displayed name. This is deliberately not
/// cryptographic authentication and can never authorize an edit, repair or device approval.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct SavedAttribution {
    pub id: String,
    pub name: String,
    pub outgoing: bool,
    pub sent: i64,
    pub sent_subsec_millis: u32,
}

impl Opened {
    pub(crate) fn keep_saved_attribution(&mut self, message: &protocol::Message) -> bool {
        if self.auth.is_some()
            || self.saved_attribution.is_some()
            || message.seal != protocol::Seal::Encrypted
            || message.deleted
            || message.text != self.text
            || message.attachments.len() != self.attachments.len()
            || !message
                .attachments
                .iter()
                .zip(&self.attachments)
                .all(|(saved, opened)| {
                    saved.file.id == opened.id
                        && saved.name == opened.name
                        && saved.mime == opened.mime
                })
        {
            return false;
        }
        self.saved_attribution = Some(SavedAttribution {
            id: message.sender.id.clone(),
            name: message.sender.name.clone(),
            outgoing: message.outgoing,
            sent: message.sent,
            sent_subsec_millis: message.sent_subsec_millis,
        });
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_authentication_rejects_every_server_controlled_identity_and_context_claim() {
        let user = UserId::parse("11111111-1111-1111-1111-111111111111").unwrap();
        let engine = Engine::detached(user).unwrap();
        let conversation = ConversationId::parse("22222222-2222-2222-2222-222222222222").unwrap();
        let group = ConversationId::parse("33333333-3333-3333-3333-333333333333").unwrap();
        let auth = Authentication::new(
            conversation,
            group,
            2,
            7,
            &engine.device_ref(),
            Kind::Edit,
            Some("target"),
        );
        let author = user.to_string();
        let device = engine.device_id().to_string();
        let group = group.to_string();
        let mut row = Row {
            id: "edit",
            author_id: &author,
            device_id: &device,
            generation: Some(2),
            group_id: Some(&group),
            epoch: 7,
            kind: "edit",
            target_id: Some("target"),
            ciphertext: None,
            client_id: None,
            created_at: "",
        };
        assert!(auth.matches(&conversation.to_string(), &row));
        for claim in 0..8 {
            let (author, device, generation, group, epoch, kind, target) = (
                row.author_id,
                row.device_id,
                row.generation,
                row.group_id,
                row.epoch,
                row.kind,
                row.target_id,
            );
            match claim {
                0 => row.author_id = "44444444-4444-4444-4444-444444444444",
                1 => row.device_id = "55555555-5555-5555-5555-555555555555",
                2 => row.generation = Some(1),
                3 => row.group_id = Some("66666666-6666-6666-6666-666666666666"),
                4 => row.epoch = 8,
                5 => row.kind = "message",
                6 => row.target_id = Some("another-target"),
                _ => row.generation = None,
            }
            assert!(
                !auth.matches(&conversation.to_string(), &row),
                "claim {claim}"
            );
            row.author_id = author;
            row.device_id = device;
            row.generation = generation;
            row.group_id = group;
            row.epoch = epoch;
            row.kind = kind;
            row.target_id = target;
        }
        assert!(!auth.matches("77777777-7777-7777-7777-777777777777", &row));
    }

    #[test]
    fn saved_attribution_requires_matching_preexisting_readable_local_content_and_never_becomes_auth(
    ) {
        let mut opened = Opened {
            text: "saved text".into(),
            ..Opened::default()
        };
        let mut previous = protocol::Message::plain("id", "Ada", "saved text", 123, false);
        previous.sender.id = "ada".into();
        assert!(
            !opened.keep_saved_attribution(&previous),
            "plaintext rows provide no encrypted-history provenance"
        );
        previous.seal = protocol::Seal::Undecryptable;
        assert!(!opened.keep_saved_attribution(&previous));
        previous.seal = protocol::Seal::Encrypted;
        previous.text = "different".into();
        assert!(!opened.keep_saved_attribution(&previous));
        previous.text = "saved text".into();
        assert!(opened.keep_saved_attribution(&previous));
        assert_eq!(opened.saved_attribution.as_ref().unwrap().name, "Ada");
        assert!(opened.auth.is_none());
        let restored: Opened =
            serde_json::from_value(serde_json::to_value(&opened).unwrap()).unwrap();
        assert!(
            restored.auth.is_none(),
            "archive round trips preserve the lower-trust class"
        );
        assert_eq!(restored.saved_attribution.as_ref().unwrap().id, "ada");
    }
}
