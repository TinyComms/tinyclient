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
pub(super) const FILE_CAP: u64 = 100 * 1024 * 1024;
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

    #[cfg(test)]
    fn double(&mut self, field: u32, value: f64) {
        self.key(field, 1);
        self.0.extend_from_slice(&value.to_bits().to_le_bytes());
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
    fixed64: Option<u64>,
    fixed32: Option<u32>,
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
                    fixed64: None,
                    fixed32: None,
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
                    fixed64: None,
                    fixed32: None,
                });
            }
            1 => {
                if input.len() < 8 {
                    break;
                }
                let raw = u64::from_le_bytes(input[..8].try_into().unwrap());
                input = &input[8..];
                out.push(Field {
                    number,
                    bytes: Vec::new(),
                    varint: None,
                    fixed64: Some(raw),
                    fixed32: None,
                });
            }
            5 => {
                if input.len() < 4 {
                    break;
                }
                let raw = u32::from_le_bytes(input[..4].try_into().unwrap());
                input = &input[4..];
                out.push(Field {
                    number,
                    bytes: Vec::new(),
                    varint: None,
                    fixed64: None,
                    fixed32: Some(raw),
                });
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

fn field_double(list: &[Field], number: u32) -> Option<f64> {
    let f = field(list, number)?;
    if let Some(bits) = f.fixed64 {
        return Some(f64::from_bits(bits));
    }
    if let Some(bits) = f.fixed32 {
        return Some(f64::from(f32::from_bits(bits)));
    }
    if !f.bytes.is_empty() {
        return std::str::from_utf8(&f.bytes).ok()?.trim().parse::<f64>().ok();
    }
    None
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

/// Action 46, `UnpairRequest`: the pairing attempt id in field 1. Gaia pairing actions travel
/// in the clear (field 3 of the envelope), the same as 44, 45 and 47.
pub fn unpair_request(attempt: &str) -> Vec<u8> {
    let mut buf = Buf::new();
    buf.text(1, attempt);
    buf.0
}

pub fn clear_action(action: u32) -> bool {
    matches!(action, 44..=47)
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

/// `ListConversationsRequest.folder`: inbox, archive, and spam & blocked.
fn list_request(count: u64, folder: u64, before: Option<&Cursor>) -> Vec<u8> {
    let mut buf = Buf::new();
    buf.int(2, count);
    buf.int(4, folder);
    if let Some(cursor) = before {
        buf.msg(5, &cursor_message(cursor));
    }
    buf.0
}

/// `DeleteMessageRequest.messageID` is field 2. ActionType DELETE_MESSAGE is 23.
fn delete_message_request(message: &str) -> Vec<u8> {
    let mut req = Buf::new();
    req.text(2, message);
    req.0
}

/// `UpdateConversationRequest`: action DELETE is 1 at field 2, the id is field 3, and
/// `DeleteConversationData.conversationID` is field 1 inside field 6. The phone number there
/// is optional and this does not invent one.
fn delete_conversation_request(conversation: &str) -> Vec<u8> {
    let mut data = Buf::new();
    data.text(1, conversation);
    let mut req = Buf::new();
    req.int(2, 1);
    req.text(3, conversation);
    req.msg(6, &data.0);
    req.0
}

/// Where a `Conversation.status` sits. The layout's cross-country folder has no shelf here,
/// so it stays off the lists, the same as a deleted chat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Place {
    Main,
    Archive,
    Request,
    Hidden,
}

pub(super) fn place(status: u64) -> Place {
    match status {
        2 | 4 => Place::Archive,
        5 | 6 => Place::Request,
        3 | 7 | 8 => Place::Hidden,
        _ => Place::Main,
    }
}

/// The lab sets `TINYCLIENT_PROVE=messages-folders`. A build from before this ignores that
/// and keeps serving, so the same driver fails there.
pub(super) fn prove_message_folders() -> Result<(), String> {
    for folder in [1u64, 2, 5] {
        let got = field_int(&fields(&list_request(50, folder, None)), 4);
        if got != folder {
            return Err(format!("the list asked for folder {got}, not {folder}"));
        }
    }
    if place(1) != Place::Main || place(0) != Place::Main {
        return Err("an open chat left the main list".into());
    }
    if place(2) != Place::Archive || place(4) != Place::Archive {
        return Err("an archived chat was not filed on the archive".into());
    }
    if place(5) != Place::Request || place(6) != Place::Request {
        return Err("spam or a blocked chat was not filed as a request".into());
    }
    if place(3) != Place::Hidden || place(7) != Place::Hidden || place(8) != Place::Hidden {
        return Err("a deleted chat stayed on a list".into());
    }
    Ok(())
}

fn block_request(conversation: &str) -> Vec<u8> {
    let mut data = Buf::new();
    data.text(1, conversation);
    let mut extra = Buf::new();
    extra.int(2, 1);
    let mut req = Buf::new();
    req.int(2, 7); // BLOCK
    req.text(3, conversation);
    req.msg(5, &extra.0);
    req.msg(6, &data.0);
    req.0
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

fn send_request(
    conversation: &str,
    text: &str,
    tmp: &str,
    mine: &str,
    reply: Option<&str>,
    sim: Option<&str>,
) -> Vec<u8> {
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
    if let Some(sim_id) = sim.filter(|id| !id.is_empty()) {
        let mut sim_payload = Buf::new();
        sim_payload.text(1, sim_id);
        req.msg(4, &sim_payload.0);
    }
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

/// One file already sealed and uploaded, named the way the phone shows it.
pub(super) struct MediaPart<'a> {
    pub media_id: &'a str,
    pub key: &'a [u8],
    pub name: &'a str,
    pub mime: &'a str,
    pub size: u64,
    /// True when this part is a recorded voice message (`Outgoing::voice`).
    pub voice: bool,
    /// Voice message length in seconds, when `voice` is true.
    pub duration_secs: Option<u32>,
}

fn audio_format_of(mime: &str) -> u64 {
    match mime {
        "audio/aac" => 15,
        "audio/mp4" => 16,
        "audio/amr" => 17,
        "audio/mpeg" => 18,
        "audio/ogg" | "audio/opus" => 20,
        _ => 16,
    }
}

fn send_media_request(
    conversation: &str,
    caption: &str,
    tmp: &str,
    mine: &str,
    parts: &[MediaPart<'_>],
    reply: Option<&str>,
    sim: Option<&str>,
) -> Vec<u8> {
    let mut payload = Buf::new();
    payload.text(1, tmp);
    payload.text(7, conversation);
    payload.text(9, mine);
    for part in parts {
        let mut media = Buf::new();
        if part.voice {
            media.int(1, audio_format_of(part.mime));
        }
        media.text(2, part.media_id);
        media.text(4, part.name);
        media.int(5, part.size);
        media.bytes(11, part.key);
        media.text(14, part.mime);
        if let Some(secs) = part.duration_secs.filter(|s| *s > 0) {
            media.int(18, u64::from(secs));
        }
        let mut media_info = Buf::new();
        media_info.msg(3, &media.0);
        payload.msg(10, &media_info.0);
    }
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
    if let Some(sim_id) = sim.filter(|id| !id.is_empty()) {
        let mut sim_payload = Buf::new();
        sim_payload.text(1, sim_id);
        req.msg(4, &sim_payload.0);
    }
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
    if raw > 1_000_000_000_000_000 {
        raw / 1_000_000
    } else if raw > 1_000_000_000_000 {
        raw / 1_000
    } else {
        raw
    }
}

/// One SIM card reported by the phone in `SettingsUpdate` (field 5 of `UpdateEvents`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SimCard {
    /// Subscription / participant id carried in `SIMPayload`.
    pub id: String,
    /// 1-based slot number (e.g. 1, 2).
    pub slot: u32,
    /// Carrier display name (e.g. "T-Mobile", "Work").
    pub carrier: String,
    /// Phone number for this SIM, when reported by the phone.
    pub number: String,
    /// SIM colour (`0xRRGGBB`), when reported by the phone.
    pub color: Option<u32>,
    /// Whether RCS is enabled on this SIM.
    pub rcs: bool,
}

impl SimCard {
    pub fn label(&self) -> String {
        if !self.carrier.trim().is_empty() {
            self.carrier.trim().to_owned()
        } else {
            format!("SIM {}", self.slot.max(1))
        }
    }
}

/// One participant of a Messages conversation, as reported by the phone.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Participant {
    pub id: String,
    pub name: String,
    pub first_name: String,
    pub number: String,
    pub is_me: bool,
    /// Avatar colour assigned by the phone (`0xRRGGBB`), when present.
    pub color: Option<u32>,
}

/// One contact from the phone's address book (`LIST_CONTACTS`, action 6).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContactEntry {
    pub id: String,
    pub participant_id: String,
    pub name: String,
    pub number: String,
    pub formatted_number: String,
}

#[derive(Clone, Default)]
pub struct Chat {
    pub id: String,
    pub title: String,
    pub group: bool,
    pub unread: bool,
    pub mine: String,
    /// The other side of a one-to-one chat, by participant id: whose picture the row shows.
    /// Empty for a group.
    pub peer: String,
    /// `Conversation.type`, field 22: 1 SMS, 2 RCS, 3 RBM (business), 0 when the phone did not say.
    pub carrier: u64,
    /// Unix seconds of the latest message, from field 5. 0 when the phone did not say.
    /// The inbox cursor uses it when the response has no cursor of its own.
    pub updated: i64,
    /// A tombstone said the phones encrypt the chat. A later conversation update does not
    /// clear it: the conversation message has no such field.
    pub phones: bool,
    /// `Conversation.status`, field 12. 0 when the phone did not say, which keeps the status
    /// already stored. 3 is DELETED.
    pub status: u64,
    /// `Conversation.readOnly`, field 13: the phone says this sender cannot receive replies.
    pub read_only: bool,
    /// Verified business sender (RBM or verified business flag).
    pub verified: bool,
    /// Participants reported on this conversation (id, name, number, is_me).
    pub participants: Vec<Participant>,
}

/// True when a sender identifier is alphanumeric (e.g. "BANKOTP", "FedEx") rather than a phone
/// number or numeric short code. Alphanumeric senders cannot receive SMS replies; numeric short
/// codes often accept "STOP" and stay replyable unless the phone marks the chat read-only.
pub fn is_alphanumeric_sender(number: &str) -> bool {
    let trimmed = number.trim();
    !trimmed.is_empty() && trimmed.chars().any(|c| c.is_ascii_alphabetic())
}

/// Standard emergency numbers that must never be texted or called from tinyclient.
pub fn is_emergency_number(number: &str) -> bool {
    let digits: String = number.chars().filter(char::is_ascii_digit).collect();
    matches!(
        digits.as_str(),
        "911" | "112" | "999" | "000" | "111" | "110" | "119" | "118" | "100" | "101" | "102"
            | "108" | "190" | "192" | "193" | "995" | "997" | "998"
    )
}

/// Normalises a user-typed phone number to E.164 using `default_cc` (e.g. `"+1"`).
/// Refuses alphanumeric senders, emergency numbers, and too-short/too-long inputs.
pub fn normalize_e164(input: &str, default_cc: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() || is_alphanumeric_sender(trimmed) || is_emergency_number(trimmed) {
        return None;
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, '+' | '-' | '(' | ')' | ' ' | '.'))
    {
        return None;
    }
    let has_plus = trimmed.starts_with('+');
    let digits: String = trimmed.chars().filter(char::is_ascii_digit).collect();
    if digits.len() < 7 || digits.len() > 15 {
        return None;
    }
    if has_plus {
        return Some(format!("+{digits}"));
    }
    if let Some(rest) = digits.strip_prefix("00") {
        if (7..=15).contains(&rest.len()) {
            return Some(format!("+{rest}"));
        }
    }
    let cc: String = default_cc.chars().filter(char::is_ascii_digit).collect();
    let cc = if cc.is_empty() { "1" } else { cc.as_str() };
    if cc == "1" {
        if digits.len() == 10 {
            return Some(format!("+1{digits}"));
        }
        if digits.len() == 11 && digits.starts_with('1') {
            return Some(format!("+{digits}"));
        }
    }
    if digits.starts_with(cc) && digits.len() >= 10 {
        Some(format!("+{digits}"))
    } else {
        Some(format!("+{cc}{digits}"))
    }
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

/// One emoji or custom (Photomoji) reaction on a message, and who put it there.
#[derive(Clone, Default)]
pub struct Mark {
    pub emoji: String,
    pub who: Vec<String>,
    /// Optional media reference for Photomoji / custom image reactions.
    pub media: Option<Piece>,
}

/// A shared geographic location decoded from `MessageInfo.location` (field 5).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Location {
    pub lat: f64,
    pub lng: f64,
    pub label: String,
    pub address: String,
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
    pub location: Option<Location>,
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
    /// True when the phone marked this RCS message as edited.
    pub edited: bool,
    /// Unix seconds when the message was edited, if reported by the phone.
    pub edited_at: Option<i64>,
    /// `Message.isStarred`, field 24.
    pub starred: bool,
    /// True when the phone reports this message as scheduled (`status == 16`).
    pub scheduled: bool,
    /// SIM subscription / participant id reported on this message, when present.
    pub sim: Option<String>,
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

/// A tombstone a person would notice. `MessageStatusType` 200–299. The rest of that range
/// (education, satellite, toolstones) stays hidden. 300 is a delete, not one of these.
/// Encryption tombstones are handled first, by [`between_phones`], and are not drawn as these lines.
pub fn tombstone(status: u64) -> Option<&'static str> {
    Some(match status {
        200 | 225 | 226 => "Someone joined",
        201 | 227 => "Someone left",
        202 | 228 => "You left",
        203 | 204 | 229 => "Group created",
        205 => "Broadcast created",
        208 => "Switched to group MMS",
        211 | 217 => "Group renamed",
        218 => "Group name cleared",
        220 | 230 => "You were removed",
        221 | 231 => "Someone was removed",
        244 => "Missed video call",
        _ => return None,
    })
}

