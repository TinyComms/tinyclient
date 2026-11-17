//! The Google Messages phone relay, written from the published field layout of the Bugle RPC
//! (pairing, long-poll, list, send). The phone stays the account: this computer is one more
//! paired device, and every message goes through the phone. Nothing here is the official client.

use aes::Aes256;
mod google;
use base64::Engine;
use cipher::{KeyIvInit, StreamCipher};
use ctr::Ctr128BE;
pub use google::pair_google;
use hmac::{Hmac, Mac};
use p256::ecdsa::SigningKey;
use p256::pkcs8::EncodePublicKey;
use sha2::Sha256;
use std::io::Read;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const API_KEY: &str = "AIzaSyCA4RsOZUFrm9whhtGosPlJLmVPnfSHKz8";
const USER_AGENT: &str = "Mozilla/5.0 (Linux; Android 14) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/152.0.0.0 Safari/537.36";
const ORIGIN: &str = "https://messages.google.com";
const RELAY: &str = "https://instantmessaging-pa.googleapis.com/$rpc/google.internal.communications.instantmessaging.v1";
const UPLOAD: &str = "https://instantmessaging-pa.googleapis.com/upload";
/// A guard for this client, not a limit Google publishes.
const FILE_CAP: u64 = 100 * 1024 * 1024;
const NETWORK: &str = "Bugle";
/// How long one request waits for the phone to answer on the long-poll.
const PHONE: Duration = Duration::from_secs(20);
/// One inbox page. `ListConversationsRequest.count`, field 2.
pub(super) const CHAT_PAGE: u64 = 50;
/// One conversation page. `ListMessagesRequest.count`, field 3.
pub(super) const MESSAGE_PAGE: u64 = 40;
/// What a request says when that wait runs out. The relay itself can still be up.
pub const PHONE_QUIET: &str =
    "Your phone isn't responding. Keep Messages open, and the phone online.";
/// The account shows this when the phone unpaired this computer, or the pairing expired.
pub const UNPAIRED: &str = "Your phone unpaired this computer. Pair again.";
/// The quiet poll starts here and doubles after a failed refresh, connect, or refused status, up to this.
const POLL_FLOOR: Duration = Duration::from_secs(2);
const POLL_CEILING: Duration = Duration::from_secs(900);

/// Why one poll stopped. A network failure is not an unpaired phone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// The relay refused the session (401 or 403).
    Auth,
    /// The network, or any other status. A retry can survive it.
    Closed,
}

/// What to do after one stop. Two auth refusals in a row end the session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Retry,
    End,
}

/// Consecutive auth refusals, kept across reconnects. One can be a blip.
#[derive(Default)]
pub struct Gate {
    auth: u32,
}

impl Gate {
    pub fn observe(&mut self, stop: Stop) -> Step {
        match stop {
            Stop::Auth => {
                self.auth = self.auth.saturating_add(1);
                if self.auth >= 2 {
                    Step::End
                } else {
                    Step::Retry
                }
            }
            Stop::Closed => {
                self.auth = 0;
                Step::Retry
            }
        }
    }

    /// A 401, a 403, or the cookie refresh giving up counts as auth. Two in a row end the session.
    /// One `Sign in again` can be a Google hiccup, so it retries.
    pub fn fail(&mut self, words: &str) -> crate::adapters::Failure {
        let auth = words.contains("(401)") || words.contains("(403)") || cookie_failure_ends(words);
        let stop = if auth { Stop::Auth } else { Stop::Closed };
        match self.observe(stop) {
            Step::End => crate::adapters::Failure::SignedOut(UNPAIRED.to_owned()),
            Step::Retry => crate::adapters::Failure::Dropped(words.to_owned()),
        }
    }
}

pub fn cookie_failure_ends(words: &str) -> bool {
    words.contains("Sign in again")
}

pub fn http_stop(status: u16) -> Stop {
    match status {
        401 | 403 => Stop::Auth,
        _ => Stop::Closed,
    }
}

pub fn next_pause(pause: Duration) -> Duration {
    pause.saturating_mul(2).min(POLL_CEILING)
}

/// The long-poll's own close. The 90-second global timeout cuts a quiet healthy stream
/// with no payload; that is not an outage, so the pause stays at the floor.
fn stream_exit_failed() -> bool {
    false
}

type AesCtr = Ctr128BE<Aes256>;
type HmacSha = Hmac<Sha256>;

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn unb64(text: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::STANDARD
        .decode(text.trim())
        .ok()
}

fn random(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    getrandom::fill(&mut buf).expect("the operating system random source");
    buf
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).ok())
        .collect()
}

/// AES-256-CTR then HMAC-SHA256 over `ciphertext || iv`. The wire is `ciphertext || iv || mac`.
fn seal(aes_key: &[u8], hmac_key: &[u8], plain: &[u8]) -> Vec<u8> {
    let iv = random(16);
    let mut out = plain.to_vec();
    AesCtr::new(aes_key.into(), iv.as_slice().into()).apply_keystream(&mut out);
    out.extend_from_slice(&iv);
    let mut mac = <HmacSha as Mac>::new_from_slice(hmac_key).expect("hmac key");
    mac.update(&out);
    out.extend_from_slice(&mac.finalize().into_bytes());
    out
}

fn open_seal(aes_key: &[u8], hmac_key: &[u8], packed: &[u8]) -> Option<Vec<u8>> {
    if packed.len() < 48 {
        return None;
    }
    let (body, mac) = packed.split_at(packed.len() - 32);
    let mut check = <HmacSha as Mac>::new_from_slice(hmac_key).ok()?;
    check.update(body);
    check.verify_slice(mac).ok()?;
    let (cipher, iv) = body.split_at(body.len() - 16);
    let mut plain = cipher.to_vec();
    AesCtr::new(aes_key.into(), iv.into()).apply_keystream(&mut plain);
    Some(plain)
}

struct Buf(Vec<u8>);

impl Buf {
    fn new() -> Buf {
        Buf(Vec::new())
    }

    fn varint(&mut self, mut n: u64) {
        loop {
            let mut byte = (n & 0x7f) as u8;
            n >>= 7;
            if n != 0 {
                byte |= 0x80;
            }
            self.0.push(byte);
            if n == 0 {
                break;
            }
        }
    }

    fn key(&mut self, field: u32, wire: u32) {
        self.varint(((field as u64) << 3) | wire as u64);
    }

    fn int(&mut self, field: u32, n: u64) {
        if n != 0 {
            self.key(field, 0);
            self.varint(n);
        }
    }

    fn bytes(&mut self, field: u32, data: &[u8]) {
        if data.is_empty() && field != 0 {
            return;
        }
        self.key(field, 2);
        self.varint(data.len() as u64);
        self.0.extend_from_slice(data);
    }

    fn text(&mut self, field: u32, text: &str) {
        if !text.is_empty() {
            self.bytes(field, text.as_bytes());
        }
    }

    fn msg(&mut self, field: u32, inner: &[u8]) {
        self.bytes(field, inner);
    }
}

fn config_version() -> Vec<u8> {
    let mut buf = Buf::new();
    buf.int(3, 2026);
    buf.int(4, 9);
    buf.int(5, 2);
    buf.int(7, 4);
    buf.int(9, 6);
    buf.0
}

fn auth_message(request: &str, token: &[u8]) -> Vec<u8> {
    let mut buf = Buf::new();
    buf.text(1, request);
    buf.text(3, NETWORK);
    buf.bytes(6, token);
    buf.msg(7, &config_version());
    buf.0
}

fn browser_details() -> Vec<u8> {
    let mut buf = Buf::new();
    buf.text(1, USER_AGENT);
    buf.int(2, 1); // OTHER
    buf.text(3, "tinyclient");
    buf.int(6, 2); // TABLET, so a browser session can stay signed in beside this one
    buf.0
}

fn uuid() -> String {
    let b = random(16);
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6] & 0x0f | 0x40, b[7], b[8] & 0x3f | 0x80, b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    )
}

/// One protobuf field, as far as this client reads them.
struct Field {
    number: u32,
    bytes: Vec<u8>,
    varint: Option<u64>,
}

fn read_varint(input: &mut &[u8]) -> Option<u64> {
    let mut n = 0u64;
    let mut shift = 0;
    for _ in 0..10 {
        let byte = *input.first()?;
        *input = &input[1..];
        n |= ((byte & 0x7f) as u64) << shift;
        if byte & 0x80 == 0 {
            return Some(n);
        }
        shift += 7;
    }
    None
}

fn fields(mut input: &[u8]) -> Vec<Field> {
    let mut out = Vec::new();
    while !input.is_empty() {
        let Some(key) = read_varint(&mut input) else {
            break;
        };
        let number = (key >> 3) as u32;
        let wire = (key & 7) as u32;
        match wire {
            0 => {
                let Some(n) = read_varint(&mut input) else {
                    break;
                };
                out.push(Field {
                    number,
                    bytes: Vec::new(),
                    varint: Some(n),
                });
            }
            2 => {
                let Some(len) = read_varint(&mut input) else {
                    break;
                };
                let len = len as usize;
                if input.len() < len {
                    break;
                }
                let (data, rest) = input.split_at(len);
                input = rest;
                out.push(Field {
                    number,
                    bytes: data.to_vec(),
                    varint: None,
                });
            }
            1 => {
                if input.len() < 8 {
                    break;
                }
                input = &input[8..];
            }
            5 => {
                if input.len() < 4 {
                    break;
                }
                input = &input[4..];
            }
            _ => break,
        }
    }
    out
}

fn field<'a>(list: &'a [Field], number: u32) -> Option<&'a Field> {
    list.iter().find(|field| field.number == number)
}

fn field_text(list: &[Field], number: u32) -> String {
    field(list, number)
        .map(|field| String::from_utf8_lossy(&field.bytes).into_owned())
        .unwrap_or_default()
}

fn field_bytes(list: &[Field], number: u32) -> Vec<u8> {
    field(list, number)
        .map(|field| field.bytes.clone())
        .unwrap_or_default()
}

fn field_int(list: &[Field], number: u32) -> u64 {
    field(list, number)
        .and_then(|field| field.varint)
        .unwrap_or(0)
}

/// `RPCPairData.revoked` (field 5) with no `paired` (field 4): the phone removed this computer.
pub fn revoked_pair(bytes: &[u8]) -> bool {
    let list = fields(bytes);
    field_bytes(&list, 4).is_empty() && field(&list, 5).is_some()
}

fn field_texts(list: &[Field], number: u32) -> Vec<String> {
    list.iter()
        .filter(|field| field.number == number)
        .map(|field| String::from_utf8_lossy(&field.bytes).into_owned())
        .filter(|text| !text.is_empty())
        .collect()
}

#[derive(Clone, Default)]
struct Device {
    user: i64,
    source: String,
}

fn decode_device(bytes: &[u8]) -> Device {
    let list = fields(bytes);
    Device {
        user: field_int(&list, 1) as i64,
        source: field_text(&list, 2),
    }
}

fn decode_token(bytes: &[u8]) -> (Vec<u8>, i64) {
    let list = fields(bytes);
    (field_bytes(&list, 1), field_int(&list, 2) as i64)
}

/// What the keychain holds after a phone has paired. `key` is the ECDSA signing key, hex scalar,
/// kept so a later refresh can sign. Not `Debug`.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Saved {
    #[serde(default)]
    pub cookies: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub dest_reg_id: String,
    #[serde(default)]
    pub pairing_id: String,
    #[serde(default)]
    pub dest_phone_id: u64,
    pub account: String,
    #[serde(default)]
    pub name: String,
    pub aes: String,
    pub hmac: String,
    pub token: String,
    pub ttl: i64,
    pub mobile_user: i64,
    pub mobile: String,
    pub browser_user: i64,
    pub browser: String,
    pub key: String,
}

