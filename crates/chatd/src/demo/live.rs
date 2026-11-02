//! `--seed-demo=live`: the direct conversation with Yusuf Demirci keeps moving, one thing every
//! [`STEP`], through the calls an adapter's `Context` makes.

use super::discord::{Discord, YUSUF};
use super::png::{self, Art};
use super::Who;
use crate::hub::{now, Hub};
use crate::store::Stored;
use protocol::{
    AccountKey, Attachment, Change, Key, MediaFile, MediaKind, Message, Presence, Reaction, Typist,
};
use std::path::PathBuf;
use std::time::Duration;

pub const STEP: Duration = Duration::from_secs(3);

const LINES: &[&str] = &[
    "Timestamps are fixed. It was daylight saving, twice.",
    "New build is on the camera. Motion trigger at 400 ms.",
    "It just caught the neighbour's cat. Very clear frame.",
    "Battery says 96% after a full night.",
    "I moved the sensor two centimetres left and everything changed.",
    "Can you check whether the card mounts on your machine?",
    "The marten is back. Same log, same time.",
    "Uploading last night's frames now.",
];

pub(crate) struct Plan {
    account: AccountKey,
    conversation: Key,
    /// The voice channel whose occupants change.
    voice: Key,
    yusuf: Who,
    friends: Vec<Key>,
    media: PathBuf,
    origin: String,
    /// The message this round is about.
    latest: Option<Message>,
}

impl Plan {
    pub fn new(discord: &Discord, media: PathBuf, origin: String) -> Plan {
        Plan {
            account: discord.account.clone(),
            conversation: discord.live.clone(),
            voice: discord.voice.clone(),
            yusuf: discord.cast[YUSUF].clone(),
            friends: discord.cast[..8].iter().map(|who| Key::new(&discord.account, who.id.clone())).collect(),
            media,
            origin,
            latest: None,
        }
    }

    /// A message heard on the socket, as `Context::message` records and announces it.
    fn heard(&self, hub: &Hub, message: &Message, live: bool) {
        let stored = hub.store().ingest(&self.conversation, message, live);
        if matches!(stored, Ok(Stored::New | Stored::Updated)) {
            hub.changed(Change::Messages { conversation: self.conversation.clone() });
            hub.changed(Change::Conversation { conversation: self.conversation.clone() });
            hub.changed(Change::Status);
        }
    }

    fn typing(&self, hub: &Hub, typing: bool) {
        let typist = Typist {
            id: self.yusuf.id.clone(),
            name: self.yusuf.name.clone(),
            until: now() + 8,
        };
        hub.set_typing(self.conversation.clone(), typist, typing);
    }

    /// Every other round's message carries a picture that is not on disk yet.
    fn picture(&self, hub: &Hub, round: u64) -> Option<Attachment> {
        if !round.is_multiple_of(2) {
            return None;
        }
        let file = MediaFile {
            width: 640,
            height: 400,
            ..MediaFile::new(format!("live-{round}-thumb"), MediaKind::Image)
        };
        let url = format!("{}/art/{}/{}/640/400?wait=900", self.origin, Art::Scene.letter(), round % 360);
        hub.store().register_media(&self.account, &[(file.clone(), url)]).ok()?;
        Some(Attachment {
            thumb: Some(file.clone()),
            mime: "image/png".to_owned(),
            ..Attachment::new(format!("frame-{round:04}.png"), file)
        })
    }

    /// Puts the round's picture on disk, unless the shell's own `Fetch` already did.
    fn land(&self, hub: &Hub, round: u64) {
        let Some(file) = self.latest.as_ref().and_then(|message| message.files().next()) else {
            return;
        };
        let known = hub.store().media(&self.account, std::slice::from_ref(&file.id));
        if !matches!(known.as_deref(), Ok([(MediaFile { path: None, .. }, _)])) {
            return;
        }
        let path = crate::blobs::path_of(&self.media, &self.account, &file.id);
        let bytes = png::encode(Art::Scene, (round % 360) as u32, file.width, file.height);
        if std::fs::write(&path, &bytes).is_err() {
            return;
        }
        let path = path.to_string_lossy();
        let landed = hub.store().media_landed(&self.account, &file.id, &path, bytes.len() as u64, now());
        if let Ok(Some(file)) = landed {
            hub.changed(Change::Media { account: self.account.clone(), file });
        }
    }

    pub fn step(&mut self, hub: &Hub, tick: u64) {
        let round = tick / 6;
        match tick % 6 {
            0 => self.typing(hub, true),
            1 => {
                let text = LINES[round as usize % LINES.len()];
                let id = (1_300_000_001_000_000_000 + round).to_string();
                let mut message = Message::plain(id, "", text, now(), false);
                message.sender = self.yusuf.sender();
                message.attachments.extend(self.picture(hub, round));
                self.typing(hub, false);
                self.heard(hub, &message, true);
                self.latest = Some(message);
            }
            2 => {
                let Some(message) = &self.latest else { return };
                let reaction = Reaction {
                    emoji: ["👍", "🎉", "👀"][round as usize % 3].to_owned(),
                    file: None,
                    count: 1,
                    mine: false,
                };
                let reacted = hub.store().react(&self.conversation, &message.id, &reaction, true);
                if matches!(reacted, Ok(true)) {
                    hub.changed(Change::Messages { conversation: self.conversation.clone() });
                }
            }
            3 => {
                // The stored message has the reaction; the edit must keep it.
                let Some(id) = self.latest.as_ref().map(|message| message.id.clone()) else { return };
                let Ok(Some(mut message)) = hub.store().message(&self.conversation, &id) else { return };
                message.text.push_str(" (I checked twice.)");
                message.edited = true;
                self.heard(hub, &message, false);
            }
            4 => {
                let person = self.friends[round as usize % self.friends.len()].clone();
                let presence = [Presence::Idle, Presence::Dnd, Presence::Offline, Presence::Online][round as usize % 4];
                let status = ["", "back soon", "", "at the bench"][round as usize % 4];
                hub.set_presence(person, presence, status, true);
                // And whoever is first in a voice channel mutes, or unmutes.
                if let Some(mut occupants) = hub.occupants(&self.voice) {
                    if let Some(first) = occupants.people.first_mut() {
                        first.muted = !first.muted;
                    }
                    hub.set_occupants(self.voice.clone(), occupants);
                }
            }
            _ => self.land(hub, round),
        }
    }
}
