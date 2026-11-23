//! The account's call stream and voicemail in its Exchange mailbox.

use super::{escaped, millis, seconds, Audience, Auth, Body, Fault, Teams};
use crate::adapters::CallHistoryPage;
use base64::Engine;
use protocol::{
    CallLogEntry, CallNote, CallOutcome, MediaFile, MediaKind, Message, MessageKind, Voicemail,
    VoicemailAction,
};
use serde_json::{json, Value};

const CALL_LOG: &str = "48:calllogs";
const INBOX: &str = "/v1.0/me/mailFolders/inbox/messages";
const MESSAGE_CLASS: &str = "String 0x001A";
const VOICEMAIL_CLASS: &str = "IPM.Note.Microsoft.Voicemail.UM";

impl Teams {
    pub(super) fn call_history(
        &self,
        missed: bool,
        before: Option<&str>,
        limit: u32,
    ) -> Result<CallHistoryPage, Fault> {
        let window = match before {
            None => "startTime=1".to_owned(),
            Some(cursor)
                if !cursor.is_empty()
                    && cursor.len() <= 2048
                    && cursor
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"-._~%+/=".contains(&c)) =>
            {
                format!("startTime=1&syncState={cursor}")
            }
            Some(_) => return Err(bad("Invalid call history cursor.")),
        };
        let (raws, older) = self.page(CALL_LOG, limit, &window)?;
        let me = self.me();
        let mut entries = Vec::new();
        for raw in raws.iter().take(limit as usize) {
            let Some(entry) = self.call_entry(raw, &me)? else {
                continue;
            };
            let incoming_missed = !entry.outgoing && entry.note.outcome == CallOutcome::Missed;
            let unread = raw["isRead"] == false
                || raw["properties"]["isRead"] == false
                || raw["properties"]["isRead"] == "false";
            if incoming_missed && before.is_none() && unread && !entry.conversation.id.is_empty() {
                let mut line = Message::plain(
                    &entry.message_id,
                    &entry.name,
                    format!("Missed call from {}", entry.name),
                    entry.time,
                    false,
                );
                line.kind = MessageKind::System;
                line.sender.id.clone_from(&entry.person_id);
                line.call = Some(entry.note.clone());
                self.ctx.quiet_line(&entry.conversation.id, &line, true)?;
            }
            if !missed || incoming_missed {
                entries.push(entry);
            }
        }
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.time));
        let more = !older.is_empty() && before != Some(older.as_str());
        Ok(CallHistoryPage {
            entries,
            next: more.then_some(older),
            more,
        })
    }

    fn call_entry(&self, raw: &Value, me: &str) -> Result<Option<CallLogEntry>, Fault> {
        let content = &raw["properties"]["call-log"];
        let parsed = content
            .as_str()
            .and_then(|s| serde_json::from_str::<Value>(s).ok());
        let content = parsed.as_ref().unwrap_or(content);
        let id = super::id_of(&raw["id"]);
        let caller = party(&content["originatorParticipant"])
            .or_else(|| content["originator"].as_str())
            .unwrap_or_default();
        let callee = party(&content["targetParticipant"])
            .or_else(|| content["target"].as_str())
            .unwrap_or_default();
        let outgoing = match field(content, &["direction", "callDirection"])
            .to_ascii_lowercase()
            .as_str()
        {
            "outgoing" | "outbound" => true,
            "incoming" | "inbound" => false,
            _ => !caller.is_empty() && caller == me,
        };
        let peer = if outgoing { callee } else { caller };
        let outcome = match field(content, &["callState"]).to_ascii_lowercase().as_str() {
            "missed" | "noanswer" | "unanswered" => CallOutcome::Missed,
            "declined" | "rejected" => CallOutcome::Declined,
            "cancelled" | "canceled" => CallOutcome::Cancelled,
            "busy" => CallOutcome::Busy,
            "accepted" if content["forwardingInfo"]["acceptedBy"]["type"] == "voicemail" => {
                CallOutcome::Voicemail
            }
            "accepted" | "answered" | "completed" | "ended" => CallOutcome::Ended,
            "voicemail" => CallOutcome::Voicemail,
            _ => return Ok(None),
        };
        let mut time = seconds(field(content, &["startTime", "startDateTime"]));
        if time <= 0 {
            time = seconds(field(raw, &["composetime", "originalarrivaltime"]))
                .max(millis(&id) / 1000);
        }
        if id.is_empty() || time <= 0 {
            return Ok(None);
        }
        let thread = field(content, &["threadId"]);
        let thread = if thread.is_empty() {
            self.state().partners.get(peer).cloned().unwrap_or_default()
        } else {
            thread.to_owned()
        };
        let row = if thread.starts_with("19:") || thread.starts_with("8:") {
            self.ctx.known(&thread)?
        } else {
            None
        };
        let display = if outgoing {
            &content["targetParticipant"]
        } else {
            &content["originatorParticipant"]
        };
        let name = row
            .as_ref()
            .map(|row| row.title.as_str())
            .filter(|name| !name.is_empty())
            .or_else(|| display["displayName"].as_str())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| {
                if peer.is_empty() {
                    "Unknown caller"
                } else {
                    peer
                }
            });
        let connected = seconds(field(content, &["connectTime"]));
        let ended = seconds(field(content, &["endTime"]));
        let duration = (connected > 0 && ended >= connected)
            .then(|| u32::try_from(ended - connected).ok())
            .flatten();
        let target = row.as_ref().map_or("", |row| row.key.id.as_str());
        Ok(Some(CallLogEntry {
            conversation: self.ctx.key(target),
            person_id: peer.to_owned(),
            name: name.to_owned(),
            message_id: id,
            outgoing,
            note: CallNote {
                outcome,
                caller_id: caller.to_owned(),
                seconds: duration,
                participants: content["participants"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(party)
                    .take(32)
                    .map(str::to_owned)
                    .collect(),
                video: content["isVideo"] == true,
                call_id: content["callId"].as_str().map(str::to_owned),
            },
            time,
        }))
    }

    pub(super) fn voicemails(
        &self,
        before: Option<&str>,
    ) -> Result<(Vec<Voicemail>, Option<String>), Fault> {
        let endpoint = format!("{}{INBOX}", self.graph);
        let valid = |link: &str| {
            link.len() <= 4096
                && link
                    .strip_prefix(&endpoint)
                    .is_some_and(|tail| tail.starts_with('?') && !tail.contains('#'))
        };
        let url = match before {
            Some(link) if valid(link) && self.state().voicemail_next.as_deref() == Some(link) => {
                link.to_owned()
            }
            Some(_) => return Err(bad("Invalid voicemail cursor.")),
            None => {
                let filter = escaped(&format!("singleValueExtendedProperties/any(p:p/id eq '{MESSAGE_CLASS}' and p/value eq '{VOICEMAIL_CLASS}')"));
                let expand = escaped("attachments($select=id,contentType)");
                format!("{endpoint}?$top=24&$filter={filter}&$select=id,subject,from,receivedDateTime,isRead,bodyPreview&$expand={expand}")
            }
        };
        let body = self
            .call(
                "GET",
                &url,
                Auth::Bearer(Audience::Graph),
                Body::None,
                false,
            )?
            .body;
        let entries = body["value"]
            .as_array()
            .ok_or_else(|| bad("The mailbox did not return a voicemail list."))?
            .iter()
            .take(24)
            .filter_map(voicemail)
            .collect();
        let next = body["@odata.nextLink"]
            .as_str()
            .filter(|s| valid(s) && Some(*s) != before)
            .map(str::to_owned);
        self.state().voicemail_next.clone_from(&next);
        Ok((entries, next))
    }

    fn voicemail_item(&self, id: &str) -> Result<String, Fault> {
        if id.is_empty() || id.len() > 1024 || matches!(id, "." | "..") {
            return Err(bad("Invalid voicemail id."));
        }
        let url = format!("{}/v1.0/me/messages/{}", self.graph, escaped(id));
        let expand = escaped(&format!(
            "singleValueExtendedProperties($filter=id eq '{MESSAGE_CLASS}')"
        ));
        let body = self
            .call(
                "GET",
                &format!("{url}?$select=id&$expand={expand}"),
                Auth::Bearer(Audience::Graph),
                Body::None,
                false,
            )?
            .body;
        let voicemail = body["singleValueExtendedProperties"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|p| p["id"] == MESSAGE_CLASS && p["value"] == VOICEMAIL_CLASS);
        if !voicemail {
            return Err(bad("This mailbox item is not a Teams voicemail."));
        }
        Ok(url)
    }

    pub(super) fn change_voicemail(&self, id: &str, action: VoicemailAction) -> Result<(), Fault> {
        let url = self.voicemail_item(id)?;
        let patch = json!({"isRead": true});
        let (method, body) = match action {
            VoicemailAction::MarkRead => ("PATCH", Body::Json(&patch)),
            VoicemailAction::Delete => ("DELETE", Body::None),
        };
        self.call(method, &url, Auth::Bearer(Audience::Graph), body, false)
            .map(|_| ())
    }

    pub(super) fn voicemail_audio(&self, id: &str, attachment: &str) -> Result<MediaFile, Fault> {
        if attachment.is_empty() || attachment.len() > 1024 || matches!(attachment, "." | "..") {
            return Err(bad("Invalid voicemail attachment."));
        }
        let item = self.voicemail_item(id)?;
        let address = format!("{item}/attachments/{}", escaped(attachment));
        let body = self
            .call(
                "GET",
                &address,
                Auth::Bearer(Audience::Graph),
                Body::None,
                false,
            )?
            .body;
        let encoded = body["contentBytes"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 8 * 1024 * 1024)
            .ok_or_else(|| bad("Voicemail audio is unavailable."))?;
        if !is_audio(&body) {
            return Err(bad("The voicemail attachment is not audio."));
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| bad("Voicemail audio is invalid."))?;
        let file = MediaFile::new(
            format!("voicemail:{}:{}", escaped(id), escaped(attachment)),
            MediaKind::Audio,
        );
        self.ctx
            .landed(&file, &address, &bytes)
            .ok_or_else(|| bad("Voicemail audio could not be saved."))
    }
}