impl Saved {
    fn keys(&self) -> Option<(Vec<u8>, Vec<u8>, Vec<u8>)> {
        Some((unhex(&self.aes)?, unhex(&self.hmac)?, unb64(&self.token)?))
    }
}

fn json_array(value: &serde_json::Value) -> Option<&Vec<serde_json::Value>> {
    value.as_array()
}

fn at<'a>(value: &'a serde_json::Value, field: usize) -> Option<&'a serde_json::Value> {
    let list = json_array(value)?;
    let item = list.get(field.saturating_sub(1))?;
    (!item.is_null()).then_some(item)
}

fn as_i64(value: &serde_json::Value) -> i64 {
    match value {
        serde_json::Value::Number(n) => n.as_i64().unwrap_or(0),
        serde_json::Value::String(text) => text.parse().unwrap_or(0),
        _ => 0,
    }
}

fn as_bytes(value: &serde_json::Value) -> Vec<u8> {
    value.as_str().and_then(unb64).unwrap_or_default()
}

/// PBLite: array index is field number minus one. Int64 is a string. Bytes are standard base64.
fn pblite_auth(request: &str, token: &[u8]) -> serde_json::Value {
    let config = serde_json::json!([null, null, 2026, 9, 2, null, 4, null, 6]);
    serde_json::json!([
        request,
        null,
        NETWORK,
        null,
        null,
        if token.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::Value::String(b64(token))
        },
        config
    ])
}

fn pblite_device(device: &Device) -> serde_json::Value {
    serde_json::json!([device.user.to_string(), device.source, NETWORK])
}

fn outgoing(
    mobile: &Device,
    request: &str,
    session: &str,
    action: u32,
    encrypted: &[u8],
    token: &[u8],
    ttl: i64,
    message_type: u32,
) -> serde_json::Value {
    let mut data = Buf::new();
    data.text(1, request);
    data.int(2, action as u64);
    data.bytes(5, encrypted);
    data.text(6, session);
    let inner = serde_json::json!([
        request,
        19,
        null,
        null,
        null,
        null,
        null,
        null,
        null,
        null,
        null,
        b64(&data.0),
        null,
        null,
        null,
        null,
        null,
        null,
        null,
        null,
        null,
        null,
        [serde_json::json!([]), message_type]
    ]);
    serde_json::json!([
        pblite_device(mobile),
        inner,
        [
            request,
            null,
            null,
            null,
            null,
            b64(token),
            serde_json::json!([null, null, 2026, 9, 2, null, 4, null, 6])
        ],
        null,
        ttl.to_string()
    ])
}

/// Where the next page starts. Field 1 is the last item already seen, field 2 its time in
/// milliseconds. Message times are stored here as unix seconds, so the request multiplies.
#[derive(Clone)]
pub(super) struct Cursor {
    pub id: String,
    pub millis: i64,
}

/// Unix seconds, as a message is stored, to the cursor's milliseconds.
pub(super) fn cursor_millis(sent: i64) -> i64 {
    sent.saturating_mul(1000)
}

/// A page shorter than asked is the last one. An empty page is too.
pub(super) fn more_messages(got: usize) -> bool {
    got as u64 >= MESSAGE_PAGE
}

fn cursor_message(cursor: &Cursor) -> Vec<u8> {
    let mut buf = Buf::new();
    buf.text(1, &cursor.id);
    if cursor.millis > 0 {
        buf.int(2, cursor.millis as u64);
    }
    buf.0
}

fn list_request(count: u64, before: Option<&Cursor>) -> Vec<u8> {
    let mut buf = Buf::new();
    buf.int(2, count);
    buf.int(4, 1); // INBOX
    if let Some(cursor) = before {
        buf.msg(5, &cursor_message(cursor));
    }
    buf.0
}

fn messages_request(conversation: &str, count: u64, before: Option<&Cursor>) -> Vec<u8> {
    let mut buf = Buf::new();
    buf.text(2, conversation);
    buf.int(3, count);
    if let Some(cursor) = before {
        buf.msg(5, &cursor_message(cursor));
    }
    buf.0
}

/// `ListConversationsResponse` and `ListMessagesResponse` field 5, when the phone sent one.
fn decode_cursor(bytes: &[u8]) -> Option<Cursor> {
    let list = fields(bytes);
    let raw = field(&list, 5)?;
    if raw.varint.is_some() || raw.bytes.is_empty() {
        return None;
    }
    let inner = fields(&raw.bytes);
    let id = field_text(&inner, 1);
    if id.is_empty() {
        return None;
    }
    Some(Cursor { id, millis: field_int(&inner, 2) as i64 })
}

/// The oldest chat on a page, when the phone sent no cursor of its own. A chat with no time is
/// left out: a cursor at time 0 would ask for the same page again.
fn oldest_cursor(chats: &[Chat]) -> Option<Cursor> {
    let oldest = chats.iter().filter(|chat| chat.updated > 0 && !chat.id.is_empty()).min_by_key(|chat| chat.updated)?;
    Some(Cursor { id: oldest.id.clone(), millis: oldest.updated.saturating_mul(1000) })
}

fn send_request(conversation: &str, text: &str, tmp: &str, mine: &str, reply: Option<&str>) -> Vec<u8> {
    let mut content = Buf::new();
    content.text(1, text);
    let mut info = Buf::new();
    info.msg(2, &content.0);
    let mut payload_content = Buf::new();
    payload_content.msg(1, &content.0);
    let mut payload = Buf::new();
    payload.text(1, tmp);
    payload.msg(6, &payload_content.0);
    payload.text(7, conversation);
    payload.text(9, mine);
    payload.msg(10, &info.0);
    payload.text(12, tmp);
    let mut req = Buf::new();
    req.text(2, conversation);
    req.msg(3, &payload.0);
    req.text(5, tmp);
    // `SendMessageRequest.reply`, field 8: `ReplyPayload.messageID`. Absent on a plain send.
    if let Some(id) = reply.filter(|id| !id.is_empty()) {
        let mut quoted = Buf::new();
        quoted.text(1, id);
        req.msg(8, &quoted.0);
    }
    req.0
}

fn resend_request(message: &str) -> Vec<u8> {
    let mut buf = Buf::new();
    // ResendMessageRequest.messageID (field 2).
    buf.text(2, message);
    buf.0
}

fn refused(bytes: &[u8]) -> Result<(), String> {
    let list = fields(bytes);
    let status = field_int(&list, 3);
    if status > 1 {
        return Err("Messages didn't do that. Keep the phone online.".into());
    }
    if field(&list, 1).is_some_and(|field| field.varint == Some(0)) {
        return Err("Messages didn't do that.".into());
    }
    Ok(())
}

fn opened(key: &[u8], bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    if key.len() == 32 && bytes.first() == Some(&0) {
        super::media::unpack(key, &bytes)
    } else {
        Ok(bytes)
    }
}

fn looks_b64(bytes: &[u8]) -> bool {
    !bytes.is_empty()
        && bytes.len() % 4 == 0
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'='))
}

/// What the phone is told a file is. The bytes win when they name a type, because a wrong
/// extension (or none) is how a video arrives as a generic download. A zip that is a Word,
/// Excel or PowerPoint file keeps the name: those three start with the same bytes.
/// `upload::mime_of` only reads the name, and other networks send through that. Messages keeps
/// this one so a carrier is not handed `application/octet-stream` for a type it knows.
pub(super) fn mime_of(name: &str, bytes: &[u8]) -> &'static str {
    if let Some(found) = mime_of_bytes(bytes) {
        if found == "application/zip" {
            if let Some(office) = office_of(name) {
                return office;
            }
        }
        return found;
    }
    mime_of_name(name)
}

fn extension(name: &str) -> String {
    name.rsplit_once('.')
        .map(|(_, extension)| extension)
        .unwrap_or("")
        .to_ascii_lowercase()
}

fn office_of(name: &str) -> Option<&'static str> {
    match extension(name).as_str() {
        "docx" => Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
        "xlsx" => Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
        "pptx" => Some("application/vnd.openxmlformats-officedocument.presentationml.presentation"),
        _ => None,
    }
}

fn mime_of_name(name: &str) -> &'static str {
    match extension(name).as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "heic" => "image/heic",
        "heif" => "image/heif",
        "bmp" => "image/bmp",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "3gp" => "video/3gpp",
        "3g2" => "video/3gpp2",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "ogg" | "oga" => "audio/ogg",
        "opus" => "audio/opus",
        "wav" => "audio/wav",
        "amr" => "audio/amr",
        "awb" => "audio/amr-wb",
        "pdf" => "application/pdf",
        "vcf" => "text/vcard",
        "txt" => "text/plain",
        "csv" => "text/csv",
        "zip" => "application/zip",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        _ => "application/octet-stream",
    }
}

fn mime_of_bytes(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some("image/jpeg");
    }
    if bytes.starts_with(b"\x89PNG") {
        return Some("image/png");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        return Some("image/webp");
    }
    if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WAVE") {
        return Some("audio/wav");
    }
    if bytes.starts_with(b"%PDF") {
        return Some("application/pdf");
    }
    if bytes.starts_with(b"#!AMR-WB") {
        return Some("audio/amr-wb");
    }
    if bytes.starts_with(b"#!AMR") {
        return Some("audio/amr");
    }
    if bytes.starts_with(b"ID3") {
        return Some("audio/mpeg");
    }
    if bytes.starts_with(b"OggS") {
        if bytes.get(28..36) == Some(b"OpusHead") {
            return Some("audio/opus");
        }
        return Some("audio/ogg");
    }
    if bytes.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]) {
        return Some("video/webm");
    }
    if bytes.starts_with(b"PK\x03\x04") || bytes.starts_with(b"PK\x05\x06") || bytes.starts_with(b"PK\x07\x08") {
        return Some("application/zip");
    }
    if let Some(brand) = ftyp_brand(bytes) {
        if let Some(mime) = brand_mime(brand) {
            return Some(mime);
        }
    }
    if bmp(bytes) {
        return Some("image/bmp");
    }
    if let Some(audio) = audio_frame(bytes) {
        return Some(audio);
    }
    None
}

fn ftyp_brand(bytes: &[u8]) -> Option<&[u8]> {
    (bytes.get(4..8) == Some(b"ftyp")).then(|| bytes.get(8..12)).flatten()
}

fn brand_mime(brand: &[u8]) -> Option<&'static str> {
    Some(match brand {
        b"qt  " => "video/quicktime",
        b"3gp4" | b"3gp5" | b"3gp6" | b"3gp7" => "video/3gpp",
        b"3g2a" | b"3g2b" | b"3g2c" => "video/3gpp2",
        b"M4A " | b"M4B " => "audio/mp4",
        b"M4V " | b"mp41" | b"mp42" | b"isom" | b"iso2" | b"avc1" => "video/mp4",
        b"heic" | b"heix" | b"hevc" | b"hevx" => "image/heic",
        b"heif" | b"mif1" | b"msf1" => "image/heif",
        _ => return None,
    })
}

/// `BM` alone is two letters. A bitmap also names where its pixels start, at least the 54-byte
/// header.
fn bmp(bytes: &[u8]) -> bool {
    if bytes.len() < 14 || !bytes.starts_with(b"BM") {
        return false;
    }
    let offset = u32::from_le_bytes([bytes[10], bytes[11], bytes[12], bytes[13]]);
    (54..=1024).contains(&offset)
}

/// An MPEG frame or an AAC ADTS frame. Layer `00` is reserved in MPEG and is what ADTS uses.
fn audio_frame(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() < 3 || bytes[0] != 0xff || bytes[1] & 0xe0 != 0xe0 {
        return None;
    }
    let layer = (bytes[1] >> 1) & 0x03;
    if layer == 0 {
        let rate = (bytes[2] >> 2) & 0x0f;
        return (rate != 0x0f).then_some("audio/aac");
    }
    matches!(bytes[1], 0xfb | 0xf3 | 0xf2 | 0xfa | 0xe3).then_some("audio/mpeg")
}

