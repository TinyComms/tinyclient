//! Google account pairing wire format (authentication.proto and ukey.proto).
//! Field definitions: https://github.com/mautrix/gmessages/tree/main/pkg/libgm/gmproto

use super::*;
use sha2::{Digest, Sha512};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

const GAIA: &str = "https://instantmessaging-pa.clients6.google.com/$rpc/google.internal.communications.instantmessaging.v1.Registration/SignInGaia";
const KEY_INFO: [u8; 32] = [
    130, 170, 85, 160, 211, 151, 248, 131, 70, 202, 28, 238, 141, 57, 9, 185, 95, 19, 250, 125,
    235, 29, 74, 179, 131, 118, 184, 37, 109, 168, 85, 16,
];

fn validate_cookies(cookies: &HashMap<String, String>) -> Result<(), String> {
    for name in ["SID", "HSID", "SSID", "OSID", "APISID", "SAPISID"] {
        if cookies
            .get(name)
            .is_none_or(|value| value.is_empty() || value.contains(['\r', '\n', ';']))
        {
            return Err(format!(
                "Google sign-in did not provide the required {name} cookie. Sign in again."
            ));
        }
    }
    Ok(())
}

pub(super) fn cookie_header(cookies: &HashMap<String, String>) -> String {
    let mut pairs: Vec<_> = cookies.iter().collect();
    pairs.sort_by_key(|(name, _)| *name);
    pairs
        .into_iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("; ")
}

pub(super) fn authorization(cookies: &str) -> Option<String> {
    let sapisid = cookies
        .split(';')
        .find_map(|pair| pair.trim().strip_prefix("SAPISID="))?;
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    let hash = sha1::Sha1::digest(format!("{timestamp} {sapisid} {ORIGIN}").as_bytes());
    Some(format!("SAPISIDHASH {timestamp}_{}", hex(&hash)))
}

pub(super) fn update_cookies(saved: &mut Saved, header: &str) {
    for pair in header.split(';') {
        if let Some((name, value)) = pair.trim().split_once('=') {
            saved.cookies.insert(name.to_owned(), value.to_owned());
        }
    }
}

pub(super) fn acknowledge(values: &[serde_json::Value], live: &Mutex<Live>) {
    let (body, cookies) = {
        let live = live.lock().unwrap_or_else(|p| p.into_inner());
        if live.saved.cookies.is_empty() {
            return;
        }
        let browser = serde_json::json!([
            live.saved.browser_user.to_string(),
            live.saved.browser,
            "GDitto"
        ]);
        let acks: Vec<_> = values
            .iter()
            .filter_map(|value| {
                at(value, 2)
                    .and_then(|v| at(v, 1))
                    .and_then(|v| v.as_str())
                    .filter(|id| !id.is_empty())
            })
            .map(|id| serde_json::json!([id, browser]))
            .collect();
        if acks.is_empty() {
            return;
        }
        let mut auth = pblite_auth(&uuid(), &live.token);
        auth[2] = "GDitto".into();
        (
            serde_json::to_vec(&serde_json::json!([auth, [], null, acks])).unwrap_or_default(),
            live.cookies.clone(),
        )
    };
    if let Ok(response) = post(
        &agent(Duration::from_secs(10)),
        &format!("{RELAY}.Messaging/AckMessages"),
        "application/json+protobuf",
        &body,
        &cookies,
    ) {
        let mut live = live.lock().unwrap_or_else(|p| p.into_inner());
        live.cookies = merge_cookies(&live.cookies, &response.cookies);
        let cookies = live.cookies.clone();
        update_cookies(&mut live.saved, &cookies);
    }
}

fn derive(key: &[u8], salt: &[u8], info: &[u8]) -> Vec<u8> {
    let mut out = vec![0; 32];
    hkdf::Hkdf::<Sha256>::new(Some(salt), key)
        .expand(info, &mut out)
        .expect("32-byte HKDF output");
    out
}

fn network_auth() -> serde_json::Value {
    let mut auth = pblite_auth(&uuid(), &[]);
    auth[2] = "GDitto".into();
    auth
}

fn text_binary(value: &serde_json::Value) -> String {
    value
        .as_str()
        .and_then(unb64)
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_default()
}

