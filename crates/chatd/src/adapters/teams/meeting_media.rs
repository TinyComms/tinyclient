//! Recording notices and the transcript file attached to them.

use super::html::{self, Token};
use super::{bare, graph_content, https_host, seconds, within, Audience, Auth, Body, Fault, Teams};
use protocol::{Card, CardAction, MediaFile, MediaKind, Message};
use serde_json::Value;
use std::time::{Duration, Instant};

const MAX_TRANSCRIPT: usize = 512 * 1024;

#[derive(Default)]
struct Notice {
    title: String,
    status: String,
    initiator: String,
    call_id: String,
    start: i64,
    duration: u32,
    thumbnail: String,
    video: String,
    transcript: String,
    link: String,
}

fn notice(mut body: &str) -> Notice {
    let mut out = Notice::default();
    let mut in_title = false;
    while let Some((token, rest)) = html::next(body) {
        body = rest;
        match token {
            Token::Open { name, attrs, .. } => {
                let attr = |key| html::attr(&attrs, key);
                match name.as_str() {
                    "title" => in_title = true,
                    "uriobject" => out.thumbnail = attr("thumbnail").to_owned(),
                    "a" if out.link.is_empty() => out.link = attr("href").to_owned(),
                    "recordingstatus" | "transcriptionstatus" => {
                        out.status = attr("status").to_ascii_lowercase()
                    }
                    "recordinginitiatorid" => out.initiator = attr("value").to_owned(),
                    "id" if attr("type").eq_ignore_ascii_case("callId") => {
                        out.call_id = attr("value").to_owned()
                    }
                    "recordingcontent" => {
                        out.start = seconds(attr("timestamp"));
                        let raw = attr("duration");
                        // Older notices use the .NET TimeSpan day.hour form.
                        let raw = raw
                            .strip_prefix("0.")
                            .filter(|s| s.contains(':'))
                            .unwrap_or(raw);
                        out.duration = html::parse_duration_secs(raw, false);
                    }
                    "item" => match attr("type").to_ascii_lowercase().as_str() {
                        "onedriveforbusinessvideo" => out.video = attr("uri").to_owned(),
                        "onedriveforbusinesstranscript" | "transcript" => {
                            out.transcript = attr("uri").to_owned()
                        }
                        _ => {}
                    },
                    _ => {}
                }
            }
            Token::Text(text) if in_title => out.title.push_str(&html::decoded(text)),
            Token::Close(name) if name == "title" => in_title = false,
            Token::Close(name) if name == "uriobject" => break,
            _ => {}
        }
    }
    out.title = out.title.trim().to_owned();
    out
}

impl Teams {
    fn sharepoint_link(&self, url: &str) -> bool {
        let Some(host) = https_host(url) else {
            return false;
        };
        url.parse::<ureq::http::Uri>()
            .ok()
            .and_then(|u| u.authority().cloned())
            .is_some_and(|a| a.as_str() == host)
            && self
                .cloud
                .endpoints()
                .sharepoint
                .iter()
                .any(|domain| within(host, domain))
    }