fn ftyp(brand: &[u8; 4]) -> Vec<u8> {
    let mut bytes = vec![0, 0, 0, 20];
    bytes.extend_from_slice(b"ftyp");
    bytes.extend_from_slice(brand);
    bytes.extend_from_slice(&[0, 0, 0, 0]);
    bytes
}

/// The lab sets `TINYCLIENT_PROVE=messages-file-types`. A build from before this ignores that
/// and keeps serving, so the same driver fails there.
pub(super) fn prove_message_file_types() -> Result<(), String> {
    file_type_samples()
}

fn file_type_samples() -> Result<(), String> {
    let mov = ftyp(b"qt  ");
    let mp4 = ftyp(b"isom");
    let three_gp = ftyp(b"3gp4");
    let three_g2 = ftyp(b"3g2a");
    let m4a = ftyp(b"M4A ");
    let heic = ftyp(b"heic");
    let heif = ftyp(b"mif1");
    let samples: Vec<(&str, &[u8], &str)> = vec![
        ("clip.mov", &mov, "video/quicktime"),
        ("clip.dat", &mov, "video/quicktime"),
        ("film.mp4", &mp4, "video/mp4"),
        ("film.m4v", b"", "video/mp4"),
        ("clip.webm", &[0x1a, 0x45, 0xdf, 0xa3], "video/webm"),
        ("clip.3gp", &three_gp, "video/3gpp"),
        ("clip.3g2", &three_g2, "video/3gpp2"),
        ("song.m4a", &m4a, "audio/mp4"),
        ("song.aac", &[0xff, 0xf1, 0x50, 0x80, 0x00, 0x1f, 0xfc], "audio/aac"),
        ("song.ogg", b"OggS", "audio/ogg"),
        ("voice.opus", b"OggS\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0OpusHead", "audio/opus"),
        ("song.wav", b"RIFF\0\0\0\0WAVE", "audio/wav"),
        ("song.amr", b"#!AMR\n", "audio/amr"),
        ("song.awb", b"#!AMR-WB\n", "audio/amr-wb"),
        ("song.mp3", b"ID3", "audio/mpeg"),
        ("photo.heic", &heic, "image/heic"),
        ("photo.heif", &heif, "image/heif"),
        ("photo.jpg", b"\xff\xd8\xff", "image/jpeg"),
        ("photo.bin", b"\xff\xd8\xff", "image/jpeg"),
        ("photo.png", b"\x89PNG\r\n\x1a\n", "image/png"),
        ("anim.gif", b"GIF89a", "image/gif"),
        ("still.webp", b"RIFF\0\0\0\0WEBP", "image/webp"),
        ("photo.bmp", b"BM\0\0\0\0\0\0\0\0\x36\0\0\0", "image/bmp"),
        ("page.pdf", b"%PDF", "application/pdf"),
        ("notes.txt", b"hello", "text/plain"),
        ("notes.csv", b"a,b\n", "text/csv"),
        ("card.vcf", b"BEGIN:VCARD", "text/vcard"),
        ("pack.zip", b"PK\x03\x04", "application/zip"),
        ("report.docx", b"PK\x03\x04", "application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
        ("sheet.xlsx", b"PK\x03\x04", "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
        ("deck.pptx", b"PK\x03\x04", "application/vnd.openxmlformats-officedocument.presentationml.presentation"),
        ("mystery.bin", b"hello", "application/octet-stream"),
        ("photo.jpg", b"\x89PNG\r\n\x1a\n", "image/png"),
    ];
    for (name, bytes, want) in samples {
        let got = mime_of(name, bytes);
        if got != want {
            return Err(format!("{name} was {got}, not {want}"));
        }
    }
    Ok(())
}

fn send_media_request(
    conversation: &str,
    caption: &str,
    tmp: &str,
    mine: &str,
    media_id: &str,
    key: &[u8],
    name: &str,
    mime: &str,
    size: u64,
    reply: Option<&str>,
) -> Vec<u8> {
    let mut media = Buf::new();
    media.text(2, media_id);
    media.text(4, name);
    media.int(5, size);
    media.bytes(11, key);
    media.text(14, mime);
    let mut media_info = Buf::new();
    media_info.msg(3, &media.0);
    let mut payload = Buf::new();
    payload.text(1, tmp);
    payload.text(7, conversation);
    payload.text(9, mine);
    payload.msg(10, &media_info.0);
    if !caption.is_empty() {
        let mut content = Buf::new();
        content.text(1, caption);
        let mut payload_content = Buf::new();
        payload_content.msg(1, &content.0);
        payload.msg(6, &payload_content.0);
        let mut caption_info = Buf::new();
        caption_info.msg(2, &content.0);
        payload.msg(10, &caption_info.0);
    }
    payload.text(12, tmp);
    let mut req = Buf::new();
    req.text(2, conversation);
    req.msg(3, &payload.0);
    req.text(5, tmp);
    if let Some(id) = reply.filter(|id| !id.is_empty()) {
        let mut quoted = Buf::new();
        quoted.text(1, id);
        req.msg(8, &quoted.0);
    }
    req.0
}

fn reaction_request(message: &str, emoji: &str, action: u64) -> Vec<u8> {
    let mut data = Buf::new();
    data.text(1, emoji);
    data.int(2, reaction_type(emoji));
    let mut req = Buf::new();
    req.text(1, message);
    req.msg(2, &data.0);
    req.int(3, action);
    req.0
}

fn image_request(message: &str, action: &str) -> Vec<u8> {
    let mut req = Buf::new();
    req.text(1, message);
    req.text(2, action);
    req.0
}

fn read_request(conversation: &str, message: &str) -> Vec<u8> {
    let mut buf = Buf::new();
    buf.text(2, conversation);
    buf.text(3, message);
    buf.0
}

fn typing_request(conversation: &str) -> Vec<u8> {
    let mut data = Buf::new();
    data.text(1, conversation);
    data.int(3, 1);
    let mut buf = Buf::new();
    buf.msg(2, &data.0);
    buf.0
}

/// GetThumbnailRequest: the participants whose pictures are wanted (field 1, repeated).
fn thumbnail_request(participants: &[String]) -> Vec<u8> {
    let mut req = Buf::new();
    for participant in participants {
        req.text(1, participant);
    }
    req.0
}

/// GetThumbnailResponse: field 1 is repeated Thumbnail, each its participant (1) and its data
/// (2), whose field 3 is the picture's bytes. Only what reads as a picture is kept: a phone
/// with no picture for someone leaves them out or sends nothing.
fn decode_thumbnails(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
    fields(bytes)
        .iter()
        .filter(|field| field.number == 1)
        .filter_map(|thumbnail| {
            let thumbnail = fields(&thumbnail.bytes);
            let participant = field_text(&thumbnail, 1);
            let picture = field_bytes(&fields(&field_bytes(&thumbnail, 2)), 3);
            (!participant.is_empty() && crate::blobs::is_picture(&picture)).then_some((participant, picture))
        })
        .collect()
}

/// Unix seconds from a Bugle timestamp, which arrives in microseconds.
fn unix_seconds(raw: i64) -> i64 {
    if raw > 10_000_000_000_000_000 {
        raw / 1_000_000
    } else if raw > 10_000_000_000_000 {
        raw / 1_000
    } else {
        raw
    }
}

#[derive(Clone)]
pub struct Chat {
    pub id: String,
    pub title: String,
    pub group: bool,
    pub unread: bool,
    pub mine: String,
    /// The other side of a one-to-one chat, by participant id: whose picture the row shows.
    /// Empty for a group.
    pub peer: String,
    /// `Conversation.type`, field 22: 1 SMS, 2 RCS, 0 when the phone did not say.
    pub carrier: u64,
    /// Unix seconds of the latest message, from field 5. 0 when the phone did not say.
    /// The inbox cursor uses it when the response has no cursor of its own.
    pub updated: i64,
    /// A tombstone said the phones encrypt the chat. A later conversation update does not
    /// clear it: the conversation message has no such field.
    pub phones: bool,
}

/// One photo or file on a message. Bytes are already opened when the phone sent them inline.
#[derive(Clone, Default)]
pub struct Piece {
    pub name: String,
    pub mime: String,
    pub media_id: String,
    pub key: Vec<u8>,
    /// The message part this file belongs to, for a later full-size fetch.
    pub action_id: String,
    pub inline: Vec<u8>,
    pub size: u64,
    pub width: u32,
    pub height: u32,
}

/// One emoji on a message, and who put it there.
#[derive(Clone, Default)]
pub struct Mark {
    pub emoji: String,
    pub who: Vec<String>,
}

#[derive(Clone, Default)]
pub struct Note {
    pub id: String,
    pub conversation: String,
    pub author: String,
    pub name: String,
    pub sent: i64,
    pub text: String,
    pub outgoing: bool,
    pub file: bool,
    pub pieces: Vec<Piece>,
    pub reactions: Vec<Mark>,
    /// The phone's word on the message (`MessageStatus.status`, field 4.2).
    pub status: u64,
    /// `Message.tmpID` (field 12): the id this computer used when it asked the phone to send.
    pub tmp: String,
    /// `MessageStatus.statusText` (field 4.5), or `errMsg` (field 4.4) when the phone sent no text.
    pub detail: String,
    /// `Message.type`, field 11. 0 when the phone did not say.
    pub wire: u64,
    /// This update only turns encryption between the phones on or off. It is not a message.
    pub phones: Option<bool>,
    /// `ReplyMessage.messageID`, field 21.1. Empty when this message is not a reply.
    pub reply: String,
}

/// What the other side did with one of my messages, as far as the phone knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Receipt {
    Delivered,
    Read,
}

/// Bugle's `MessageStatusType` for my messages: 2 is OUTGOING_DELIVERED, 11 OUTGOING_DISPLAYED
/// (read). 1 is OUTGOING_COMPLETE and 14 is OUTGOING_NOT_DELIVERED_YET: both have left this
/// computer. A failure is not sent.
pub fn receipt(status: u64) -> Option<Receipt> {
    match status {
        2 => Some(Receipt::Delivered),
        11 => Some(Receipt::Read),
        _ => None,
    }
}

/// Whether one of my messages is on its way, sent, or refused by the phone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    Sent,
    Pending,
    Failed(&'static str),
}

/// Maps Bugle's `MessageStatusType` for an outgoing message. Codes 100 and above are incoming
/// and are not passed here. A code this build does not know is a failure, never a success.
pub fn progress(status: u64) -> Progress {
    match status {
        1 | 2 | 11 | 14 => Progress::Sent,
        0 | 3 | 4 | 5 | 6 | 7 | 10 | 15 | 16 | 20 => Progress::Pending,
        8 => Progress::Failed("Not sent. No service."),
        9 => Progress::Failed("That number is invalid."),
        13 => Progress::Failed("That message is too large."),
        17 => Progress::Failed("The other person can't receive this."),
        19 | 22 => Progress::Failed("The other person couldn't open this."),
        21 => Progress::Failed("The other person can't receive an encrypted text."),
        _ => Progress::Failed("Not sent."),
    }
}

/// What the composer can say. Field 22 is `ConversationType`: 1 SMS, 2 RCS.
/// Encryption between the phones is not a field on the conversation; tombstones say it, and
/// this computer cannot check it, so the words stay "Encrypted between phones".
pub fn composer_label(carrier: u64, phones: bool) -> String {
    let base = match carrier {
        2 => "RCS message",
        1 => "Text message",
        _ => "",
    };
    if !phones {
        return base.to_owned();
    }
    if base.is_empty() {
        "Encrypted between phones".to_owned()
    } else {
        format!("{base} · Encrypted between phones")
    }
}

/// "SMS" or "MMS" when the message is not the conversation's usual kind. Field 11 is
/// `Message.type`: 1 SMS, 2 downloaded MMS, 3 undownloaded MMS, 4 RCS.
pub fn wire_tag(wire: u64, carrier: u64) -> &'static str {
    let usual = match carrier {
        2 => 4,
        1 => 1,
        _ => 0,
    };
    if wire == 0 || (usual != 0 && wire == usual) {
        return "";
    }
    match wire {
        1 => "SMS",
        2 | 3 => "MMS",
        4 => "RCS",
        _ => "",
    }
}