/// Reissue the token for the stable Google web device before reconnecting.
pub(super) fn refresh(saved: &mut Saved) -> Result<(), String> {
    sign_in(saved).map(|_| ())
}

fn sign_in(saved: &mut Saved) -> Result<serde_json::Value, String> {
    let signing = SigningKey::from_slice(
        &unhex(&saved.key).ok_or("The Messages signing key is unreadable.")?,
    )
    .map_err(|_| "The Messages signing key is unreadable.")?;
    let public = signing
        .verifying_key()
        .to_public_key_der()
        .map_err(|_| "Could not encode the Messages signing key.")?;
    let mut key = Buf::new();
    key.bytes(3, public.as_bytes());
    let mut inner = vec![serde_json::Value::Null; 36];
    inner[0] = serde_json::json!([
        3,
        format!("messages-web-{}", saved.session_id.replace('-', ""))
    ]);
    inner[35] = b64(&key.0).into();
    let body = serde_json::to_vec(&serde_json::json!([network_auth(), inner, null, "GDitto"]))
        .map_err(|e| e.to_string())?;
    let response = post(
        &agent(Duration::from_secs(30)),
        GAIA,
        "application/json+protobuf",
        &body,
        &cookie_header(&saved.cookies),
    )?;
    update_cookies(saved, &response.cookies);
    let response: serde_json::Value = serde_json::from_slice(&response.body)
        .map_err(|_| "Google returned an unreadable Messages sign-in response.")?;
    let token =
        at(&response, 4).ok_or("Google did not issue a Messages session. Sign in again.")?;
    saved.token = at(token, 1)
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_owned();
    saved.ttl = at(token, 2).map(as_i64).unwrap_or(0);
    let device = at(&response, 3)
        .and_then(|v| at(v, 1))
        .and_then(|v| at(v, 1))
        .ok_or("Google returned no Messages device.")?;
    saved.browser_user = at(device, 1).map(as_i64).unwrap_or(0);
    saved.mobile_user = saved.browser_user;
    saved.browser = at(device, 2)
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_owned();
    saved.mobile = saved.browser.to_lowercase();
    if saved.token.is_empty() || saved.browser.is_empty() {
        return Err("Google returned an incomplete Messages session.".into());
    }
    Ok(response)
}

fn fetch_config(saved: &mut Saved) -> Result<(), String> {
    let cookies = cookie_header(&saved.cookies);
    let mut request = agent(Duration::from_secs(30))
        .get("https://messages.google.com/web/config")
        .header("Cookie", &cookies);
    if let Some(auth) = authorization(&cookies) {
        request = request.header("Authorization", &auth);
    }
    let mut response = request
        .call()
        .map_err(|_| "Could not load Google Messages configuration.")?;
    if !response.status().is_success() {
        return Err("Google refused the Messages configuration. Sign in again.".into());
    }
    for header in response.headers().get_all("set-cookie") {
        if let Some(pair) = header.to_str().ok().and_then(|line| line.split(';').next()) {
            update_cookies(saved, pair);
        }
    }
    let value: serde_json::Value = response
        .body_mut()
        .read_json()
        .map_err(|_| "Google returned unreadable Messages configuration.")?;
    if let Some(id) = at(&value, 5)
        .and_then(|v| at(v, 4))
        .and_then(|v| v.as_str())
        .filter(|id| !id.is_empty())
    {
        saved.session_id = id.to_owned();
    }
    Ok(())
}

struct Handshake {
    secret: p256::SecretKey,
    init: Vec<u8>,
    finish: Vec<u8>,
}

fn wrap(kind: u64, data: &[u8]) -> Vec<u8> {
    let mut out = Buf::new();
    out.int(1, kind);
    out.bytes(2, data);
    out.0
}