fn voicemail(item: &Value) -> Option<Voicemail> {
    let id = item["id"].as_str().filter(|s| !s.is_empty())?;
    let sender = &item["from"]["emailAddress"];
    let caller = field(sender, &["name", "address"]);
    let caller = if caller.is_empty() {
        field(item, &["subject"])
    } else {
        caller
    };
    let audio = item["attachments"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|item| is_audio(item))
        .and_then(|item| item["id"].as_str())
        .map(str::to_owned);
    Some(Voicemail {
        id: id.to_owned(),
        caller: caller.chars().take(200).collect(),
        time: seconds(field(item, &["receivedDateTime"])),
        transcript: field(item, &["bodyPreview"]).chars().take(2000).collect(),
        read: item["isRead"] == true,
        audio,
    })
}

fn is_audio(item: &Value) -> bool {
    item["@odata.type"] == "#microsoft.graph.fileAttachment"
        && item["contentType"]
            .as_str()
            .is_some_and(|mime| mime.starts_with("audio/"))
}

fn party(value: &Value) -> Option<&str> {
    value
        .as_str()
        .or_else(|| value["id"].as_str())
        .or_else(|| value["mri"].as_str())
}

fn field<'a>(value: &'a Value, keys: &[&str]) -> &'a str {
    keys.iter()
        .filter_map(|key| value[key].as_str())
        .find(|text| !text.is_empty())
        .unwrap_or_default()
}

fn bad(message: &str) -> Fault {
    Fault::Refused {
        status: 400,
        message: message.to_owned(),
    }
}