/// A reply reference is only sent in an RCS conversation (`Conversation.type` 2). SMS, MMS,
/// and a chat the phone has not typed stay plain: a reference there can make the phone refuse
/// the message.
pub fn allows_reply(carrier: u64) -> bool {
    carrier == 2
}

/// Typing and read ticks exist on RCS. An SMS chat (carrier 1) has neither. Unknown stays on:
/// the phone has not said the chat is SMS.
pub fn shows_typing(carrier: u64) -> bool {
    carrier != 1
}

pub fn shows_receipts(carrier: u64) -> bool {
    carrier != 1
}

/// Said before a file leaves a chat the phone has called SMS, not RCS.
pub const MMS_WARNING: &str = "Sends as MMS; may be compressed";

/// Published tombstones that turn encryption between the phones on or off. Nothing else in the
/// conversation message says it.
pub fn between_phones(status: u64) -> Option<bool> {
    match status {
        213 | 216 | 219 | 224 | 225 | 226 | 229 | 233 | 235 | 237 => Some(true),
        214 | 234 | 236 | 238 | 252 => Some(false),
        _ => None,
    }
}

struct Live {
    saved: Saved,
    aes: Vec<u8>,
    hmac: Vec<u8>,
    token: Vec<u8>,
    ttl: i64,
    /// One id for the long-poll session. Responses are matched on it and on the request id.
    session: String,
    chats: Vec<Chat>,
    cookies: String,
    token_refreshed: Instant,
}

struct Waiter {
    request: String,
    session: String,
    reply: Sender<Vec<u8>>,
}

fn agent(timeout: Duration) -> ureq::Agent {
    // The same system TLS as the rest of chatd. ureq's default is rustls, which this crate does
    // not enable, and asking for it panics before a pairing code exists.
    ureq::Agent::config_builder()
        .tls_config(crate::tls::config())
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .user_agent(USER_AGENT)
        .build()
        .into()
}

struct Http {
    body: Vec<u8>,
    cookies: String,
}

fn post(
    agent: &ureq::Agent,
    url: &str,
    content: &str,
    body: &[u8],
    cookies: &str,
) -> Result<Http, String> {
    let google = google::authorization(cookies);
    let url = if google.is_some() {
        url.replace(
            "instantmessaging-pa.googleapis.com/$rpc",
            "instantmessaging-pa.clients6.google.com/$rpc",
        )
    } else {
        url.to_owned()
    };
    let mut request = agent
        .post(&url)
        .header("Content-Type", content)
        .header("x-goog-api-key", API_KEY)
        .header("x-user-agent", "grpc-web-javascript/0.1")
        .header("Origin", ORIGIN)
        .header("Referer", &format!("{ORIGIN}/"))
        .header(
            "sec-ch-ua",
            "\"Google Chrome\";v=\"152\", \"Chromium\";v=\"152\", \"Not-A.Brand\";v=\"24\"",
        );
    if !cookies.is_empty() {
        request = request.header("Cookie", cookies);
    }
    if let Some(auth) = google {
        request = request.header("Authorization", &auth);
    }
    let mut response = request
        .send(body)
        .map_err(|err| format!("Messages: {err}"))?;
    let status = response.status().as_u16();
    let mut cookies_out = String::new();
    for (name, value) in response.headers() {
        if name.as_str().eq_ignore_ascii_case("set-cookie") {
            if let Some(pair) = value.to_str().ok().and_then(|line| line.split(';').next()) {
                if !cookies_out.is_empty() {
                    cookies_out.push_str("; ");
                }
                cookies_out.push_str(pair.trim());
            }
        }
    }
    let mut body = Vec::new();
    response
        .body_mut()
        .as_reader()
        .read_to_end(&mut body)
        .map_err(|err| err.to_string())?;
    if !(200..300).contains(&status) {
        return Err(format!("Messages refused the request ({status})."));
    }
    Ok(Http {
        body,
        cookies: cookies_out,
    })
}

fn merge_cookies(old: &str, new: &str) -> String {
    if new.is_empty() {
        return old.to_owned();
    }
    if old.is_empty() {
        return new.to_owned();
    }
    let mut cookies = std::collections::BTreeMap::new();
    for pair in old.split(';').chain(new.split(';')) {
        if let Some((name, value)) = pair.trim().split_once('=') {
            cookies.insert(name, value);
        }
    }
    cookies
        .into_iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("; ")
}

/// A running session: the long-poll thread and the commands share `live`.
/// The inbox page still to ask for. `done` until the first list says there is another.
struct Inbox {
    next: Option<Cursor>,
    done: bool,
}

pub struct Session {
    live: Arc<Mutex<Live>>,
    events: Sender<crate::adapters::messages::Event>,
    heard: Mutex<Option<Receiver<crate::adapters::messages::Event>>>,
    waiters: Arc<Mutex<Vec<Waiter>>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    inbox: Mutex<Inbox>,
}