impl Handshake {
    fn new() -> Result<Self, String> {
        let secret = p256::SecretKey::from_slice(&random(32))
            .map_err(|_| "Could not create a Messages pairing key.")?;
        use p256::elliptic_curve::sec1::ToEncodedPoint;
        let point = secret.public_key().to_encoded_point(false);
        let mut coordinates = Buf::new();
        coordinates.bytes(1, &[&[0u8][..], point.x().unwrap().as_slice()].concat());
        coordinates.bytes(2, &[&[0u8][..], point.y().unwrap().as_slice()].concat());
        let mut public = Buf::new();
        public.int(1, 1);
        public.msg(2, &coordinates.0);
        let mut finished = Buf::new();
        finished.msg(1, &public.0);
        let finish = wrap(4, &finished.0);
        let mut commitment = Buf::new();
        commitment.int(1, 100);
        commitment.bytes(2, &Sha512::digest(&finish));
        let mut init = Buf::new();
        init.int(1, 1);
        init.bytes(2, &random(32));
        init.msg(3, &commitment.0);
        init.text(4, "AES_256_CBC-HMAC_SHA256");
        Ok(Self {
            secret,
            init: wrap(2, &init.0),
            finish,
        })
    }

    fn process(&self, response: &[u8]) -> Result<(String, Vec<u8>, u64), String> {
        let container = fields(response);
        let server = field_bytes(&container, 5);
        let wrapper = fields(&server);
        if field_int(&wrapper, 1) != 3 {
            return Err("Messages returned an unexpected pairing handshake.".into());
        }
        let init = fields(&field_bytes(&wrapper, 2));
        if field_int(&init, 1) != 1
            || field_int(&init, 3) != 100
            || field_bytes(&init, 2).len() != 32
        {
            return Err("Messages returned an unsupported pairing handshake.".into());
        }
        let public = fields(&field_bytes(&init, 4));
        if field_int(&public, 1) != 1 {
            return Err("Messages returned an unsupported pairing public key.".into());
        }
        let coords = fields(&field_bytes(&public, 2));
        let coordinate = |n| -> Result<Vec<u8>, String> {
            let mut bytes = field_bytes(&coords, n);
            if bytes.len() == 33 && bytes[0] == 0 {
                bytes.remove(0);
            }
            if bytes.len() != 32 {
                return Err("Messages returned an invalid pairing public key.".into());
            }
            Ok(bytes)
        };
        let point = [
            &[4u8][..],
            coordinate(1)?.as_slice(),
            coordinate(2)?.as_slice(),
        ]
        .concat();
        let public = p256::PublicKey::from_sec1_bytes(&point)
            .map_err(|_| "Messages returned an invalid pairing public key.")?;
        let shared =
            p256::ecdh::diffie_hellman(self.secret.to_nonzero_scalar(), public.as_affine());
        let shared = Sha256::digest(shared.raw_secret_bytes());
        let info = [&self.init[..], &server[..]].concat();
        let auth = derive(&shared, b"UKEY2 v1 auth", &info);
        let next = derive(&shared, b"UKEY2 v1 next", &info);
        let emojis = emoji_list(field_int(&container, 6))?;
        let emoji = emojis
            [u32::from_be_bytes(auth[..4].try_into().unwrap()) as usize % emojis.len()]
        .clone();
        Ok((emoji, next, field_int(&container, 7)))
    }
}

fn keys(next: &[u8], version: u64) -> Result<(Vec<u8>, Vec<u8>), String> {
    let client = derive(next, &KEY_INFO, b"client");
    let server = derive(next, &KEY_INFO, b"server");
    if version == 0 {
        return Ok((client, server));
    }
    if version != 1 {
        return Err("Messages uses an unsupported pairing key version.".into());
    }
    let byte_hash = |bytes: &[u8]| {
        bytes.iter().fold(1i32, |hash, byte| {
            hash.wrapping_mul(31).wrapping_add(*byte as i8 as i32)
        })
    };
    let (first, second) = if byte_hash(&client) < byte_hash(&server) {
        (&client, &server)
    } else {
        (&server, &client)
    };
    let digest = Sha256::digest([&KEY_INFO[..], &first[..], &second[..]].concat());
    Ok((
        derive(&digest, b"Ditto salt 1", b"Ditto info 1"),
        derive(&digest, b"Ditto salt 2", b"Ditto info 2"),
    ))
}

fn pairing_container(id: &str, start: u64, data: &[u8], finished: bool) -> Vec<u8> {
    let mut out = Buf::new();
    out.text(1, id);
    out.msg(2, &browser_details());
    out.int(3, start);
    out.bytes(4, data);
    if finished {
        out.text(8, "This is an undocumented API. Use or access of undocumented Google APIs without express authorization is prohibited per the Google API Terms of Service (https://developers.google.com/terms).");
    } else {
        out.int(5, 1);
        out.int(6, 1);
    }
    out.0
}