/// The lab sets `TINYCLIENT_PROVE=messages-delete`. A build from before this ignores that
/// and keeps serving, so the same driver fails there.
pub(super) fn prove_messages_delete() -> Result<(), String> {
    let message = delete_message_request("m1");
    if field_text(&fields(&message), 2) != "m1" {
        return Err("the delete-message request did not name the message".into());
    }
    let chat = delete_conversation_request("c1");
    let chat_fields = fields(&chat);
    if field_int(&chat_fields, 2) != 1 || field_text(&chat_fields, 3) != "c1" {
        return Err("the delete-chat request was not a delete of that chat".into());
    }
    if field_text(&fields(&field_bytes(&chat_fields, 6)), 1) != "c1" {
        return Err("the delete-chat data did not name the chat".into());
    }
    let mut status = Buf::new();
    status.int(2, 300);
    let mut gone = Buf::new();
    gone.text(1, "m-gone");
    gone.text(7, "c1");
    gone.msg(4, &status.0);
    let mut wrap = Buf::new();
    wrap.msg(1, &gone.0);
    let notes = decode_messages(&wrap.0);
    if notes.len() != 1 || notes[0].status != 300 || notes[0].id != "m-gone" {
        return Err("a deleted message was not reported".into());
    }
    if tombstone(300).is_some() {
        return Err("a delete was shown as a system line".into());
    }
    if tombstone(200) != Some("Someone joined") {
        return Err("someone joining was dropped".into());
    }
    if tombstone(251).is_some() {
        return Err("a toolstone was shown".into());
    }
    let mut conv = Buf::new();
    conv.text(1, "c1");
    conv.text(2, "Club");
    conv.int(12, 3);
    let mut list = Buf::new();
    list.msg(2, &conv.0);
    let chats = decode_chats(&list.0);
    if chats.len() != 1 || chats[0].status != 3 {
        return Err("a chat deleted on the phone was kept".into());
    }
    Ok(())
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

/// Google Messages lets a person edit an RCS message for 15 minutes after sending.
pub const EDIT_WINDOW_SECS: u32 = 15 * 60;

/// Editing is only allowed in an RCS conversation (`Conversation.type` 2), never SMS/MMS.
pub fn allows_edit(carrier: u64) -> bool {
    carrier == 2
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
    crate::adapters::net::apply_proxy(ureq::Agent::config_builder())
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

    /// Asks the phone to drop this computer. An empty attempt id has nothing to revoke.
    pub fn unpair(&self) -> Result<(), String> {
        let attempt = self
            .live
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .saved
            .pairing_id
            .clone();
        if attempt.is_empty() {
            return Ok(());
        }
        let _ = self.rpc(46, &unpair_request(&attempt), 2)?;
        Ok(())
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
            let gaia_pairing = clear_action(action);
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
        // Sign-out waits five seconds and then forgets the account. Unpair has to answer
        // inside that, or the request is simply best effort.
        let deadline = Instant::now()
            + if action == 45 {
                Duration::from_secs(180)
            } else if action == 46 {
                Duration::from_secs(4)
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
        // The first list after pairing is the annotation. Archive and spam use the ordinary call.
        let first = self.rpc(1, &list_request(CHAT_PAGE, 1, None), 16)?;
        let chats = decode_chats(&first);
        self.note_inbox(&first, &chats);
        self.apply_chats(chats);
        for folder in [2u64, 5] {
            match self.rpc(1, &list_request(CHAT_PAGE, folder, None), 2) {
                Ok(page) => self.apply_chats(decode_chats(&page)),
                Err(why) if why == PHONE_QUIET => return Err(why),
                Err(why) => tracing::warn!("chatd: Messages: {why}"),
            }
        }
        Ok(())
    }

    /// Block the conversation. The published request is `BLOCK` with the conversation id, and
    /// the same id again in the delete-shaped data the phone expects. Reporting spam is a
    /// different action and is not sent from here.
    pub fn block(&self, conversation: &str) -> Result<(), String> {
        let bytes = self.rpc(15, &block_request(conversation), 2)?;
        refused(&bytes)?;
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
        let bytes = self.rpc(1, &list_request(CHAT_PAGE, 1, Some(&cursor)), 2)?;
        let chats = decode_chats(&bytes);
        let next = decode_cursor(&bytes).or_else(|| oldest_cursor(&chats));
        let moved = next.as_ref().is_some_and(|next| next.id != cursor.id || next.millis != cursor.millis);
        if !moved {
            self.finish_inbox();
        } else {
            self.note_inbox(&bytes, &chats);
        }
        self.apply_chats(chats.clone());
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

    fn apply_chats(&self, chats: Vec<Chat>) {
        let sent = keep_chats(&self.live, chats);
        if !sent.is_empty() {
            let _ = self
                .events
                .send(crate::adapters::messages::Event::Chats(sent));
        }
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
        sim: Option<&str>,
        noted: &mut dyn FnMut(&str),
    ) -> Result<String, String> {
        let mine = self.mine_of(conversation);
        let tmp = uuid();
        noted(&tmp);
        let bytes = self.rpc(3, &send_request(conversation, text, &tmp, &mine, reply, sim), 2)?;
        refused(&bytes)?;
        Ok(tmp)
    }

    /// Asks the phone to send again the copy it already has (`RESEND_MESSAGE`, action 25).
    pub fn resend(&self, message: &str) -> Result<(), String> {
        let bytes = self.rpc(25, &resend_request(message), 2)?;
        refused(&bytes)
    }

    /// Uploads one sealed file. A large photo may leave as an MMS copy; this does not promise
    /// an RCS high-quality send.
    pub fn upload_bytes(&self, packed: &[u8], mime: &str) -> Result<String, String> {
        self.upload(packed, mime)
    }

    /// Asks the phone to send every part as one message. `parts` are already uploaded.
    pub fn send_media(
        &self,
        conversation: &str,
        caption: &str,
        parts: &[MediaPart<'_>],
        reply: Option<&str>,
        sim: Option<&str>,
        noted: &mut dyn FnMut(&str),
    ) -> Result<String, String> {
        let mine = self.mine_of(conversation);
        let tmp = uuid();
        noted(&tmp);
        let bytes = self.rpc(
            3,
            &send_media_request(conversation, caption, &tmp, &mine, parts, reply, sim),
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

    /// Lists the phone's contacts (`LIST_CONTACTS`, action 6).
    pub fn contacts(&self) -> Result<Vec<ContactEntry>, String> {
        let bytes = self.rpc(6, &list_contacts_request(), 2)?;
        Ok(decode_contacts(&bytes))
    }

    /// Gets or creates a 1:1 or group conversation (`GET_OR_CREATE_CONVERSATION`, action 11).
    pub fn get_or_create_conversation(
        &self,
        numbers: &[String],
        group_name: Option<&str>,
    ) -> Result<Chat, String> {
        let bytes = self.rpc(
            11,
            &get_or_create_conversation_request(numbers, group_name),
            2,
        )?;
        refused(&bytes)?;
        let chats = decode_chats(&bytes);
        let chat = chats
            .into_iter()
            .next()
            .ok_or_else(|| "Messages didn't return a conversation.".to_owned())?;
        self.apply_chats(vec![chat.clone()]);
        Ok(chat)
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

    pub fn delete_message(&self, message: &str) -> Result<(), String> {
        let bytes = self.rpc(23, &delete_message_request(message), 2)?;
        if field_int(&fields(&bytes), 2) != 1 {
            return Err("Messages didn't delete that. Keep the phone online.".into());
        }
        Ok(())
    }

    pub fn edit_message(&self, conversation: &str, message: &str, text: &str) -> Result<(), String> {
        let bytes = self.rpc(47, &edit_message_request(conversation, message, text), 2)?;
        refused(&bytes)
    }

    pub fn star_message(&self, conversation: &str, message: &str, starred: bool) -> Result<(), String> {
        let bytes = self.rpc(48, &star_message_request(conversation, message, starred), 2)?;
        refused(&bytes)
    }

    pub fn delete_conversation(&self, conversation: &str) -> Result<(), String> {
        let bytes = self.rpc(15, &delete_conversation_request(conversation), 2)?;
        refused(&bytes)
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

/// A later list or update adds to the chats already held. A status of 0 means this message
/// did not say, so the one already stored stays. Replacing the whole list would drop the inbox
/// when the archive page arrives, and would put an archived chat back when an update omits it.
fn keep_chats(live: &Mutex<Live>, incoming: Vec<Chat>) -> Vec<Chat> {
    if incoming.is_empty() {
        return Vec::new();
    }
    let mut held = live.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut sent = Vec::with_capacity(incoming.len());
    for mut chat in incoming {
        if let Some(slot) = held.chats.iter_mut().find(|had| had.id == chat.id) {
            if chat.status == 0 {
                chat.status = slot.status;
            }
            *slot = chat.clone();
        } else {
            held.chats.push(chat.clone());
        }
        sent.push(chat);
    }
    sent
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
            let sent = keep_chats(live, decode_chats(&plain));
            if !sent.is_empty() {
                let _ = events.send(crate::adapters::messages::Event::Chats(sent));
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

/// Messages, then the conversation row, then typing. A read flag in the same batch must not
/// land before the messages it already includes, or the count flickers.
pub(super) fn update_field_order() -> [u64; 3] {
    [3, 2, 4]
}

/// Field 6 is a bool, not a count. True leaves whatever the messages added. False means the
/// phone has read the chat, so the badge is cleared. A direct or group row cannot show a dot.
pub(super) fn flag_count(unread: bool) -> Option<u32> {
    if unread { None } else { Some(0) }
}

fn on_updates(bytes: &[u8], live: &Mutex<Live>, events: &Sender<crate::adapters::messages::Event>) {
    let list = fields(bytes);
    for number in update_field_order() {
        match number {
            3 => {
                if let Some(messages) = field(&list, 3) {
                    for note in decode_messages(&messages.bytes) {
                        let _ = events.send(crate::adapters::messages::Event::Message(note));
                    }
                }
            }
            2 => {
                if let Some(conversations) = field(&list, 2) {
                    let sent = keep_chats(live, decode_chats(&conversations.bytes));
                    if !sent.is_empty() {
                        let _ = events.send(crate::adapters::messages::Event::Chats(sent));
                    }
                }
            }
            4 => {
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
            _ => {}
        }
    }
    if let Some(settings) = field(&list, 5) {
        let sims = decode_settings(&settings.bytes);
        if !sims.is_empty() {
            let _ = events.send(crate::adapters::messages::Event::Settings(sims));
        }
    }
}

/// Decodes `SettingsUpdate` (field 5 of `UpdateEvents`) into the phone's SIM card list.
/// Supports both Bugle's nested `SIMCard { 1: { 1: id, 2: slot }, 2: color, 3: { 1: carrier, 2: number, 5: rcs }, 4: rcs }`
/// and flat `SIMCard { 1: id, 2: slot, 3: carrier, 4: number, 5: color, 6: rcs }` layouts.
fn decode_settings(bytes: &[u8]) -> Vec<SimCard> {
    let list = fields(bytes);
    let mut sims = Vec::new();
    for item in list.iter().filter(|f| f.number == 2 || f.number == 3) {
        if item.bytes.is_empty() {
            continue;
        }
        let card = fields(&item.bytes);
        let (id, mut slot) = if let Some(f1) = field(&card, 1) {
            if f1.varint.is_some() {
                (f1.varint.unwrap().to_string(), 0)
            } else {
                let inner = fields(&f1.bytes);
                let nested_id = field_text(&inner, 1);
                let nested_slot = field_int(&inner, 2) as u32;
                if !nested_id.is_empty() {
                    (nested_id, nested_slot)
                } else {
                    (String::from_utf8_lossy(&f1.bytes).into_owned(), 0)
                }
            }
        } else {
            (String::new(), 0)
        };
        let mut carrier = String::new();
        let mut number = String::new();
        let mut color = None;
        let mut rcs = false;

        if let Some(f2) = field(&card, 2) {
            if let Some(v) = f2.varint {
                if v <= 8 && slot == 0 {
                    slot = v as u32;
                } else if v > 8 {
                    color = Some((v as u32) & 0x00FF_FFFF);
                }
            } else if !f2.bytes.is_empty() {
                color = parse_hex_color(&String::from_utf8_lossy(&f2.bytes));
            }
        }
        if let Some(f3) = field(&card, 3).filter(|f| !f.bytes.is_empty()) {
            let sub = fields(&f3.bytes);
            let sub_carrier = field_text(&sub, 1);
            let sub_number = field_text(&sub, 2);
            if !sub_carrier.is_empty() || !sub_number.is_empty() {
                carrier = sub_carrier;
                number = sub_number;
                if slot == 0 {
                    slot = field_int(&sub, 3) as u32;
                }
                if color.is_none() {
                    let c = field_int(&sub, 4);
                    if c != 0 {
                        color = Some((c as u32) & 0x00FF_FFFF);
                    }
                }
                if field_int(&sub, 5) != 0 {
                    rcs = true;
                }
            } else {
                carrier = String::from_utf8_lossy(&f3.bytes).into_owned();
            }
        }
        if number.is_empty() {
            if let Some(f4) = field(&card, 4) {
                if let Some(v) = f4.varint {
                    rcs |= v != 0;
                } else if !f4.bytes.is_empty() {
                    let text = String::from_utf8_lossy(&f4.bytes).into_owned();
                    if text.chars().any(|c| c.is_ascii_digit()) {
                        number = text;
                    } else {
                        let sub = fields(&f4.bytes);
                        rcs |= field_int(&sub, 1) != 0;
                    }
                }
            }
        } else if let Some(f4) = field(&card, 4) {
            if let Some(v) = f4.varint {
                rcs |= v != 0;
            }
        }
        if color.is_none() {
            if let Some(f5) = field(&card, 5) {
                if let Some(v) = f5.varint {
                    if v != 0 {
                        color = Some((v as u32) & 0x00FF_FFFF);
                    }
                } else if !f5.bytes.is_empty() {
                    color = parse_hex_color(&String::from_utf8_lossy(&f5.bytes));
                }
            }
        }
        if field_int(&card, 6) != 0 {
            rcs = true;
        }
        if !id.is_empty() || !carrier.is_empty() || !number.is_empty() {
            if slot == 0 {
                slot = (sims.len() + 1) as u32;
            }
            let id = if id.is_empty() { format!("sim-{slot}") } else { id };
            sims.push(SimCard {
                id,
                slot,
                carrier,
                number,
                color,
                rcs,
            });
        }
    }
    sims
}

/// The lab sets `TINYCLIENT_PROVE=messages-unread`. A build from before this ignores that
/// and keeps serving, so the same driver fails there.
pub(super) fn prove_messages_unread() -> Result<(), String> {
    if update_field_order() != [3, 2, 4] {
        return Err("a batch applies the conversation before its messages".into());
    }
    if flag_count(true).is_some() {
        return Err("an unread flag was published as a count".into());
    }
    if flag_count(false) != Some(0) {
        return Err("a chat read on the phone was not cleared".into());
    }
    let mut bare = Buf::new();
    bare.text(1, "club");
    bare.text(2, "Club");
    bare.int(6, 1);
    bare.int(10, 1);
    let mut list = Buf::new();
    list.msg(2, &bare.0);
    let flagged = decode_chats(&list.0);
    if flagged.len() != 1 || !flagged[0].unread {
        return Err("the unread flag was not read".into());
    }
    let mut quiet = 0u32;
    if let Some(set) = flag_count(flagged[0].unread) {
        quiet = set;
    }
    if quiet != 0 {
        return Err("a flag with no messages became a count".into());
    }
    let mut notes = Buf::new();
    for (id, words) in [("m1", "a"), ("m2", "bb"), ("m3", "ccc")] {
        let mut message = Buf::new();
        message.text(1, id);
        message.text(7, "club");
        let mut body = Buf::new();
        body.text(1, words);
        let mut info = Buf::new();
        info.msg(2, &body.0);
        message.msg(10, &info.0);
        notes.msg(1, &message.0);
    }
    let arrived = decode_messages(&notes.0);
    if arrived.len() != 3 {
        return Err(format!("{} messages decoded, not 3", arrived.len()));
    }
    let mut count = arrived.len() as u32;
    if let Some(set) = flag_count(true) {
        count = set;
    }
    if count != 3 {
        return Err(format!("three texts counted as {count}"));
    }
    let mut read = Buf::new();
    read.text(1, "club");
    read.text(2, "Club");
    read.int(6, 0);
    let mut read_list = Buf::new();
    read_list.msg(2, &read.0);
    let cleared = decode_chats(&read_list.0);
    if cleared.first().is_some_and(|chat| chat.unread) {
        return Err("a read chat still looked unread".into());
    }
    if let Some(set) = flag_count(false) {
        count = set;
    }
    if count != 0 {
        return Err("a chat read on the phone kept its count".into());
    }
    Ok(())
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
        let status = field_int(&conv, 12);
        let read_only = field_int(&conv, 13) != 0;
        let mut verified = carrier == 3 || field_int(&conv, 26) != 0;
        let mut mine = field_text(&conv, 11);
        let mut peer = String::new();
        let mut participants = Vec::new();
        for person in conv.iter().filter(|field| field.number == 20) {
            let participant = fields(&person.bytes);
            let info = fields(&field_bytes(&participant, 1));
            let pid = field_text(&info, 3);
            if field_int(&participant, 18) != 0 || field_int(&info, 1) == 3 {
                verified = true;
            }
            let first_name = field_text(&participant, 2);
            let name = {
                let full = field_text(&participant, 3);
                if full.is_empty() {
                    first_name.clone()
                } else {
                    full
                }
            };
            let number = {
                let raw = field_text(&participant, 15);
                if raw.is_empty() {
                    field_text(&info, 2)
                } else {
                    raw
                }
            };
            let is_me = field_int(&participant, 6) != 0;
            let color = parse_hex_color(&field_text(&participant, 22));
            if !is_me && !group && peer.is_empty() {
                peer = pid.clone();
            }
            if is_me && !pid.is_empty() {
                mine = pid.clone();
            } else if title.is_empty() && !name.is_empty() && !group {
                title = name.clone();
            } else if title.is_empty() && !group && !number.is_empty() {
                title = number.clone();
            }
            if !pid.is_empty() || !name.is_empty() || !number.is_empty() {
                participants.push(Participant {
                    id: pid,
                    name,
                    first_name,
                    number,
                    is_me,
                    color,
                });
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
            status,
            read_only,
            verified,
            participants,
        });
    }
    chats
}

fn parse_hex_color(raw: &str) -> Option<u32> {
    let hex = raw.trim().trim_start_matches('#');
    if hex.len() == 6 {
        u32::from_str_radix(hex, 16).ok()
    } else if hex.len() == 8 {
        // ARGB -> RGB
        u32::from_str_radix(&hex[2..], 16).ok()
    } else {
        None
    }
}

fn list_contacts_request() -> Vec<u8> {
    let mut req = Buf::new();
    req.int(1, 1);
    req.int(2, 1);
    req.int(3, 500);
    req.0
}

fn get_or_create_conversation_request(numbers: &[String], group_name: Option<&str>) -> Vec<u8> {
    let mut req = Buf::new();
    for number in numbers {
        let mut entry = Buf::new();
        entry.text(1, number);
        req.msg(2, &entry.0);
    }
    if let Some(name) = group_name.filter(|n| !n.trim().is_empty()) {
        req.text(3, name.trim());
    }
    if numbers.len() > 1 {
        req.int(4, 1);
    }
    req.0
}

fn decode_contacts(bytes: &[u8]) -> Vec<ContactEntry> {
    let list = fields(bytes);
    let mut out = Vec::new();
    for item in list.iter().filter(|f| f.number == 2) {
        let entry = fields(&item.bytes);
        let participant_id = field_text(&entry, 1);
        let name = field_text(&entry, 2);
        let num = fields(&field_bytes(&entry, 3));
        let number = field_text(&num, 1);
        let formatted_number = {
            let f = field_text(&num, 2);
            if f.is_empty() { number.clone() } else { f }
        };
        let id = {
            let cid = field_text(&entry, 7);
            if cid.is_empty() { participant_id.clone() } else { cid }
        };
        if !name.is_empty() || !number.is_empty() {
            out.push(ContactEntry {
                id,
                participant_id,
                name,
                number,
                formatted_number,
            });
        }
    }
    out
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
        let kind = field_int(&data, 2);
        let mut emoji = emoji_of(&field_text(&data, 1), kind);
        let media = field(&data, 3)
            .or_else(|| field(&parts, 3))
            .filter(|f| !f.bytes.is_empty())
            .and_then(|f| decode_piece(&f.bytes, ""));
        if emoji.is_empty() {
            // Photomoji or another custom/unknown reaction: never drop it silently.
            if media.is_some() {
                emoji = ":photomoji:".to_owned();
            } else if kind != 0 || !data.is_empty() {
                emoji = "🖼️".to_owned();
            } else {
                continue;
            }
        }
        let who = field_texts(&parts, 2);
        if let Some(have) = marks
            .iter_mut()
            .find(|mark| mark.emoji == emoji && mark.media.is_none() && media.is_none())
        {
            for id in who {
                if !have.who.contains(&id) {
                    have.who.push(id);
                }
            }
        } else {
            marks.push(Mark { emoji, who, media });
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
        // 300 is MESSAGE_DELETED. A known tombstone is a system line. The rest of 200–299
        // stay hidden: they are toolstones, not something to put in the transcription.
        // Encryption changes were already taken above.
        if status == 300 {
            notes.push(Note {
                id,
                conversation,
                status,
                ..Note::default()
            });
            continue;
        }
        if let Some(words) = tombstone(status) {
            notes.push(Note {
                id,
                conversation,
                sent: unix_seconds(field_int(&msg, 5) as i64),
                text: words.to_owned(),
                status,
                ..Note::default()
            });
            continue;
        }
        if (200..300).contains(&status) {
            continue;
        }
        let wire = field_int(&msg, 11);
        let mut sent = unix_seconds(field_int(&msg, 5) as i64);
        let scheduled = status == 16;
        if scheduled && sent == 0 {
            sent = unix_seconds(field_int(&msg, 26) as i64);
        }
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
        let mut location = None;
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
            if let Some(loc_field) = field(&parts, 5) {
                if let Some(loc) = decode_location(&loc_field.bytes) {
                    location = Some(loc);
                } else if location.is_none() && pieces.is_empty() {
                    file = true;
                    pieces.push(Piece {
                        name: "Location".into(),
                        mime: "text/plain".into(),
                        ..Piece::default()
                    });
                }
            }
        }
        let reactions = decode_reactions(&msg);
        let reply = field(&msg, 21)
            .map(|field| field_text(&fields(&field.bytes), 1))
            .unwrap_or_default();
        let (edited, edited_at) = decode_edit_status(&msg);
        let starred = field_int(&msg, 24) != 0;
        let sim = field(&msg, 29)
            .or_else(|| field(&msg, 17))
            .and_then(|f| {
                if let Some(v) = f.varint {
                    (v != 0).then(|| v.to_string())
                } else if !f.bytes.is_empty() {
                    let s = String::from_utf8_lossy(&f.bytes).into_owned();
                    (!s.is_empty()).then_some(s)
                } else {
                    None
                }
            });
        if text.is_empty()
            && file
            && location.is_none()
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
            location,
            reactions,
            status,
            tmp,
            detail,
            wire,
            phones: None,
            reply,
            edited,
            edited_at,
            starred,
            scheduled,
            sim,
        });
    }
    notes
}

/// Decodes a Bugle `Location` part (`MessageInfo` field 5).
/// Supports both flat coordinates (`1: lat, 2: lng, 3: label, 4: address`) and nested
/// `Coordinates` submessages (`1: name, 2: address, 3: { 1: lat, 2: lng }` or
/// `1: { 1: lat, 2: lng }, 2: label, 3: address`).
fn decode_location(bytes: &[u8]) -> Option<Location> {
    if bytes.is_empty() {
        return None;
    }
    let list = fields(bytes);
    let valid = |lat: f64, lng: f64| {
        lat.is_finite()
            && lng.is_finite()
            && (-90.0..=90.0).contains(&lat)
            && (-180.0..=180.0).contains(&lng)
    };
    // 1. Flat layout: field 1 = lat, field 2 = lng, field 3 = label, field 4 = address.
    if let (Some(lat), Some(lng)) = (field_double(&list, 1), field_double(&list, 2)) {
        if valid(lat, lng) {
            return Some(Location {
                lat,
                lng,
                label: field_text(&list, 3),
                address: field_text(&list, 4),
            });
        }
    }
    // 2. Nested Coordinates in field 3: field 1 = label, field 2 = address, field 3 = { 1: lat, 2: lng }.
    if let Some(coords_field) = field(&list, 3).filter(|f| !f.bytes.is_empty()) {
        let coords = fields(&coords_field.bytes);
        if let (Some(lat), Some(lng)) = (field_double(&coords, 1), field_double(&coords, 2)) {
            if valid(lat, lng) {
                return Some(Location {
                    lat,
                    lng,
                    label: field_text(&list, 1),
                    address: field_text(&list, 2),
                });
            }
        }
    }
    // 3. Nested Coordinates in field 1: field 1 = { 1: lat, 2: lng }, field 2 = label, field 3 = address.
    if let Some(coords_field) = field(&list, 1).filter(|f| !f.bytes.is_empty()) {
        let coords = fields(&coords_field.bytes);
        if let (Some(lat), Some(lng)) = (field_double(&coords, 1), field_double(&coords, 2)) {
            if valid(lat, lng) {
                return Some(Location {
                    lat,
                    lng,
                    label: field_text(&list, 2),
                    address: field_text(&list, 3),
                });
            }
        }
    }
    None
}

/// Decodes RCS edit metadata from a Bugle `Message`:
/// - field 27 as submessage (edit status with timestamp in field 1 or 2) or varint != 0
/// - field 28 as boolean `isEdited`
fn decode_edit_status(msg: &[Field]) -> (bool, Option<i64>) {
    if let Some(f) = field(msg, 27) {
        if let Some(v) = f.varint {
            if v != 0 {
                return (true, None);
            }
        } else if !f.bytes.is_empty() {
            let inner = fields(&f.bytes);
            let ts = field_int(&inner, 2).max(field_int(&inner, 1)) as i64;
            let at = if ts > 1_000_000_000_000_000 {
                Some(ts / 1_000_000)
            } else if ts > 1_000_000_000_000 {
                Some(ts / 1_000)
            } else if ts > 0 {
                Some(ts)
            } else {
                None
            };
            return (true, at);
        }
    }
    if field_int(msg, 28) != 0 {
        return (true, None);
    }
    (false, None)
}

fn edit_message_request(conversation: &str, message: &str, text: &str) -> Vec<u8> {
    let mut content = Buf::new();
    content.text(1, text);
    let mut info = Buf::new();
    info.msg(2, &content.0);
    let mut req = Buf::new();
    req.text(1, conversation);
    req.text(2, message);
    req.msg(3, &info.0);
    req.0
}

fn star_message_request(conversation: &str, message: &str, starred: bool) -> Vec<u8> {
    let mut req = Buf::new();
    req.text(1, conversation);
    req.text(2, message);
    req.int(3, u64::from(starred));
    req.0
}

/// The lab sets `TINYCLIENT_PROVE=messages-reply`. A build from before this ignores that
/// and keeps serving, so the same driver fails there.
pub(super) fn prove_message_reply() -> Result<(), String> {
    if allows_reply(1) || allows_reply(0) || !allows_reply(2) {
        return Err("a reply was allowed outside an RCS chat".into());
    }
    let asked = fields(&send_request("rcs-chat", "On my way", "tmp-1", "me", Some("m-orig"), None));
    let quoted = fields(&field_bytes(&asked, 8));
    if field_text(&quoted, 1) != "m-orig" {
        return Err("the send dropped the message it answers".into());
    }
    let plain = fields(&send_request("sms-chat", "On my way", "tmp-2", "me", None, None));
    if field(&plain, 8).is_some() {
        return Err("a plain send carried a reply".into());
    }
    let media = fields(&send_media_request(
        "rcs-chat",
        "",
        "tmp-3",
        "me",
        &[MediaPart {
            media_id: "media-1",
            key: &[1; 32],
            name: "a.png",
            mime: "image/png",
            size: 4,
            voice: false,
            duration_secs: None,
        }],
        Some("m-orig"),
        None,
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
    let inbox = fields(&list_request(CHAT_PAGE, 1, Some(&cursor)));
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

/// Three files and a caption are one send payload, and a file longer than one chunk seals
/// without holding the plaintext. Errors from this check do not name the scratch file.
pub(super) fn prove_several_files() -> Result<(), String> {
    let keys = [[1u8; 32], [2u8; 32], [3u8; 32]];
    let parts = [
        MediaPart {
            media_id: "m1",
            key: &keys[0],
            name: "one.txt",
            mime: "application/octet-stream",
            size: 3,
            voice: false,
            duration_secs: None,
        },
        MediaPart {
            media_id: "m2",
            key: &keys[1],
            name: "two.txt",
            mime: "application/octet-stream",
            size: 4,
            voice: false,
            duration_secs: None,
        },
        MediaPart {
            media_id: "m3",
            key: &keys[2],
            name: "three.txt",
            mime: "application/octet-stream",
            size: 5,
            voice: false,
            duration_secs: None,
        },
    ];
    let bytes = send_media_request("phone-grace", "picnic", "tmp", "phone-ada", &parts, None, None);
    let payload = fields(&field_bytes(&fields(&bytes), 3));
    let mut media = 0;
    let mut caption = 0;
    let mut names = Vec::new();
    for info in payload.iter().filter(|field| field.number == 10) {
        let inner = fields(&info.bytes);
        if field(&inner, 3).is_some() {
            media += 1;
            names.push(field_text(&fields(&field_bytes(&inner, 3)), 4));
        }
        if let Some(content) = field(&inner, 2) {
            caption += 1;
            if field_text(&fields(&content.bytes), 1) != "picnic" {
                return Err("the caption was not kept with the files".into());
            }
        }
    }
    if media != 3 || caption != 1 || names != ["one.txt", "two.txt", "three.txt"] {
        return Err(format!("a message of three files encoded {media} files and {caption} captions"));
    }
    let plain: Vec<u8> = (0..super::media::CHUNK as u32 + 20).map(|n| (n % 256) as u8).collect();
    let scratch = Scratch(std::env::temp_dir().join(format!("messages-several-{}", std::process::id())));
    let fail = |_| "Messages could not seal that file.".to_owned();
    std::fs::create_dir_all(&scratch.0).map_err(fail)?;
    let path = scratch.0.join("photo.bin");
    std::fs::write(&path, &plain).map_err(fail)?;
    let mut file = std::fs::File::open(&path).map_err(fail)?;
    let mut packer = super::media::Packer::open(&[9u8; 32])?;
    let mut rest = plain.len();
    while rest > 0 {
        let want = rest.min(super::media::CHUNK);
        let mut buf = vec![0u8; want];
        std::io::Read::read_exact(&mut file, &mut buf).map_err(fail)?;
        packer.push(&buf)?;
        rest -= want;
    }
    let packed = packer.finish()?;
    if super::media::unpack(&[9u8; 32], &packed)? != plain {
        return Err("a sealed file did not open to the same bytes".into());
    }
    Ok(())
}

struct Scratch(std::path::PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The lab sets `TINYCLIENT_PROVE=messages-sign-out`. A build from before this ignores that
/// and keeps serving, so the same driver fails there.
pub(super) fn prove_sign_out() -> Result<(), String> {
    let list = fields(&unpair_request("attempt-1"));
    if field_text(&list, 1) != "attempt-1" {
        return Err("the unpair request did not name the pairing attempt".into());
    }
    if !clear_action(46) {
        return Err("unpair would be sent as an encrypted message".into());
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
    fn an_archived_conversation_keeps_its_status_and_each_folder_is_asked() {
        prove_message_folders().unwrap();
        let mut conv = Buf::new();
        conv.text(1, "c-archive");
        conv.text(2, "Old");
        conv.int(12, 2);
        let mut list = Buf::new();
        list.msg(2, &conv.0);
        let chats = decode_chats(&list.0);
        assert_eq!(chats.len(), 1);
        assert_eq!(chats[0].status, 2);
        assert_eq!(place(2), Place::Archive);
    }

    #[test]
    fn a_picture_a_caption_and_a_reaction_decode_and_a_join_is_a_line() {
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
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0].text, "look");
        assert_eq!(notes[1].status, 200);
        assert_eq!(notes[1].text, "Someone joined");
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

    #[test]
    fn a_deleted_message_and_a_deleted_chat_are_read_from_the_published_fields() {
        prove_messages_delete().unwrap();
    }

    #[test]
    fn an_unread_flag_is_not_a_count_and_a_read_chat_clears() {
        prove_messages_unread().unwrap();
    }

    #[test]
    fn an_unpair_names_the_pairing_attempt_in_the_clear() {
        let bytes = unpair_request("attempt-1");
        let list = fields(&bytes);
        assert_eq!(field_text(&list, 1), "attempt-1");
        assert!(field(&list, 3).is_none());
        assert!(clear_action(46));
        assert!(clear_action(44));
        assert!(!clear_action(3));
    }

    #[test]
    fn read_only_verified_alphanumeric_and_emergency_helpers() {
        assert!(is_alphanumeric_sender("BANKOTP"));
        assert!(is_alphanumeric_sender("FedEx"));
        assert!(!is_alphanumeric_sender("89203"));
        assert!(!is_alphanumeric_sender("+15550100002"));

        assert!(is_emergency_number("911"));
        assert!(is_emergency_number("112"));
        assert!(is_emergency_number("999"));
        assert!(!is_emergency_number("+15550100911"));

        let mut conv = Buf::new();
        conv.text(1, "c-ro");
        conv.text(2, "Acme Alerts");
        conv.int(13, 1); // readOnly
        conv.int(22, 3); // RBM verified business
        let mut list = Buf::new();
        list.msg(2, &conv.0);
        let chats = decode_chats(&list.0);
        assert_eq!(chats.len(), 1);
        assert!(chats[0].read_only);
        assert!(chats[0].verified);
    }

    #[test]
    fn rcs_edit_flag_decodes_and_edit_request_encodes() {
        assert!(allows_edit(2));
        assert!(!allows_edit(1));
        assert!(!allows_edit(0));

        let mut edit_info = Buf::new();
        edit_info.int(2, 1_791_893_000_000_000);
        let mut msg = Buf::new();
        msg.text(1, "m-edit");
        msg.text(7, "c-rcs");
        msg.msg(27, &edit_info.0);
        let mut words = Buf::new();
        words.text(1, "corrected");
        let mut info = Buf::new();
        info.msg(2, &words.0);
        msg.msg(10, &info.0);
        let mut wrap = Buf::new();
        wrap.msg(1, &msg.0);
        let notes = decode_messages(&wrap.0);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].edited);
        assert_eq!(notes[0].edited_at, Some(1_791_893_000));
        assert_eq!(notes[0].text, "corrected");

        let req = fields(&edit_message_request("c-rcs", "m-edit", "updated text"));
        assert_eq!(field_text(&req, 1), "c-rcs");
        assert_eq!(field_text(&req, 2), "m-edit");
        let inner = fields(&field_bytes(&req, 3));
        assert_eq!(field_text(&fields(&field_bytes(&inner, 2)), 1), "updated text");
    }

    #[test]
    fn contacts_and_participant_color_decode() {
        assert_eq!(parse_hex_color("#4285F4"), Some(0x4285F4));
        assert_eq!(parse_hex_color("FF4285F4"), Some(0x4285F4));
        assert_eq!(parse_hex_color("bad"), None);

        let mut num = Buf::new();
        num.text(1, "+15550100004");
        num.text(2, "(555) 010-0004");
        let mut contact = Buf::new();
        contact.text(1, "participant-linnea");
        contact.text(2, "Linnea Sol");
        contact.msg(3, &num.0);
        contact.text(7, "contact-4");
        let mut resp = Buf::new();
        resp.msg(2, &contact.0);
        let decoded = decode_contacts(&resp.0);
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].participant_id, "participant-linnea");
        assert_eq!(decoded[0].name, "Linnea Sol");
        assert_eq!(decoded[0].number, "+15550100004");
        assert_eq!(decoded[0].formatted_number, "(555) 010-0004");
        assert_eq!(decoded[0].id, "contact-4");
    }

    #[test]
    fn e164_normalization_and_get_or_create_request() {
        assert_eq!(normalize_e164("(555) 010-0099", "+1").as_deref(), Some("+15550100099"));
        assert_eq!(normalize_e164("+44 20 7946 0958", "+1").as_deref(), Some("+442079460958"));
        assert_eq!(normalize_e164("1-555-010-0099", "+1").as_deref(), Some("+15550100099"));
        assert_eq!(normalize_e164("911", "+1"), None);
        assert_eq!(normalize_e164("BANKOTP", "+1"), None);
        assert_eq!(normalize_e164("123", "+1"), None);

        let single = fields(&get_or_create_conversation_request(&["+15550100099".into()], None));
        let nums: Vec<String> = single
            .iter()
            .filter(|f| f.number == 2)
            .map(|f| field_text(&fields(&f.bytes), 1))
            .collect();
        assert_eq!(nums, ["+15550100099"]);
        assert_eq!(field_int(&single, 4), 0);

        let group = fields(&get_or_create_conversation_request(
            &["+15550100002".into(), "+15550100003".into()],
            Some("Weekend"),
        ));
        assert_eq!(field_text(&group, 3), "Weekend");
        assert_eq!(field_int(&group, 4), 1);
    }

    #[test]
    fn starred_flag_decodes_and_star_request_encodes() {
        let mut msg = Buf::new();
        msg.text(1, "m-star");
        msg.text(7, "c1");
        msg.int(24, 1);
        let mut words = Buf::new();
        words.text(1, "Keep this");
        let mut info = Buf::new();
        info.msg(2, &words.0);
        msg.msg(10, &info.0);
        let mut wrap = Buf::new();
        wrap.msg(1, &msg.0);
        let notes = decode_messages(&wrap.0);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].starred);

        let req = fields(&star_message_request("c1", "m-star", true));
        assert_eq!(field_text(&req, 1), "c1");
        assert_eq!(field_text(&req, 2), "m-star");
        assert_eq!(field_int(&req, 3), 1);
    }

    #[test]
    fn location_part_decodes_flat_and_nested_coordinates_and_rejects_out_of_range() {
        // Flat layout: double lat/lng + label + address.
        let mut loc = Buf::new();
        loc.double(1, 37.7749);
        loc.double(2, -122.4194);
        loc.text(3, "Ferry Building");
        loc.text(4, "1 Ferry Building, San Francisco, CA");
        let mut info = Buf::new();
        info.msg(5, &loc.0);
        let mut msg = Buf::new();
        msg.text(1, "m-loc-1");
        msg.text(7, "c1");
        msg.msg(10, &info.0);
        let mut wrap = Buf::new();
        wrap.msg(1, &msg.0);
        let notes = decode_messages(&wrap.0);
        assert_eq!(notes.len(), 1);
        let got = notes[0].location.as_ref().expect("location decoded");
        assert!((got.lat - 37.7749).abs() < 1e-6);
        assert!((got.lng + 122.4194).abs() < 1e-6);
        assert_eq!(got.label, "Ferry Building");
        assert_eq!(got.address, "1 Ferry Building, San Francisco, CA");
        assert!(!notes[0].file, "decoded location must not be flagged as unsupported file");

        // Nested layout: label/address + Coordinates submessage in field 3.
        let mut coords = Buf::new();
        coords.double(1, 51.5074);
        coords.double(2, -0.1278);
        let mut nested = Buf::new();
        nested.text(1, "London");
        nested.text(2, "London, UK");
        nested.msg(3, &coords.0);
        let dec = decode_location(&nested.0).expect("nested location decoded");
        assert!((dec.lat - 51.5074).abs() < 1e-6);
        assert!((dec.lng + 0.1278).abs() < 1e-6);
        assert_eq!(dec.label, "London");
        assert_eq!(dec.address, "London, UK");

        // Out-of-range coordinates rejected.
        let mut bad = Buf::new();
        bad.double(1, 95.0);
        bad.double(2, 200.0);
        assert!(decode_location(&bad.0).is_none());
    }

    #[test]
    fn voice_media_part_encodes_audio_format_and_duration() {
        let key = [7u8; 32];
        let part = MediaPart {
            media_id: "voice-1",
            key: &key,
            name: "voice.m4a",
            mime: "audio/mp4",
            size: 4096,
            voice: true,
            duration_secs: Some(12),
        };
        let bytes = send_media_request("c-rcs", "", "tmp-v", "me", &[part], None, None);
        let payload = fields(&field_bytes(&fields(&bytes), 3));
        let info = payload.iter().find(|f| f.number == 10).expect("media info");
        let media = fields(&field_bytes(&fields(&info.bytes), 3));
        assert_eq!(field_int(&media, 1), 16, "AUDIO_MP4 format");
        assert_eq!(field_text(&media, 2), "voice-1");
        assert_eq!(field_text(&media, 14), "audio/mp4");
        assert_eq!(field_int(&media, 18), 12, "duration seconds");
    }

    #[test]
    fn photomoji_and_unknown_custom_reactions_are_kept_instead_of_dropped() {
        // 1. Photomoji reaction with media reference in ReactionData field 3.
        let mut media = Buf::new();
        media.text(2, "photomoji-media-1");
        media.text(4, "photomoji.png");
        media.text(14, "image/png");
        let mut data = Buf::new();
        data.int(2, 13); // EMOTIFY / Photomoji
        data.msg(3, &media.0);
        let mut entry = Buf::new();
        entry.msg(1, &data.0);
        entry.text(2, "phone-grace");

        // 2. Unknown custom reaction without media reference -> neutral placeholder.
        let mut data2 = Buf::new();
        data2.int(2, 99);
        let mut entry2 = Buf::new();
        entry2.msg(1, &data2.0);
        entry2.text(2, "phone-alan");

        let mut msg = Buf::new();
        msg.text(1, "m-photo-react");
        msg.text(7, "c1");
        msg.msg(19, &entry.0);
        msg.msg(19, &entry2.0);
        let mut wrap = Buf::new();
        wrap.msg(1, &msg.0);
        let notes = decode_messages(&wrap.0);
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].reactions.len(), 2);
        assert_eq!(notes[0].reactions[0].emoji, ":photomoji:");
        assert_eq!(
            notes[0].reactions[0].media.as_ref().map(|p| p.media_id.as_str()),
            Some("photomoji-media-1")
        );
        assert_eq!(notes[0].reactions[1].emoji, "🖼️");
        assert!(notes[0].reactions[1].media.is_none());
    }

    #[test]
    fn phone_scheduled_message_decodes_as_scheduled_pending() {
        let mut status = Buf::new();
        status.int(2, 16); // OUTGOING_SCHEDULED
        let mut words = Buf::new();
        words.text(1, "Happy birthday!");
        let mut info = Buf::new();
        info.msg(2, &words.0);
        let mut msg = Buf::new();
        msg.text(1, "m-sched-phone");
        msg.text(7, "c1");
        msg.msg(4, &status.0);
        msg.msg(10, &info.0);
        msg.int(26, 1_800_000_000_000_000);
        let mut wrap = Buf::new();
        wrap.msg(1, &msg.0);
        let notes = decode_messages(&wrap.0);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].scheduled);
        assert_eq!(notes[0].sent, 1_800_000_000);
        assert_eq!(progress(notes[0].status), Progress::Pending);
    }

    #[test]
    fn settings_update_decodes_dual_sims_and_send_carries_sim_only_when_chosen() {
        let mut sim1 = Buf::new();
        sim1.text(1, "sim-personal");
        sim1.int(2, 1);
        sim1.text(3, "T-Mobile");
        sim1.text(4, "+1 555 010 0001");
        sim1.int(5, 0x4285F4);
        sim1.int(6, 1);
        let mut sim2 = Buf::new();
        sim2.text(1, "sim-work");
        sim2.int(2, 2);
        sim2.text(3, "Work");
        sim2.text(4, "+1 555 010 0099");
        sim2.int(5, 0x34A853);
        sim2.int(6, 0);
        let mut settings = Buf::new();
        settings.msg(2, &sim1.0);
        settings.msg(2, &sim2.0);
        let sims = decode_settings(&settings.0);
        assert_eq!(sims.len(), 2);
        assert_eq!(sims[0].id, "sim-personal");
        assert_eq!(sims[0].slot, 1);
        assert_eq!(sims[0].carrier, "T-Mobile");
        assert_eq!(sims[0].number, "+1 555 010 0001");
        assert_eq!(sims[0].color, Some(0x4285F4));
        assert!(sims[0].rcs);
        assert_eq!(sims[1].id, "sim-work");
        assert_eq!(sims[1].slot, 2);
        assert_eq!(sims[1].label(), "Work");
        assert!(!sims[1].rcs);

        let default_req = fields(&send_request("c1", "hi", "t1", "me", None, None));
        assert!(field(&default_req, 4).is_none(), "default send must not override SIM");

        let sim2_req = fields(&send_request("c1", "hi", "t2", "me", None, Some("sim-work")));
        assert_eq!(field_text(&fields(&field_bytes(&sim2_req, 4)), 1), "sim-work");
    }
}