impl Session {
    pub fn open(mut saved: Saved) -> Result<Session, String> {
        if !saved.cookies.is_empty() {
            google::refresh(&mut saved)?;
        }
        let (aes, hmac, token) = saved
            .keys()
            .ok_or_else(|| "the Messages session is unreadable".to_owned())?;
        let (events, heard) = channel();
        let live = Arc::new(Mutex::new(Live {
            ttl: saved.ttl,
            cookies: google::cookie_header(&saved.cookies),
            token_refreshed: Instant::now(),
            saved,
            aes,
            hmac,
            token,
            session: uuid(),
            chats: Vec::new(),
        }));
        let session = Session {
            live,
            events,
            heard: Mutex::new(Some(heard)),
            waiters: Arc::default(),
            stop: Arc::default(),
            inbox: Mutex::new(Inbox { next: None, done: true }),
        };
        let (live, events, waiters, stop) = (
            session.live.clone(),
            session.events.clone(),
            session.waiters.clone(),
            session.stop.clone(),
        );
        std::thread::Builder::new()
            .name("messages-events".into())
            .spawn(move || {
                let mut first = true;
                let mut pause = POLL_FLOOR;
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    let failed = poll(
                        live.clone(),
                        events.clone(),
                        waiters.clone(),
                        stop.clone(),
                        first,
                        None,
                    );
                    first = false;
                    if stop.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }
                    // A failed refresh, connect, or refused status backs off. A normal close,
                    // including the 90-second timeout, starts the wait over.
                    pause = if failed { next_pause(pause) } else { POLL_FLOOR };
                    if stop.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }
                    wait_or_stop(&stop, pause);
                }
            })
            .map_err(|err| err.to_string())?;
        Ok(session)
    }

    pub fn events(&self) -> Option<Receiver<crate::adapters::messages::Event>> {
        self.heard
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }

    pub fn me(&self) -> (String, String) {
        let live = self
            .live
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let name = if live.saved.name.is_empty() {
            "Messages".into()
        } else {
            live.saved.name.clone()
        };
        (live.saved.account.clone(), name)
    }

    pub fn close(&self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    fn rpc(&self, action: u32, plain: &[u8], message_type: u32) -> Result<Vec<u8>, String> {
        self.rpc_as(&uuid(), action, plain, message_type)
    }

    /// SetActiveSession. The request id is the long-poll's session id; without it the phone
    /// accepts the pair and then sends nothing.
    fn activate(&self) -> Result<(), String> {
        let session = self
            .live
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .session
            .clone();
        let _ = self.rpc_as(&session, 16, &[], 2)?;
        Ok(())
    }

    fn rpc_as(
        &self,
        request: &str,
        action: u32,
        plain: &[u8],
        message_type: u32,
    ) -> Result<Vec<u8>, String> {
        self.rpc_as_cancel(request, action, plain, message_type, None)
    }

    fn rpc_as_cancel(
        &self,
        request: &str,
        action: u32,
        plain: &[u8],
        message_type: u32,
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<Vec<u8>, String> {
        let request = request.to_owned();
        let (session, body, cookies) = {
            let live = self
                .live
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let gaia_pairing = matches!(action, 44 | 45 | 47);
            let encrypted = if plain.is_empty() || gaia_pairing {
                Vec::new()
            } else {
                seal(&live.aes, &live.hmac, plain)
            };
            let mobile = Device {
                user: live.saved.mobile_user,
                source: live.saved.mobile.clone(),
            };
            let mut body = outgoing(
                &mobile,
                &request,
                &live.session,
                action,
                &encrypted,
                &live.token,
                live.ttl,
                message_type,
            );
            if !live.saved.dest_reg_id.is_empty() {
                body[0][2] = "GDitto".into();
                body.as_array_mut()
                    .unwrap()
                    .resize(9, serde_json::Value::Null);
                body[8] = serde_json::json!([b64(live.saved.dest_reg_id.as_bytes())]);
            }
            if gaia_pairing {
                let mut data = Buf::new();
                data.text(1, &request);
                data.int(2, action as u64);
                data.bytes(3, plain);
                data.text(6, &live.session);
                body[1][11] = b64(&data.0).into();
                body[4] = "300000000".into();
            }
            (
                live.session.clone(),
                serde_json::to_vec(&body).unwrap_or_default(),
                live.cookies.clone(),
            )
        };
        let (tx, rx) = channel();
        self.waiters
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(Waiter {
                request: request.clone(),
                session: session.clone(),
                reply: tx,
            });
        let agent = agent(Duration::from_secs(30));
        let sent = post(
            &agent,
            &format!("{RELAY}.Messaging/SendMessage"),
            "application/json+protobuf",
            &body,
            &cookies,
        );
        if let Ok(ref response) = sent {
            let mut live = self.live.lock().unwrap_or_else(|p| p.into_inner());
            live.cookies = merge_cookies(&live.cookies, &response.cookies);
            if !live.saved.cookies.is_empty() {
                let cookies = live.cookies.clone();
                google::update_cookies(&mut live.saved, &cookies);
            }
        }
        if let Err(err) = sent {
            self.waiters
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .retain(|waiter| waiter.request != request);
            return Err(err);
        }
        let deadline = Instant::now()
            + if action == 45 {
                Duration::from_secs(180)
            } else {
                PHONE
            };
        let received = loop {
            if cancel.is_some_and(|cancel| cancel.load(std::sync::atomic::Ordering::Relaxed)) {
                self.waiters
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .retain(|waiter| waiter.request != request);
                return Err("cancelled".into());
            }
            match rx.recv_timeout(Duration::from_millis(100)) {
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) if Instant::now() < deadline => {
                    continue
                }
                result => break result,
            }
        };
        match received {
            Ok(bytes) => Ok(bytes),
            Err(_) => {
                self.waiters
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .retain(|waiter| waiter.request != request);
                Err(PHONE_QUIET.into())
            }
        }
    }

    pub fn refresh(&self) -> Result<(), String> {
        self.activate()?;
        let first = self.rpc(1, &list_request(CHAT_PAGE, None), 16)?;
        let chats = decode_chats(&first);
        self.note_inbox(&first, &chats);
        self.store_chats(chats, true);
        Ok(())
    }

    /// True when the last inbox page was full and named a later one.
    pub fn inbox_continues(&self) -> bool {
        !self.inbox.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).done
    }

    /// The next inbox page, or nothing when the phone has no more. Later pages use message
    /// type 2; the first page after pairing stays an annotation, which [`refresh`] sends.
    pub fn older_chats(&self) -> Result<Vec<Chat>, String> {
        let sent = {
            let inbox = self.inbox.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if inbox.done {
                return Ok(Vec::new());
            }
            inbox.next.clone()
        };
        let Some(cursor) = sent else {
            self.finish_inbox();
            return Ok(Vec::new());
        };
        let bytes = self.rpc(1, &list_request(CHAT_PAGE, Some(&cursor)), 2)?;
        let chats = decode_chats(&bytes);
        let next = decode_cursor(&bytes).or_else(|| oldest_cursor(&chats));
        let moved = next.as_ref().is_some_and(|next| next.id != cursor.id || next.millis != cursor.millis);
        if !moved {
            self.finish_inbox();
        } else {
            self.note_inbox(&bytes, &chats);
        }
        self.store_chats(chats.clone(), false);
        Ok(chats)
    }

    pub fn history(&self, conversation: &str, before: Option<&Cursor>) -> Result<Vec<Note>, String> {
        let bytes = self.rpc(2, &messages_request(conversation, MESSAGE_PAGE, before), 2)?;
        Ok(decode_messages(&bytes))
    }

    fn finish_inbox(&self) {
        let mut inbox = self.inbox.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        inbox.next = None;
        inbox.done = true;
    }

    fn note_inbox(&self, bytes: &[u8], chats: &[Chat]) {
        let next = decode_cursor(bytes).or_else(|| oldest_cursor(chats));
        let more = chats.len() >= CHAT_PAGE as usize && next.is_some();
        let mut inbox = self.inbox.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if more {
            inbox.next = next;
            inbox.done = false;
        } else {
            inbox.next = None;
            inbox.done = true;
        }
    }

    fn store_chats(&self, chats: Vec<Chat>, replace: bool) {
        if chats.is_empty() {
            return;
        }
        {
            let mut live = self.live.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if replace {
                live.chats = chats.clone();
            } else {
                for chat in &chats {
                    if let Some(had) = live.chats.iter_mut().find(|had| had.id == chat.id) {
                        *had = chat.clone();
                    } else {
                        live.chats.push(chat.clone());
                    }
                }
            }
        }
        let _ = self.events.send(crate::adapters::messages::Event::Chats(chats));
    }

    fn mine_of(&self, conversation: &str) -> String {
        let live = self
            .live
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        live.chats
            .iter()
            .find(|chat| chat.id == conversation)
            .map(|chat| chat.mine.clone())
            .unwrap_or_default()
    }

    pub fn send(
        &self,
        conversation: &str,
        text: &str,
        reply: Option<&str>,
        noted: &mut dyn FnMut(&str),
    ) -> Result<String, String> {
        let mine = self.mine_of(conversation);
        let tmp = uuid();
        noted(&tmp);
        let bytes = self.rpc(3, &send_request(conversation, text, &tmp, &mine, reply), 2)?;
        refused(&bytes)?;
        Ok(tmp)
    }

    /// Asks the phone to send again the copy it already has (`RESEND_MESSAGE`, action 25).
    pub fn resend(&self, message: &str) -> Result<(), String> {
        let bytes = self.rpc(25, &resend_request(message), 2)?;
        refused(&bytes)
    }

    /// Encrypts the file, uploads it, and asks the phone to send it. A large photo may leave as
    /// an MMS copy; this does not promise an RCS high-quality send.
    pub fn send_file(
        &self,
        conversation: &str,
        caption: &str,
        path: &str,
        name: &str,
        reply: Option<&str>,
        noted: &mut dyn FnMut(&str),
    ) -> Result<String, String> {
        let named = if name.is_empty() { "file" } else { name };
        let meta =
            std::fs::metadata(path).map_err(|_| format!("Messages couldn't read {named}."))?;
        if meta.len() > FILE_CAP {
            return Err(
                "That file is larger than 100 MB, which tinyclient won't send through Messages."
                    .into(),
            );
        }
        let plain = std::fs::read(path).map_err(|_| format!("Messages couldn't read {named}."))?;
        let mut key = [0u8; 32];
        getrandom::fill(&mut key).map_err(|_| "Messages could not make a media key.".to_owned())?;
        let packed = super::media::pack(&key, &plain)?;
        let mime = mime_of(named, &plain);
        let media_id = self.upload(&packed, &mime)?;
        let mine = self.mine_of(conversation);
        let tmp = uuid();
        noted(&tmp);
        let bytes = self.rpc(
            3,
            &send_media_request(
                conversation,
                caption,
                &tmp,
                &mine,
                &media_id,
                &key,
                named,
                &mime,
                plain.len() as u64,
                reply,
            ),
            2,
        )?;
        refused(&bytes)?;
        Ok(tmp)
    }

    pub fn react(&self, message: &str, emoji: &str, on: bool) -> Result<(), String> {
        let action = if on { 1 } else { 2 };
        let bytes = self.rpc(38, &reaction_request(message, emoji, action), 2)?;
        if on && refused(&bytes).is_err() {
            let switched = self.rpc(38, &reaction_request(message, emoji, 3), 2)?;
            refused(&switched)?;
            return Ok(());
        }
        refused(&bytes)
    }

    /// The bytes of one attachment, opened. The content store is asked first; the phone is asked
    /// for the full image when that has nothing.
    pub fn fetch(
        &self,
        media_id: &str,
        key: &[u8],
        message: &str,
        action: &str,
    ) -> Result<Vec<u8>, String> {
        if !media_id.is_empty() {
            if let Ok(bytes) = self.download(media_id) {
                return opened(key, bytes);
            }
        }
        if !message.is_empty() {
            let asked = self
                .rpc(36, &image_request(message, action), 2)
                .unwrap_or_default();
            if let Some(inline) = decode_messages(&asked)
                .into_iter()
                .flat_map(|note| note.pieces)
                .find(|piece| !piece.inline.is_empty())
            {
                return Ok(inline.inline);
            }
        }
        Err("Messages couldn't fetch that file. Keep the phone online.".into())
    }

    /// The pictures the phone keeps for these participants (GetParticipantsThumbnail, as
    /// messages.google.com asks), as (participant, bytes). Someone with no picture is left out.
    pub fn thumbnails(&self, participants: &[String]) -> Result<Vec<(String, Vec<u8>)>, String> {
        let bytes = self.rpc(37, &thumbnail_request(participants), 2)?;
        Ok(decode_thumbnails(&bytes))
    }

    fn upload(&self, packed: &[u8], mime: &str) -> Result<String, String> {
        let (payload, cookies) = {
            let live = self
                .live
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut device = Buf::new();
            device.int(1, live.saved.mobile_user as u64);
            device.text(2, &live.saved.mobile);
            device.text(3, NETWORK);
            let mut req = Buf::new();
            req.int(1, 1);
            req.msg(2, &auth_message(&uuid(), &live.token));
            req.msg(3, &device.0);
            (b64(&req.0), live.cookies.clone())
        };
        let http = agent(Duration::from_secs(60));
        let mut start = http
            .post(UPLOAD)
            .header("Content-Type", "text/plain")
            .header("x-goog-api-key", API_KEY)
            .header("Origin", ORIGIN)
            .header("X-Goog-Upload-Protocol", "resumable")
            .header("X-Goog-Upload-Command", "start")
            .header(
                "X-Goog-Upload-Header-Content-Length",
                &packed.len().to_string(),
            )
            .header("X-Goog-Upload-Header-Content-Type", mime);
        if !cookies.is_empty() {
            start = start.header("Cookie", &cookies);
        }
        let response = start
            .send(payload.as_bytes())
            .map_err(|err| format!("Messages couldn't upload that file ({err})."))?;
        if !(200..300).contains(&response.status().as_u16()) {
            return Err("Messages couldn't start the upload. Keep the phone online.".into());
        }
        let url = response
            .headers()
            .get("x-goog-upload-url")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_owned();
        if url.is_empty() {
            return Err("Messages didn't give an upload address.".into());
        }
        let mut finish = http
            .post(&url)
            .header("Content-Type", mime)
            .header("X-Goog-Upload-Command", "upload, finalize")
            .header("X-Goog-Upload-Offset", "0");
        if !cookies.is_empty() {
            finish = finish.header("Cookie", &cookies);
        }
        let mut response = finish
            .send(packed)
            .map_err(|err| format!("Messages couldn't upload that file ({err})."))?;
        let status = response.status().as_u16();
        let mut body = Vec::new();
        response
            .body_mut()
            .as_reader()
            .read_to_end(&mut body)
            .map_err(|err| err.to_string())?;
        if !(200..300).contains(&status) {
            return Err("Messages refused the upload.".into());
        }
        let raw = if looks_b64(&body) {
            unb64(&String::from_utf8_lossy(&body)).unwrap_or(body)
        } else {
            body
        };
        let list = fields(&raw);
        let media = field_text(&fields(&field_bytes(&list, 1)), 1);
        if media.is_empty() {
            return Err("Messages uploaded the file and sent back no id.".into());
        }
        Ok(media)
    }

    fn download(&self, media_id: &str) -> Result<Vec<u8>, String> {
        let (payload, cookies) = {
            let live = self
                .live
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut info = Buf::new();
            info.text(1, media_id);
            info.int(2, 1);
            let mut req = Buf::new();
            req.msg(1, &info.0);
            req.msg(2, &auth_message(&uuid(), &live.token));
            (b64(&req.0), live.cookies.clone())
        };
        let http = agent(Duration::from_secs(60));
        let mut request = http
            .get(UPLOAD)
            .header("x-goog-api-key", API_KEY)
            .header("Origin", ORIGIN)
            .header("X-Goog-Download-Metadata", &payload);
        if !cookies.is_empty() {
            request = request.header("Cookie", &cookies);
        }
        let mut response = request
            .call()
            .map_err(|err| format!("Messages couldn't fetch that file ({err})."))?;
        if !(200..300).contains(&response.status().as_u16()) {
            return Err("Messages couldn't fetch that file.".into());
        }
        let mut body = Vec::new();
        response
            .body_mut()
            .as_reader()
            .read_to_end(&mut body)
            .map_err(|err| err.to_string())?;
        if body.is_empty() {
            return Err("Messages returned an empty file.".into());
        }
        Ok(body)
    }

    pub fn mark_read(&self, conversation: &str, message: &str) -> Result<(), String> {
        let _ = self.rpc(10, &read_request(conversation, message), 2)?;
        Ok(())
    }

    pub fn typing(&self, conversation: &str) -> Result<(), String> {
        let _ = self.rpc(12, &typing_request(conversation), 2)?;
        Ok(())
    }

}

/// Sleeps `pause`, but wakes within a slice when `close` sets `stop`.
fn wait_or_stop(stop: &std::sync::atomic::AtomicBool, pause: Duration) {
    let slice = Duration::from_millis(200);
    let mut left = pause;
    while !left.is_zero() && !stop.load(std::sync::atomic::Ordering::Relaxed) {
        let step = if left < slice { left } else { slice };
        std::thread::sleep(step);
        left = left.saturating_sub(step);
    }
}