pub fn pair_google(
    cookies: HashMap<String, String>,
    show: &dyn Fn(String),
    cancel: &AtomicBool,
) -> Result<Saved, String> {
    validate_cookies(&cookies)?;
    let signing = SigningKey::from_slice(&random(32))
        .map_err(|_| "Could not create a Messages signing key.")?;
    let mut saved = Saved {
        account: String::new(),
        name: "Messages".into(),
        aes: hex(&random(32)),
        hmac: hex(&random(32)),
        token: String::new(),
        ttl: 0,
        mobile_user: 0,
        mobile: String::new(),
        browser_user: 0,
        browser: String::new(),
        key: hex(signing.to_bytes().as_slice()),
        cookies,
        session_id: uuid(),
        pairing_id: uuid(),
        dest_reg_id: String::new(),
        dest_phone_id: 0,
    };
    fetch_config(&mut saved)?;
    let response = sign_in(&mut saved)?;
    let devices = at(&response, 3)
        .and_then(|v| at(v, 2))
        .and_then(|v| v.as_array())
        .ok_or("No Android phone is signed in to this Google Messages account.")?;
    let device = devices
        .iter()
        .find(|v| at(v, 4).map(as_i64) == Some(1))
        .ok_or("No Android phone is signed in to this Google Messages account.")?;
    saved.dest_reg_id = at(device, 1).map(text_binary).unwrap_or_default();
    let phone_number = at(device, 7).map(as_i64).unwrap_or(0) as u64;
    if saved.dest_reg_id.is_empty() {
        return Err("Google returned an unreadable phone identifier.".into());
    }
    if let Some(devices) = at(&response, 3)
        .and_then(|v| at(v, 3))
        .and_then(|v| v.as_array())
    {
        if let Some(phone) = devices
            .iter()
            .find(|v| at(v, 1).map(text_binary).as_deref() == Some(saved.dest_reg_id.as_str()))
        {
            if let Some(value) = at(phone, 8).and_then(|v| v.as_str()).and_then(unb64) {
                saved.dest_phone_id = field_int(&fields(&value), 2);
            }
        }
    }
    let handshake = Handshake::new()?;
    let start = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_millis() as u64;
    let session = Session::open(saved.clone())?;
    let outcome = (|| {
        // The poll has to be connected before the phone can answer the initial RPC.
        let events = session
            .events()
            .ok_or("Messages did not start its pairing connection.")?;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            if Instant::now() >= deadline {
                return Err("Could not connect to Google Messages.".into());
            }
            match events.recv_timeout(Duration::from_millis(100)) {
                Ok(crate::adapters::messages::Event::Ready) => break,
                Ok(crate::adapters::messages::Event::Closed(reason)) => return Err(reason),
                _ => {}
            }
        }
        let init = session.rpc_as_cancel(
            &uuid(),
            44,
            &pairing_container(&saved.pairing_id, start, &handshake.init, false),
            20,
            Some(cancel),
        )?;
        let (emoji, next, version) = handshake.process(&init)?;
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        show(emoji);
        let finish = session.rpc_as_cancel(
            &uuid(),
            45,
            &pairing_container(&saved.pairing_id, start, &handshake.finish, true),
            2,
            Some(cancel),
        )?;
        let finish = fields(&finish);
        if field_int(&finish, 1) != 0 {
            return Err(match field_int(&finish, 2) { 5 => "That emoji did not match. Start again and choose the emoji shown here on your phone.", 7 | 27 => "Messages pairing was cancelled on your phone.", 2 | 3 | 6 | 17 | 18 => "Messages pairing timed out. Keep your phone online and try again.", _ => "Google Messages could not pair this computer. Try again." }.into());
        }
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let (aes, hmac) = keys(&next, version)?;
        saved.aes = hex(&aes);
        saved.hmac = hex(&hmac);
        saved.account = format!(
            "{}/{}",
            saved.mobile,
            if saved.dest_phone_id == 0 {
                phone_number
            } else {
                saved.dest_phone_id
            }
        );
        let live = session.live.lock().unwrap_or_else(|p| p.into_inner());
        saved.token = b64(&live.token);
        saved.ttl = live.ttl;
        saved.browser = live.saved.browser.clone();
        saved.browser_user = live.saved.browser_user;
        saved.mobile = live.saved.mobile.clone();
        saved.mobile_user = live.saved.mobile_user;
        update_cookies(&mut saved, &live.cookies);
        Ok(saved)
    })();
    session.close();
    outcome
}