    pub(super) fn meeting_media(
        &self,
        conversation: &str,
        message: &mut Message,
        content: &str,
        transcript: bool,
        fallback: &str,
    ) {
        let notice = notice(content);
        let transcript_notice = if transcript && notice.call_id.is_empty() {
            serde_json::from_str::<Value>(content)
                .or_else(|_| serde_json::from_str(&content.replace(r#"\""#, "\"")))
                .unwrap_or_default()
        } else {
            Value::Null
        };
        let call_id = transcript_notice["callId"]
            .as_str()
            .unwrap_or(&notice.call_id);
        let call_id = (!call_id.is_empty()
            && call_id.len() <= 100
            && call_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-'))
        .then_some(call_id);
        let label = if transcript {
            "Meeting transcript"
        } else {
            "Meeting recording"
        };
        message.text = label.to_owned();
        let status = match notice.status.as_str() {
            "started" | "starting" | "inprogress" => "Started",
            "success" | "completed" | "saved" => "Saved",
            "expired" | "deleted" => "Expired",
            "failed" | "error" => "Unavailable",
            _ => "",
        };
        let open = if transcript {
            [&notice.transcript, &notice.link]
        } else {
            [&notice.video, &notice.link]
        };
        let open = open.into_iter().find(|link| self.sharepoint_link(link));
        if let Some(url) = (!notice.transcript.is_empty()
            && self.sharepoint_link(&notice.transcript))
        .then_some(&notice.transcript)
        .or_else(|| if transcript { open } else { None })
        {
            message.meeting_transcript = Some(url.to_owned());
            if !transcript {
                if let Some(call_id) = call_id {
                    self.ctx
                        .set_note(&format!("transcript:{conversation}:{call_id}"), &message.id);
                }
            }
        } else if transcript {
            message.meeting_transcript = call_id.map(|call_id| format!("call:{call_id}"));
        }
        let mut card = Card {
            title: if notice.title.is_empty() {
                label.to_owned()
            } else {
                notice.title
            },
            time: Some(notice.start)
                .filter(|time| *time > 0)
                .or(Some(message.sent).filter(|time| *time > 0)),
            ..Card::default()
        };
        if !status.is_empty() {
            card.fields.push(("Status".to_owned(), status.to_owned()));
        }
        if notice.duration > 0 {
            let secs = notice.duration;
            card.fields.push((
                "Duration".to_owned(),
                if secs >= 3600 {
                    format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
                } else {
                    format!("{}:{:02}", secs / 60, secs % 60)
                },
            ));
        }
        if !transcript {
            let initiator = self
                .name_of(&notice.initiator)
                .unwrap_or_else(|| message.sender.name.clone());
            if !initiator.is_empty() {
                card.fields.push(("Recorded by".to_owned(), initiator));
            }
            if self.pictures(&notice.thumbnail) {
                let thumbnail = MediaFile::new(
                    format!("recording:{}:thumbnail", message.id),
                    MediaKind::Image,
                );
                if self
                    .ctx
                    .media(&[(thumbnail.clone(), notice.thumbnail)])
                    .is_ok()
                {
                    card.thumbnail = Some(thumbnail);
                }
            }
        }
        if let Some(url) = open.filter(|_| status != "Expired" && status != "Started") {
            let label = if transcript {
                "Open in browser"
            } else {
                "Open recording"
            };
            card.actions.push(CardAction {
                label: label.to_owned(),
                url: url.to_owned(),
            });
        } else if fallback.starts_with("https://") {
            card.actions.push(CardAction {
                label: "Open in Teams".to_owned(),
                url: fallback.to_owned(),
            });
        }
        message.card = Some(Box::new(card));
    }

    pub(super) fn meeting_transcript(
        &self,
        conversation: &str,
        message_id: &str,
    ) -> Result<String, Fault> {
        let source = self
            .ctx
            .stored_message(conversation, message_id)?
            .and_then(|m| m.meeting_transcript);
        let source = if let Some(call) = source
            .as_deref()
            .and_then(|source| source.strip_prefix("call:"))
        {
            match self.ctx.note(&format!("transcript:{conversation}:{call}")) {
                Some(recording) => self
                    .ctx
                    .stored_message(conversation, &recording)?
                    .and_then(|m| m.meeting_transcript),
                None => None,
            }
        } else {
            source
        };
        let source = source
            .filter(|url| self.sharepoint_link(url))
            .ok_or_else(|| unavailable("There is no transcript file for this message."))?;
        let direct_api =
            bare(&source).contains("/_api/v2.1/") && bare(&source).ends_with("/content");
        let text = if direct_api {
            self.sharepoint_transcript(&source)?
        } else {
            let url = graph_content(&self.graph, &source);
            let mut headers = self.headers(Auth::Bearer(Audience::Graph))?;
            headers.push(("Prefer", "redeemSharingLinkIfNecessary".to_owned()));
            if self.pace() {
                return Err(Fault::Net("transcript: stopped".to_owned()));
            }
            let response = self
                .send("GET", &url, &headers, &Body::None, None)
                .map_err(|_| unavailable("The transcript could not be reached."))?;
            self.transcript_response(response)?
        };
        let mut text = readable(&text);
        text.retain(|c| !c.is_control() || matches!(c, '\n' | '\t'));
        // Keep the response within the IPC frame limit even after JSON escaping.
        protocol::clip(&mut text, 400_000);
        if text.is_empty() {
            return Err(unavailable("The transcript contains no readable text."));
        }
        Ok(text)
    }

    fn transcript_response(
        &self,
        mut response: ureq::http::Response<ureq::Body>,
    ) -> Result<String, Fault> {
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get("location")
                .and_then(|value| value.to_str().ok())
                .filter(|url| self.sharepoint_link(url))
                .ok_or_else(|| unavailable("The transcript file could not be opened."))?;
            // Graph's redirect is preauthenticated. Only the address crosses to SharePoint.
            response = self
                .agent
                .get(location)
                .call()
                .map_err(|_| unavailable("The transcript file could not be opened."))?;
        }
        if !response.status().is_success() {
            return Err(unavailable(
                "Your account could not open this transcript. Try opening it in Teams.",
            ));
        }
        response
            .body_mut()
            .with_config()
            .limit(MAX_TRANSCRIPT as u64)
            .read_to_string()
            .map_err(|_| unavailable("The transcript is too large or could not be read."))
    }

    fn sharepoint_transcript(&self, source: &str) -> Result<String, Fault> {
        let host = https_host(source)
            .ok_or_else(|| unavailable("Invalid SharePoint transcript address."))?;
        let (site, path) = bare(source)
            .split_once("/_api/v2.1/")
            .ok_or_else(|| unavailable("Invalid SharePoint transcript address."))?;
        let parts: Vec<_> = path.split('/').collect();
        let ["drives", drive, "items", item, "versions", "current", "media", "transcripts", transcript, "content"] =
            parts.as_slice()
        else {
            return Err(unavailable("Invalid SharePoint transcript address."));
        };
        if ![drive, item, transcript].iter().all(|p| {
            !matches!(**p, "" | "." | "..")
                && p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"!-._".contains(&b))
        }) {
            return Err(unavailable("Invalid SharePoint transcript address."));
        }
        let audience = format!("https://{host}");
        let access = self.transcript_access(&audience)?;
        let endpoint = format!("{site}/_api/v2.1/drives/{drive}/items/{item}?select=media/transcripts&$expand=media/transcripts");
        if self.pace() {
            return Err(Fault::Net("transcript: stopped".to_owned()));
        }
        let mut metadata = self
            .agent
            .get(&endpoint)
            .header("Accept", "application/json")
            .header("Authorization", format!("Bearer {access}"))
            .call()
            .map_err(|_| unavailable("The SharePoint transcript could not be reached."))?;
        if metadata.status().is_success() {
            let metadata = metadata
                .body_mut()
                .with_config()
                .limit(MAX_TRANSCRIPT as u64)
                .read_json::<Value>()
                .unwrap_or_default();
            let download = metadata["media"]["transcripts"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|row| row["id"] == *transcript)
                .and_then(|row| row["temporaryDownloadUrl"].as_str())
                .filter(|url| self.sharepoint_link(url));
            if let Some(url) = download {
                let separator = if url.contains('?') { '&' } else { '?' };
                // A temporary download URL is preauthenticated. Do not forward the bearer.
                let response = self
                    .agent
                    .get(format!("{url}{separator}format=json"))
                    .call()
                    .map_err(|_| unavailable("The SharePoint transcript could not be reached."))?;
                return self.transcript_response(response);
            }
        }
        if self.pace() {
            return Err(Fault::Net("transcript: stopped".to_owned()));
        }
        let response = self
            .agent
            .get(source)
            .header("Authorization", format!("Bearer {access}"))
            .call()
            .map_err(|_| unavailable("The SharePoint transcript could not be reached."))?;
        self.transcript_response(response)
    }

    fn transcript_access(&self, audience: &str) -> Result<String, Fault> {
        if let Some((known, token)) = &self.state().transcript_token {
            if known == audience && token.fresh() {
                return Ok(token.value.clone());
            }
        }
        let _one = crate::hub::lock(&self.minting.transcript);
        if let Some((known, token)) = &self.state().transcript_token {
            if known == audience && token.fresh() {
                return Ok(token.value.clone());
            }
        }
        let refresh = self.state().refresh.clone();
        let url = format!(
            "{}/{}/oauth2/v2.0/token",
            self.login,
            super::escaped(&self.session.tenant)
        );
        let scopes = format!("{audience}/.default openid profile offline_access");
        if self.pace() {
            return Err(Fault::Net("transcript token: stopped".to_owned()));
        }
        let mut answer = self
            .agent
            .post(&url)
            .send_form([
                ("client_id", self.session.client_id.as_str()),
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh.as_str()),
                ("scope", scopes.as_str()),
            ])
            .map_err(|_| unavailable("SharePoint sign-in could not be reached."))?;
        if !answer.status().is_success() {
            return Err(unavailable("Your account could not sign in to SharePoint. Try opening the transcript in Teams."));
        }
        let value: Value = answer
            .body_mut()
            .read_json()
            .map_err(|_| unavailable("SharePoint sign-in returned an invalid response."))?;
        let access = value["access_token"]
            .as_str()
            .ok_or_else(|| unavailable("SharePoint did not grant transcript access."))?;
        let token = super::Token {
            value: access.to_owned(),
            until: Instant::now()
                + Duration::from_secs(
                    value["expires_in"]
                        .as_u64()
                        .unwrap_or(3600)
                        .clamp(60, 86_400),
                ),
        };
        let next = value["refresh_token"]
            .as_str()
            .filter(|next| !next.is_empty() && *next != refresh);
        let mut state = self.state();
        state.transcript_token = Some((audience.to_owned(), token));
        if let Some(next) = next {
            state.refresh = next.to_owned();
            drop(state);
            self.ctx
                .save_session(|stored| super::renewed(stored, &self.session.tenant, next));
        }
        Ok(access.to_owned())
    }
}

fn readable(source: &str) -> String {
    let source = source.trim_start_matches('\u{feff}').trim();
    if source.starts_with('{') {
        let value: Value = serde_json::from_str(source).unwrap_or_default();
        let lines = value["entries"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|entry| {
                let text = entry["text"].as_str()?.trim();
                if text.is_empty() {
                    return None;
                }
                let speaker = entry["speakerDisplayName"].as_str().unwrap_or("").trim();
                Some(if speaker.is_empty() {
                    text.to_owned()
                } else {
                    format!("{speaker}: {text}")
                })
            });
        return lines
            .take(protocol::TRANSCRIPT_LINES_LIMIT)
            .collect::<Vec<_>>()
            .join("\n");
    }
    if !source.starts_with("WEBVTT") && !source.contains("-->") {
        return if source.starts_with(['<', '{', '[']) {
            String::new()
        } else {
            source.to_owned()
        };
    }
    let mut vtt = String::with_capacity(source.len());
    for line in source.lines() {
        let line = line.trim_start();
        if let Some((speaker, words)) = line
            .strip_prefix("<v ")
            .and_then(|line| line.split_once('>'))
        {
            vtt.push_str(speaker);
            vtt.push_str(": ");
            vtt.push_str(words);
        } else {
            vtt.push_str(line);
        }
        vtt.push('\n');
    }
    protocol::parse_webvtt(&vtt)
        .into_iter()
        .map(|cue| cue.text)
        .collect::<Vec<_>>()
        .join("\n")
}

fn unavailable(message: &str) -> Fault {
    Fault::Refused {
        status: 404,
        message: message.to_owned(),
    }
}