/// True when this poll failed before its own close, so the next wait should grow.
/// Refresh, connect, and a refused status return true. The stream's own close returns false.
fn poll(
    live: Arc<Mutex<Live>>,
    events: Sender<crate::adapters::messages::Event>,
    waiters: Arc<Mutex<Vec<Waiter>>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    announce: bool,
    paired: Option<Sender<Saved>>,
) -> bool {
    let refresh = {
        let live = live.lock().unwrap_or_else(|p| p.into_inner());
        let lifetime = if live.ttl > 0 {
            Duration::from_micros(live.ttl as u64)
        } else {
            Duration::from_secs(86400)
        };
        (!live.saved.cookies.is_empty()
            && live.token_refreshed.elapsed() >= lifetime.saturating_sub(Duration::from_secs(3600)))
        .then(|| live.saved.clone())
    };
    if let Some(mut saved) = refresh {
        if let Err(reason) = google::refresh(&mut saved) {
            let _ = events.send(crate::adapters::messages::Event::Closed(reason));
            return true;
        }
        let mut live = live.lock().unwrap_or_else(|p| p.into_inner());
        live.token = unb64(&saved.token).unwrap_or_default();
        live.ttl = saved.ttl;
        live.cookies = google::cookie_header(&saved.cookies);
        live.saved = saved;
        live.token_refreshed = Instant::now();
    }
    let agent = agent(Duration::from_secs(90));
    let (request, token, cookies) = {
        let held = live.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        (uuid(), held.token.clone(), held.cookies.clone())
    };
    // field 4 is an empty wrapper the web client sends: [null, []].
    let mut auth = pblite_auth(&request, &token);
    if google::authorization(&cookies).is_some() {
        auth[2] = "GDitto".into();
    }
    let payload = serde_json::json!([auth, null, null, [null, serde_json::json!([])]]);
    let body = serde_json::to_vec(&payload).unwrap_or_default();
    let relay = if google::authorization(&cookies).is_some() {
        RELAY.replace("googleapis.com", "clients6.google.com")
    } else {
        RELAY.to_owned()
    };
    let mut request = agent
        .post(&format!("{relay}.Messaging/ReceiveMessages"))
        .header("Content-Type", "application/json+protobuf")
        .header("x-goog-api-key", API_KEY)
        .header("x-user-agent", "grpc-web-javascript/0.1")
        .header("Origin", ORIGIN)
        .header("Referer", &format!("{ORIGIN}/"));
    if !cookies.is_empty() {
        request = request.header("Cookie", &cookies);
    }
    if let Some(auth) = google::authorization(&cookies) {
        request = request.header("Authorization", &auth);
    }
    let Ok(mut response) = request.send(&body) else {
        let _ = events.send(crate::adapters::messages::Event::Closed(
            "Couldn't reach Messages.".into(),
        ));
        return true;
    };
    if !(200..300).contains(&response.status().as_u16()) {
        let status = response.status().as_u16();
        let _ = events.send(crate::adapters::messages::Event::Closed(format!(
            "Messages refused the request ({status})."
        )));
        return true;
    }
    {
        let mut live = live.lock().unwrap_or_else(|p| p.into_inner());
        for value in response.headers().get_all("set-cookie") {
            if let Some(pair) = value.to_str().ok().and_then(|line| line.split(';').next()) {
                live.cookies = merge_cookies(&live.cookies, pair);
            }
        }
        if !live.saved.cookies.is_empty() {
            let cookies = live.cookies.clone();
            google::update_cookies(&mut live.saved, &cookies);
        }
    }
    // The long-poll is open. Commands (SetActiveSession, then the inbox) go out on another
    // request while this one stays up to carry the phone's answer.
    if announce {
        let _ = events.send(crate::adapters::messages::Event::Ready);
    }
    let mut reader = response.body_mut().as_reader();
    let mut pending = Vec::new();
    let mut opened = false;
    let mut buf = [0u8; 8192];
    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => pending.extend_from_slice(&buf[..n]),
        }
        if !opened {
            if pending.len() < 2 {
                continue;
            }
            if !pending.starts_with(b"[[") {
                break;
            }
            pending.drain(..2);
            opened = true;
        }
        // Google leaves the two outer arrays open for the lifetime of the stream.
        // Only the individual payload arrays are complete JSON values.
        while pending
            .first()
            .is_some_and(|byte| byte.is_ascii_whitespace() || *byte == b',')
        {
            pending.remove(0);
        }
        if pending.starts_with(b"]]") {
            break;
        }
        let (values, used) = complete_values(&pending);
        for value in &values {
            if on_payload(value, &live, &events, &waiters, &paired) {
                stop.store(true, std::sync::atomic::Ordering::Relaxed);
                return false;
            }
        }
        google::acknowledge(&values, &live);
        pending.drain(..used);
    }
    stream_exit_failed()
}

/// Decode completed payloads while the outer HTTP stream remains open.
fn complete_values(bytes: &[u8]) -> (Vec<serde_json::Value>, usize) {
    let mut values = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    let mut used = 0;
    let mut quoted = false;
    let mut escaped = false;
    for (at, &byte) in bytes.iter().enumerate() {
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
            continue;
        }
        match byte {
            b'"' => quoted = true,
            b'[' => {
                if depth == 0 {
                    start = at;
                }
                depth += 1;
            }
            b']' if depth == 0 => break,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    if let Ok(value) = serde_json::from_slice(&bytes[start..=at]) {
                        values.push(value);
                    }
                    used = at + 1;
                }
            }
            _ if depth == 0 => used = at + 1,
            _ => {}
        }
    }
    (values, used)
}

/// True when the phone revoked the pairing and the poll should stop.
fn on_payload(
    value: &serde_json::Value,
    live: &Mutex<Live>,
    events: &Sender<crate::adapters::messages::Event>,
    waiters: &Mutex<Vec<Waiter>>,
    paired: &Option<Sender<Saved>>,
) -> bool {
    // Long-poll chunks are one payload, or a stream wrapped in an extra array.
    if let Some(list) = value.as_array() {
        if list.first().and_then(|item| item.as_array()).is_some() && at(value, 2).is_none() {
            for item in list {
                if on_payload(item, live, events, waiters, paired) {
                    return true;
                }
            }
            return false;
        }
    }
    let Some(data) = at(value, 2) else {
        return false;
    };
    let route = at(data, 2).map(as_i64).unwrap_or(0);
    let message = at(data, 12).map(as_bytes).unwrap_or_default();
    if route == 14 {
        if on_pair(&message, live, paired) {
            let _ = events.send(crate::adapters::messages::Event::Ended(UNPAIRED.into()));
            return true;
        }
        return false;
    }
    if route != 19 || message.is_empty() {
        return false;
    }
    let rpc = fields(&message);
    let echoed = field_text(&rpc, 1);
    let action = field_int(&rpc, 4);
    let encrypted = field_bytes(&rpc, 8);
    let plain = if encrypted.is_empty() {
        field_bytes(&rpc, 5)
    } else {
        let held = live.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        open_seal(&held.aes, &held.hmac, &encrypted).unwrap_or_default()
    };
    if !echoed.is_empty() && !plain.is_empty() {
        let mut waiting = waiters
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(at) = waiting
            .iter()
            .position(|waiter| waiter.request == echoed || waiter.session == echoed)
        {
            let waiter = waiting.remove(at);
            drop(waiting);
            let _ = waiter.reply.send(plain.clone());
        }
    }
    if plain.is_empty() {
        return false;
    }
    match action {
        1 => {
            let chats = decode_chats(&plain);
            if !chats.is_empty() {
                live.lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .chats = chats.clone();
                let _ = events.send(crate::adapters::messages::Event::Chats(chats));
            }
        }
        16 => on_updates(&plain, live, events),
        _ => {}
    }
    false
}

/// True when this notice is a revoke, so the caller ends the session.
fn on_pair(message: &[u8], live: &Mutex<Live>, paired: &Option<Sender<Saved>>) -> bool {
    let list = fields(message);
    let paired_bytes = field_bytes(&list, 4);
    if paired_bytes.is_empty() {
        return revoked_pair(message);
    }
    let inner = fields(&paired_bytes);
    let mobile = decode_device(&field_bytes(&inner, 1));
    let (token, ttl) = decode_token(&field_bytes(&inner, 2));
    let browser = decode_device(&field_bytes(&inner, 3));
    if mobile.source.is_empty() {
        return false;
    }
    let mut held = live.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    held.saved.account = mobile.source.clone();
    held.saved.mobile = mobile.source;
    held.saved.mobile_user = mobile.user;
    if !browser.source.is_empty() {
        held.saved.browser = browser.source;
        held.saved.browser_user = browser.user;
    }
    if !token.is_empty() {
        held.token = token.clone();
        held.saved.token = b64(&token);
        held.ttl = ttl;
        held.saved.ttl = ttl;
    }
    if let Some(paired) = paired {
        let _ = paired.send(held.saved.clone());
    }
    false
}

fn on_updates(bytes: &[u8], live: &Mutex<Live>, events: &Sender<crate::adapters::messages::Event>) {
    let list = fields(bytes);
    if let Some(conversations) = field(&list, 2) {
        let chats = decode_chats(&conversations.bytes);
        if !chats.is_empty() {
            live.lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .chats = chats.clone();
            let _ = events.send(crate::adapters::messages::Event::Chats(chats));
        }
    }
    if let Some(messages) = field(&list, 3) {
        for note in decode_messages(&messages.bytes) {
            let _ = events.send(crate::adapters::messages::Event::Message(note));
        }
    }
    if let Some(typing) = field(&list, 4) {
        let data = fields(&field_bytes(&fields(&typing.bytes), 2));
        let conversation = field_text(&data, 1);
        let user = fields(&field_bytes(&data, 2));
        let who = field_text(&user, 2);
        let started = field_int(&data, 3) == 1;
        if !conversation.is_empty() {
            let _ = events.send(crate::adapters::messages::Event::Typing {
                conversation,
                by: who.clone(),
                name: who,
                started,
            });
        }
    }
}

fn decode_chats(bytes: &[u8]) -> Vec<Chat> {
    // ListConversationsResponse field 2 is repeated Conversation. UpdateEvents wraps the same.
    let list = fields(bytes);
    let mut chats = Vec::new();
    for item in list.iter().filter(|field| field.number == 2) {
        let conv = fields(&item.bytes);
        // A conversation's own field 2 is the name, a string, so a nested conversation is the
        // one whose field 1 is a non-empty id and field 2 is text rather than another message.
        let id = field_text(&conv, 1);
        if id.is_empty() {
            chats.extend(decode_chats(&item.bytes));
            continue;
        }
        let mut title = field_text(&conv, 2);
        let group = field_int(&conv, 10) != 0;
        let unread = field_int(&conv, 6) != 0;
        let carrier = field_int(&conv, 22);
        let mut mine = field_text(&conv, 11);
        let mut peer = String::new();
        for person in conv.iter().filter(|field| field.number == 20) {
            let participant = fields(&person.bytes);
            let info = fields(&field_bytes(&participant, 1));
            let pid = field_text(&info, 3);
            let name = {
                let full = field_text(&participant, 3);
                if full.is_empty() {
                    field_text(&participant, 2)
                } else {
                    full
                }
            };
            let is_me = field_int(&participant, 6) != 0;
            if !is_me && !group && peer.is_empty() {
                peer = pid.clone();
            }
            if is_me && !pid.is_empty() {
                mine = pid;
            } else if title.is_empty() && !name.is_empty() && !group {
                title = name;
            } else if title.is_empty() && !group {
                let number = field_text(&participant, 15);
                if !number.is_empty() {
                    title = number;
                }
            }
        }
        if title.is_empty() {
            title = if group {
                "Group".into()
            } else {
                "Messages".into()
            };
        }
        chats.push(Chat {
            id,
            title,
            group,
            unread,
            mine,
            peer,
            carrier,
            updated: unix_seconds(field_int(&conv, 5) as i64),
            phones: false,
        });
    }
    chats
}

fn emoji_of(unicode: &str, kind: u64) -> String {
    if !unicode.is_empty() {
        return unicode.to_owned();
    }
    match kind {
        1 => "👍",
        2 | 12 => "❤️",
        3 => "😂",
        4 => "😮",
        5 => "😢",
        6 | 11 => "😡",
        7 => "👎",
        9 => "🤔",
        10 => "😭",
        _ => "",
    }
    .to_owned()
}

fn reaction_type(emoji: &str) -> u64 {
    match emoji {
        "👍" => 1,
        "❤️" | "❤" => 2,
        "😂" => 3,
        "😮" => 4,
        "😢" => 5,
        "😡" => 6,
        "👎" => 7,
        "🤔" => 9,
        "😭" => 10,
        _ => 8,
    }
}