fn emoji_list(version: u64) -> Result<Vec<String>, String> {
    let mut emojis: Vec<String> = serde_json::from_str(include_str!("google-emojis.json"))
        .expect("bundled pairing emoji table");
    if version == 1 {
        let mut seen = std::collections::HashSet::new();
        emojis.retain(|emoji| seen.insert(emoji.clone()));
        emojis.extend(
            [
                "🍋‍🟩",
                "🐦‍🔥",
                "🐲",
                "🪅",
                "🦜",
                "🏺",
                "🗿",
                "🫐",
                "⛽",
                "🍱",
                "🥡",
                "🧋",
                "🍼",
                "📐",
            ]
            .map(str::to_owned),
        );
        emojis.retain(|emoji| {
            !["💻", "🤗", "💬", "👋", "😁", "😎", "😇", "🥰", "🤓", "🤩"].contains(&emoji.as_str())
        });
    } else if version != 0 {
        return Err("Messages uses an unsupported pairing emoji version.".into());
    }
    Ok(emojis)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_google_cookies_are_rejected_before_network_access() {
        assert!(validate_cookies(&std::collections::HashMap::new()).is_err());
    }

    #[test]
    fn hkdf_matches_rfc5869_case_one() {
        let got = derive(
            &[0x0b; 22],
            &(0..13).collect::<Vec<u8>>(),
            &(0xf0..=0xf9).collect::<Vec<u8>>(),
        );
        assert_eq!(
            super::super::hex(&got),
            "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf"
        );
    }

    #[test]
    fn key_versions_match_independent_hkdf_vectors() {
        let next: Vec<u8> = (0..32).collect();
        let (aes, hmac) = keys(&next, 0).unwrap();
        assert_eq!(
            hex(&aes),
            "efc82501acf57d8f69ca8057f388f32e3f40dfd560eadb41eb910d49c43cd318"
        );
        assert_eq!(
            hex(&hmac),
            "3974d2c43b41bbbb67587761e3f7a1c1f75f8c61916e443f726dd4e796eb20c2"
        );
        let (aes, hmac) = keys(&next, 1).unwrap();
        assert_eq!(
            hex(&aes),
            "5f6ce79dd48af3453f7f24e53268537f7a5962870144de1a52f9ed25765d3a43"
        );
        assert_eq!(
            hex(&hmac),
            "3b0ea4399b37c190dcb52cc976d879424c2713e4b59cdde1b77f84340194c37e"
        );
        assert!(keys(&next, 2).is_err());
    }

    #[test]
    fn handshake_rejects_malformed_server_messages() {
        let handshake = Handshake::new().unwrap();
        assert!(handshake.process(&[]).is_err());
        let mut response = Buf::new();
        response.bytes(5, &wrap(4, &[]));
        assert!(handshake.process(&response.0).is_err());
    }

    #[test]
    fn google_cookie_rotation_replaces_old_values() {
        assert_eq!(
            merge_cookies("SAPISID=old; SID=abc", "SAPISID=new"),
            "SAPISID=new; SID=abc"
        );
        assert!(authorization("SID=abc").is_none());
        assert!(authorization("SID=abc; SAPISID=secret")
            .unwrap()
            .starts_with("SAPISIDHASH "));
    }

    #[test]
    fn open_long_poll_yields_payload_before_stream_closes() {
        // ReceiveMessages starts with two unclosed wrapper arrays. poll consumes
        // that prefix once, then passes the still-open stream body to this parser.
        let wire = b"[[[null,[\"reply-id\",19]], [null,[\"next-id\",19";
        let (values, used) = complete_values(&wire[2..]);
        assert_eq!(values, vec![serde_json::json!([null, ["reply-id", 19]])]);
        assert_eq!(&wire[2 + used..], b"[null,[\"next-id\",19");
        let closing = b"[null,[\"next-id\",19]]]]";
        assert_eq!(
            complete_values(closing).0,
            vec![serde_json::json!([null, ["next-id", 19]])]
        );
    }
}