fn decode_piece(bytes: &[u8], action_id: &str) -> Option<Piece> {
    let media = fields(bytes);
    let mime = field_text(&media, 14);
    let mut name = field_text(&media, 4);
    if name.is_empty() {
        name = label_of(&mime).to_owned();
    }
    let size = field_int(&media, 5);
    let dims = fields(&field_bytes(&media, 6));
    let mut inline = field_bytes(&media, 7);
    let key = field_bytes(&media, 11);
    if !inline.is_empty() && key.len() == 32 && inline.first() == Some(&0) {
        if let Ok(opened) = super::media::unpack(&key, &inline) {
            inline = opened;
        }
    }
    let media_id = field_text(&media, 2);
    if name.is_empty() && media_id.is_empty() && inline.is_empty() {
        return None;
    }
    Some(Piece {
        name,
        mime,
        media_id,
        key,
        action_id: action_id.to_owned(),
        size: if size == 0 { inline.len() as u64 } else { size },
        width: field_int(&dims, 1) as u32,
        height: field_int(&dims, 2) as u32,
        inline,
    })
}

fn label_of(mime: &str) -> &'static str {
    if mime.starts_with("image/") {
        "Photo"
    } else if mime.starts_with("video/") {
        "Video"
    } else if mime.starts_with("audio/") {
        "Audio"
    } else if mime.contains("vcard") {
        "Contact"
    } else {
        "File"
    }
}

fn decode_reactions(msg: &[Field]) -> Vec<Mark> {
    let mut marks: Vec<Mark> = Vec::new();
    for entry in msg.iter().filter(|field| field.number == 19) {
        let parts = fields(&entry.bytes);
        let data = fields(&field_bytes(&parts, 1));
        let emoji = emoji_of(&field_text(&data, 1), field_int(&data, 2));
        if emoji.is_empty() {
            continue;
        }
        let who = field_texts(&parts, 2);
        if let Some(have) = marks.iter_mut().find(|mark| mark.emoji == emoji) {
            for id in who {
                if !have.who.contains(&id) {
                    have.who.push(id);
                }
            }
        } else {
            marks.push(Mark { emoji, who });
        }
    }
    marks
}

/// Phone status text shown under a failed row. Same bound as a forwarded title.
const DETAIL_CAP: usize = 200;

fn decode_messages(bytes: &[u8]) -> Vec<Note> {
    let list = fields(bytes);
    let mut notes = Vec::new();
    for item in list
        .iter()
        .filter(|field| field.number == 2 || field.number == 1)
    {
        let msg = fields(&item.bytes);
        let id = field_text(&msg, 1);
        let conversation = field_text(&msg, 7);
        if id.is_empty() || conversation.is_empty() {
            if field(&msg, 2).is_some() {
                notes.extend(decode_messages(&item.bytes));
            }
            continue;
        }
        let status_fields = fields(&field_bytes(&msg, 4));
        let status = field_int(&status_fields, 2);
        let said = field_text(&status_fields, 5);
        let mut detail = if said.is_empty() {
            field_text(&status_fields, 4)
        } else {
            said
        };
        // Drawn under a failed row, not as the message. `Message::bound` does not clip it.
        protocol::clip(&mut detail, DETAIL_CAP);
        let tmp = field_text(&msg, 12);
        // A tombstone is the only published word that the phones turned encryption on or off.
        if let Some(on) = between_phones(status) {
            notes.push(Note {
                id,
                conversation,
                status,
                phones: Some(on),
                ..Note::default()
            });
            continue;
        }
        // 200–299 is a tombstone, 300 a delete. Neither is a message to show.
        if (200..300).contains(&status) || status == 300 {
            continue;
        }
        let wire = field_int(&msg, 11);
        let sent = unix_seconds(field_int(&msg, 5) as i64);
        let mut author = field_text(&msg, 9);
        let mut name = String::new();
        let mut outgoing = false;
        if let Some(sender) = field(&msg, 25) {
            let participant = fields(&sender.bytes);
            let info = fields(&field_bytes(&participant, 1));
            if author.is_empty() {
                author = field_text(&info, 3);
            }
            name = field_text(&participant, 3);
            if name.is_empty() {
                name = field_text(&participant, 2);
            }
            if name.is_empty() {
                name = field_text(&participant, 15);
            }
            outgoing = field_int(&participant, 6) != 0;
        }
        let mut text = String::new();
        let mut file = false;
        let mut pieces = Vec::new();
        for info in msg.iter().filter(|field| field.number == 10) {
            let parts = fields(&info.bytes);
            let action_id = field_text(&parts, 1);
            if let Some(content) = field(&parts, 2) {
                let words = field_text(&fields(&content.bytes), 1);
                if !words.is_empty() {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(&words);
                }
            }
            if let Some(media) = field(&parts, 3) {
                file = true;
                if let Some(piece) = decode_piece(&media.bytes, &action_id) {
                    pieces.push(piece);
                }
            }
            if field(&parts, 5).is_some() && pieces.is_empty() {
                file = true;
                pieces.push(Piece {
                    name: "Location".into(),
                    mime: "text/plain".into(),
                    ..Piece::default()
                });
            }
        }
        let reactions = decode_reactions(&msg);
        let reply = field(&msg, 21)
            .map(|field| field_text(&fields(&field.bytes), 1))
            .unwrap_or_default();
        if text.is_empty()
            && file
            && pieces
                .iter()
                .all(|piece| piece.inline.is_empty() && piece.media_id.is_empty())
        {
            text = pieces
                .iter()
                .map(|piece| piece.name.as_str())
                .filter(|name| !name.is_empty())
                .collect::<Vec<_>>()
                .join(", ");
            if text.is_empty() {
                text = "File".into();
            }
        }
        notes.push(Note {
            id,
            conversation,
            author,
            name,
            sent,
            text,
            outgoing,
            file,
            pieces,
            reactions,
            status,
            tmp,
            detail,
            wire,
            phones: None,
            reply,
        });
    }
    notes
}

/// The lab sets `TINYCLIENT_PROVE=messages-reply`. A build from before this ignores that
/// and keeps serving, so the same driver fails there.
pub(super) fn prove_message_reply() -> Result<(), String> {
    if allows_reply(1) || allows_reply(0) || !allows_reply(2) {
        return Err("a reply was allowed outside an RCS chat".into());
    }
    let asked = fields(&send_request("rcs-chat", "On my way", "tmp-1", "me", Some("m-orig")));
    let quoted = fields(&field_bytes(&asked, 8));
    if field_text(&quoted, 1) != "m-orig" {
        return Err("the send dropped the message it answers".into());
    }
    let plain = fields(&send_request("sms-chat", "On my way", "tmp-2", "me", None));
    if field(&plain, 8).is_some() {
        return Err("a plain send carried a reply".into());
    }
    let media = fields(&send_media_request(
        "rcs-chat",
        "",
        "tmp-3",
        "me",
        "media-1",
        &[1; 32],
        "a.png",
        "image/png",
        4,
        Some("m-orig"),
    ));
    if field_text(&fields(&field_bytes(&media, 8)), 1) != "m-orig" {
        return Err("the file send dropped the message it answers".into());
    }
    let mut target = Buf::new();
    target.text(1, "m-orig");
    let mut message = Buf::new();
    message.text(1, "m-reply");
    message.text(7, "rcs-chat");
    message.msg(21, &target.0);
    let mut wrap = Buf::new();
    wrap.msg(1, &message.0);
    let notes = decode_messages(&wrap.0);
    if notes.first().map(|note| note.reply.as_str()) != Some("m-orig") {
        return Err("an incoming reply lost the message it answers".into());
    }
    Ok(())
}

/// The lab sets `TINYCLIENT_PROVE=messages-history`. A build from before this ignores that
/// and keeps serving, so the same driver fails there.
pub(super) fn prove_message_history() -> Result<(), String> {
    let cursor = Cursor { id: "m-0061".into(), millis: 1_700_000_061_000 };
    let asked = fields(&messages_request("phone-grace", MESSAGE_PAGE, Some(&cursor)));
    if field_text(&asked, 2) != "phone-grace" {
        return Err("the history request named the wrong chat".into());
    }
    if field_int(&asked, 3) != MESSAGE_PAGE {
        return Err(format!("the history request asked for {} rows", field_int(&asked, 3)));
    }
    let inner = fields(&field_bytes(&asked, 5));
    if field_text(&inner, 1) != "m-0061" || field_int(&inner, 2) != 1_700_000_061_000 {
        return Err("the history cursor dropped the message or its time".into());
    }
    let first = fields(&messages_request("phone-grace", MESSAGE_PAGE, None));
    if field(&first, 5).is_some() {
        return Err("the newest history page sent a cursor".into());
    }
    let inbox = fields(&list_request(CHAT_PAGE, Some(&cursor)));
    if field_int(&inbox, 2) != CHAT_PAGE || field_int(&inbox, 4) != 1 {
        return Err("the inbox request lost its page or its folder".into());
    }
    let inbox_cursor = fields(&field_bytes(&inbox, 5));
    if field_text(&inbox_cursor, 1) != "m-0061" || field_int(&inbox_cursor, 2) != 1_700_000_061_000 {
        return Err("the inbox cursor dropped the chat or its time".into());
    }
    if !more_messages(MESSAGE_PAGE as usize) || more_messages(20) || more_messages(0) {
        return Err("a short page was treated as another page".into());
    }
    Ok(())
}

/// The lab sets `TINYCLIENT_PROVE=messages-transport`. A build from before this ignores that
/// and keeps serving, so the same driver fails there.
pub(super) fn prove_message_transport() -> Result<(), String> {
    let mut sms = Buf::new();
    sms.text(1, "sms-chat");
    sms.text(2, "Sam");
    sms.int(22, 1);
    let mut sms_list = Buf::new();
    sms_list.msg(2, &sms.0);
    let sms_chat = decode_chats(&sms_list.0);
    let sms_label = composer_label(sms_chat[0].carrier, sms_chat[0].phones);
    if sms_label != "Text message" {
        return Err(format!("an SMS chat was labelled {sms_label:?}"));
    }

    let mut rcs = Buf::new();
    rcs.text(1, "rcs-chat");
    rcs.text(2, "Rae");
    rcs.int(22, 2);
    let mut rcs_list = Buf::new();
    rcs_list.msg(2, &rcs.0);
    let rcs_chat = decode_chats(&rcs_list.0);
    let mut status = Buf::new();
    status.int(2, 216);
    let mut tomb = Buf::new();
    tomb.text(1, "tomb");
    tomb.text(7, "rcs-chat");
    tomb.msg(4, &status.0);
    let mut wrap = Buf::new();
    wrap.msg(1, &tomb.0);
    let switched = decode_messages(&wrap.0);
    if switched.first().and_then(|note| note.phones) != Some(true) {
        return Err("the encrypted-RCS tombstone was dropped".into());
    }
    let rcs_label = composer_label(rcs_chat[0].carrier, true);
    if !rcs_label.contains("RCS message") || !rcs_label.contains("Encrypted between phones") {
        return Err(format!("an encrypted RCS chat was labelled {rcs_label:?}"));
    }

    let mut mms = Buf::new();
    mms.text(1, "m-mms");
    mms.text(7, "sms-chat");
    mms.int(11, 2);
    let mut mms_wrap = Buf::new();
    mms_wrap.msg(1, &mms.0);
    let mms_note = decode_messages(&mms_wrap.0);
    if wire_tag(mms_note[0].wire, 1) != "MMS" {
        return Err("an MMS inside an SMS chat had no MMS tag".into());
    }
    if !wire_tag(1, 1).is_empty() {
        return Err("an SMS inside an SMS chat was tagged".into());
    }
    if shows_typing(1) || shows_receipts(1) {
        return Err("an SMS chat still offers typing or read ticks".into());
    }
    if !shows_typing(2) || !shows_receipts(2) {
        return Err("an RCS chat lost typing or read ticks".into());
    }
    if MMS_WARNING != "Sends as MMS; may be compressed" {
        return Err("the file warning changed".into());
    }
    Ok(())
}

/// The lab sets `TINYCLIENT_PROVE=messages-unpaired`. A build from before this ignores that
/// and keeps serving, so the same driver fails there.
pub fn prove_unpaired() -> Result<(), String> {
    let mut device = Buf::new();
    device.text(1, "phone");
    let mut revoked = Buf::new();
    revoked.msg(1, &device.0);
    let mut notice = Buf::new();
    notice.msg(5, &revoked.0);
    if !revoked_pair(&notice.0) {
        return Err("a revoke on the pairing route was ignored".into());
    }
    let mut gate = Gate::default();
    if gate.observe(http_stop(403)) != Step::Retry {
        return Err("one refusal signed out".into());
    }
    if gate.observe(http_stop(403)) != Step::End {
        return Err("a second refusal kept reconnecting".into());
    }
    if http_stop(503) != Stop::Closed {
        return Err("a server error was treated as unpaired".into());
    }
    if next_pause(Duration::from_secs(2)) != Duration::from_secs(4) {
        return Err("the poll pause did not grow".into());
    }
    if UNPAIRED != "Your phone unpaired this computer. Pair again." {
        return Err("the re-pair sentence changed".into());
    }
    let words = "Google did not issue a Messages session. Sign in again.";
    let mut gate = Gate::default();
    if !matches!(gate.fail(words), crate::adapters::Failure::Dropped(_)) {
        return Err("one Sign in again ended the session".into());
    }
    if !matches!(gate.fail(words), crate::adapters::Failure::SignedOut(_)) {
        return Err("a second Sign in again kept reconnecting".into());
    }
    if stream_exit_failed() {
        return Err("an empty long-poll close counted as a failure".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_type_comes_from_its_bytes_then_its_name() {
        file_type_samples().unwrap();
    }

    #[test]
    fn a_history_page_carries_the_message_it_starts_after() {
        prove_message_history().unwrap();
    }

    #[test]
    fn a_picture_a_caption_and_a_reaction_decode_and_a_tombstone_does_not() {
        let mut media = Buf::new();
        media.text(2, "media-1");
        media.text(4, "IMG_1.jpg");
        media.text(14, "image/jpeg");
        media.int(5, 12);
        media.bytes(11, &[9; 32]);
        let mut info = Buf::new();
        info.text(1, "part-1");
        info.msg(3, &media.0);
        let mut words = Buf::new();
        words.text(1, "look");
        let mut caption = Buf::new();
        caption.msg(2, &words.0);
        let mut data = Buf::new();
        data.int(2, 1);
        let mut entry = Buf::new();
        entry.msg(1, &data.0);
        entry.text(2, "phone-grace");
        let mut message = Buf::new();
        message.text(1, "m1");
        message.int(5, 1_791_892_800_000_000);
        message.text(7, "c1");
        message.text(9, "phone-grace");
        message.msg(10, &info.0);
        message.msg(10, &caption.0);
        message.msg(19, &entry.0);
        let mut gone = Buf::new();
        gone.text(1, "m-gone");
        gone.text(7, "c1");
        let mut status = Buf::new();
        status.int(2, 200);
        gone.msg(4, &status.0);
        let mut wrap = Buf::new();
        wrap.msg(1, &message.0);
        wrap.msg(1, &gone.0);
        let notes = decode_messages(&wrap.0);
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].text, "look");
        assert_eq!(notes[0].pieces[0].name, "IMG_1.jpg");
        assert_eq!(notes[0].pieces[0].media_id, "media-1");
        assert_eq!(notes[0].pieces[0].action_id, "part-1");
        assert_eq!(notes[0].reactions[0].emoji, "👍");
        assert_eq!(notes[0].reactions[0].who, ["phone-grace"]);
    }

    #[test]
    fn my_message_says_whether_it_was_delivered_or_read() {
        let mine = |id: &str, code: u64| {
            let mut status = Buf::new();
            status.int(2, code);
            let mut message = Buf::new();
            message.text(1, id);
            message.text(7, "c1");
            message.msg(4, &status.0);
            message
        };
        let mut wrap = Buf::new();
        for (id, code) in [("m1", 1), ("m2", 2), ("m3", 11), ("m4", 8)] {
            wrap.msg(1, &mine(id, code).0);
        }
        let notes = decode_messages(&wrap.0);
        let said: Vec<Option<Receipt>> = notes.iter().map(|note| receipt(note.status)).collect();
        assert_eq!(said, [None, Some(Receipt::Delivered), Some(Receipt::Read), None]);
    }

    #[test]
    fn a_failure_status_names_the_temporary_id_and_is_not_sent() {
        let mut status = Buf::new();
        status.int(2, 13);
        let mut message = Buf::new();
        message.text(1, "phone-1");
        message.text(7, "c1");
        message.text(12, "tmp-1");
        message.msg(4, &status.0);
        let mut wrap = Buf::new();
        wrap.msg(1, &message.0);
        let notes = decode_messages(&wrap.0);
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].id, "phone-1");
        assert_eq!(notes[0].tmp, "tmp-1");
        assert_eq!(progress(notes[0].status), Progress::Failed("That message is too large."));
        assert_eq!(progress(5), Progress::Pending);
        assert_eq!(progress(8), Progress::Failed("Not sent. No service."));
        assert_eq!(progress(9), Progress::Failed("That number is invalid."));
        assert_eq!(progress(1), Progress::Sent);
        assert_eq!(progress(2), Progress::Sent);
        assert!(matches!(progress(99), Progress::Failed(_)));
        assert!(matches!(progress(24), Progress::Failed(_)));
    }

    #[test]
    fn a_long_status_line_is_clipped_before_the_note_is_built() {
        let long = "x".repeat(500);
        let decode = |field: u32| {
            let mut status = Buf::new();
            status.int(2, 13);
            status.text(field, &long);
            let mut message = Buf::new();
            message.text(1, "phone-1");
            message.text(7, "c1");
            message.msg(4, &status.0);
            let mut wrap = Buf::new();
            wrap.msg(1, &message.0);
            decode_messages(&wrap.0)
        };
        let from_text = decode(5);
        assert_eq!(from_text.len(), 1);
        assert_eq!(from_text[0].detail.chars().count(), 200);
        let from_err = decode(4);
        assert_eq!(from_err[0].detail.chars().count(), 200);
        let mut both = Buf::new();
        both.int(2, 13);
        both.text(4, &long);
        both.text(5, "phone said this");
        let mut message = Buf::new();
        message.text(1, "phone-1");
        message.text(7, "c1");
        message.msg(4, &both.0);
        let mut wrap = Buf::new();
        wrap.msg(1, &message.0);
        let notes = decode_messages(&wrap.0);
        assert_eq!(notes[0].detail, "phone said this");
    }

    #[test]
    fn a_chat_names_the_other_side_and_their_picture_decodes() {
        let participant = |pid: &str, name: &str, me: bool| {
            let mut info = Buf::new();
            info.int(1, 2);
            info.text(3, pid);
            let mut person = Buf::new();
            person.msg(1, &info.0);
            person.text(3, name);
            person.int(6, u64::from(me));
            person.0
        };
        let mut conv = Buf::new();
        conv.text(1, "c1");
        conv.msg(20, &participant("p-me", "Ada", true));
        conv.msg(20, &participant("p-grace", "Grace", false));
        let mut list = Buf::new();
        list.msg(2, &conv.0);
        let chats = decode_chats(&list.0);
        assert_eq!((chats[0].peer.as_str(), chats[0].mine.as_str(), chats[0].title.as_str()), ("p-grace", "p-me", "Grace"));
        assert_eq!(thumbnail_request(&["p-grace".to_owned()]), {
            let mut req = Buf::new();
            req.text(1, "p-grace");
            req.0
        });

        let thumbnail = |pid: &str, picture: &[u8]| {
            let mut data = Buf::new();
            data.bytes(3, picture);
            let mut one = Buf::new();
            one.text(1, pid);
            one.msg(2, &data.0);
            one.0
        };
        let jpeg = [0xff, 0xd8, 0xff, 0xe0, 1, 2, 3];
        let mut answer = Buf::new();
        answer.msg(1, &thumbnail("p-grace", &jpeg));
        // Someone the phone has no picture for, and something that is not a picture.
        answer.msg(1, &thumbnail("p-alan", &[]));
        answer.msg(1, &thumbnail("p-odd", b"<html>"));
        assert_eq!(decode_thumbnails(&answer.0), [("p-grace".to_owned(), jpeg.to_vec())]);
    }

    #[test]
    fn an_rcs_reply_names_the_message_it_answers_and_an_sms_send_does_not() {
        prove_message_reply().unwrap();
    }

    #[test]
    fn a_chat_says_sms_or_rcs_and_a_tombstone_says_whether_the_phones_encrypt_it() {
        prove_message_transport().unwrap();
        let mut plain = Buf::new();
        plain.text(1, "m-sms");
        plain.text(7, "sms-chat");
        plain.int(11, 1);
        let mut wrap = Buf::new();
        wrap.msg(1, &plain.0);
        let note = decode_messages(&wrap.0);
        assert_eq!(wire_tag(note[0].wire, 1), "");
        assert_eq!(between_phones(200), None);
        assert_eq!(between_phones(214), Some(false));
    }

    #[test]
    fn a_revoke_on_the_pairing_route_ends_and_auth_failures_need_two() {
        let mut device = Buf::new();
        device.text(1, "phone");
        let mut revoked = Buf::new();
        revoked.msg(1, &device.0);
        let mut notice = Buf::new();
        notice.msg(5, &revoked.0);
        assert!(revoked_pair(&notice.0));
        let mut paired = Buf::new();
        paired.text(1, "phone");
        let mut kept = Buf::new();
        kept.msg(4, &paired.0);
        assert!(!revoked_pair(&kept.0));
        assert_eq!(http_stop(401), Stop::Auth);
        assert_eq!(http_stop(403), Stop::Auth);
        assert_eq!(http_stop(503), Stop::Closed);
        assert_eq!(http_stop(404), Stop::Closed);
        let mut gate = Gate::default();
        assert_eq!(gate.observe(Stop::Auth), Step::Retry);
        assert_eq!(gate.observe(Stop::Auth), Step::End);
        let mut gate = Gate::default();
        let _ = gate.observe(Stop::Auth);
        let _ = gate.observe(Stop::Closed);
        assert_eq!(gate.observe(Stop::Auth), Step::Retry);
        assert_eq!(next_pause(Duration::from_secs(2)), Duration::from_secs(4));
        assert_eq!(next_pause(Duration::from_secs(600)), Duration::from_secs(900));
        assert!(cookie_failure_ends(
            "Google did not issue a Messages session. Sign in again."
        ));
        assert!(!cookie_failure_ends("Messages refused the request (403)."));
        assert!(!cookie_failure_ends("Messages: connection reset"));
    }

    #[test]
    fn a_quiet_long_poll_close_stays_at_the_floor() {
        assert!(
            !stream_exit_failed(),
            "an empty close under the 90-second timeout counted as a failed poll"
        );
        let mut pause = POLL_FLOOR;
        for _ in 0..14 {
            pause = if stream_exit_failed() {
                next_pause(pause)
            } else {
                POLL_FLOOR
            };
        }
        assert_eq!(pause, Duration::from_secs(2));
    }

    #[test]
    fn sign_in_again_needs_two_strikes_before_the_session_ends() {
        let words = "Google did not issue a Messages session. Sign in again.";
        let mut gate = Gate::default();
        assert!(
            matches!(gate.fail(words), crate::adapters::Failure::Dropped(_)),
            "one Sign in again ended the session"
        );
        assert!(
            matches!(gate.fail(words), crate::adapters::Failure::SignedOut(_)),
            "a second Sign in again kept retrying"
        );
        let mut gate = Gate::default();
        assert!(matches!(
            gate.fail(words),
            crate::adapters::Failure::Dropped(_)
        ));
        assert!(
            matches!(gate.fail("Messages: connection reset"), crate::adapters::Failure::Dropped(_)),
            "a network close was not a retry"
        );
        assert!(
            matches!(gate.fail(words), crate::adapters::Failure::Dropped(_)),
            "a Sign in again after a close ended immediately"
        );
    }
}
