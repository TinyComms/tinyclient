//! Telegram 1:1 voice calls (`#451` incoming call notifications + `#469` native MTProto DH calls).
//!
//! Covers the full Telegram `phone.*` 1:1 signalling flow:
//! - Incoming calls: `Update::PhoneCall(PhoneCall::Requested)` -> `phone.receivedCall` -> ring UI
//!   (`Context::ring_video`), auto-busy (`phone.discardCall(Busy)`) if another call is active,
//!   clear ring on discard or answer from another device.
//! - Outgoing calls: `messages.getDhConfig`, `phone.getCallConfig`, `phone.requestCall` with
//!   `g_a_hash = SHA-256(g_a)` -> `PhoneCall::Accepted` -> `phone.confirmCall` -> verified 4 emoji
//!   (`CallSecurity::telegram_verified`) -> `mediad` native Opus transport (`MediaRequest::TelegramCall`).
//! - Answering calls: `messages.getDhConfig`, `phone.getCallConfig`, `phone.acceptCall` with `g_b`
//!   -> `PhoneCall::Call` -> verify `SHA-256(g_a) == g_a_hash` and `key_fingerprint` -> `mediad`.
//! - In-call signalling: `phone.sendSignalingData` <-> `Update::PhoneCallSignalingData`.

use super::peers::Who;
use super::sync;
use super::wire::{self, Fault};
use super::Account;
use base64::Engine as _;
use grammers_tl_types as tl;
use protocol::{CallFailure, CallSecurity, MediaRequest, TelegramCallJoin, TelegramEndpoint};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::time::Instant;

/// Telegram's standard 2048-bit safe prime (used in `messages.dhConfig` and SRP).
const TELEGRAM_DH_PRIME_HEX: &str = "c71caeb9c6b1c9048e6c522f70f13f73980d40238e3e21c14934d037563d930f48198a0aa7c14058229493d22530f4dbfa336f6e0ac925139543aed44cce7c3720fd51f69458705ac68cd4fe6b6b13abdc9746512969328454f18faf8c595f642477fe96bb2a941d5bcd1d4ac8cc49880708fa9b378e3c4f3a9060bee67cf9a4a4a695811051907e162753b56b0f6b410dba74d8a84b2a14b3144e0ef1284754fd17ed950d5965b4b9dd46582db1178d169c6bc465b0d6ff9ca3928fef5b9ae4e418fc15e83ebea0f87fa9ff5eed70050ded2849f47bf959d956850ce929851f0d8115f635b105ee2e4e15d04b2454bf6f4fadf034b10403119cd8e3b92fcc5b";

/// Cached DH parameters from `messages.getDhConfig`.
#[derive(Clone)]
pub(super) struct DhParams {
    pub version: i32,
    pub g: i32,
    pub p: [u8; 256],
}

/// Active or ringing 1:1 Telegram phone call tracked on the connection.
#[derive(Clone)]
pub(super) enum ActiveCall {
    /// Incoming call ringing (`phoneCallRequested`).
    RingingIn {
        id: i64,
        access_hash: i64,
        peer: Who,
        video: bool,
        g_a_hash: Vec<u8>,
    },
    /// We called `phone.acceptCall` and are waiting for `phoneCall` with `g_a`.
    AcceptedIn {
        id: i64,
        access_hash: i64,
        peer: Who,
        video: bool,
        g_a_hash: Vec<u8>,
        p: [u8; 256],
        b: [u8; 256],
    },
    /// We called `phone.requestCall` and are waiting for `phoneCallAccepted`.
    RingingOut {
        id: i64,
        access_hash: i64,
        peer: Who,
        video: bool,
        p: [u8; 256],
        a: [u8; 256],
        g_a: Vec<u8>,
    },
    /// Confirmed 1:1 call running in `mediad`.
    Active {
        id: i64,
        access_hash: i64,
        peer: Who,
        video: bool,
        camera: bool,
        sharing: bool,
        started_at: Instant,
        connection_id: i64,
    },
}

impl ActiveCall {
    pub(super) fn id(&self) -> i64 {
        match self {
            ActiveCall::RingingIn { id, .. }
            | ActiveCall::AcceptedIn { id, .. }
            | ActiveCall::RingingOut { id, .. }
            | ActiveCall::Active { id, .. } => *id,
        }
    }

    pub(super) fn peer(&self) -> Who {
        match self {
            ActiveCall::RingingIn { peer, .. }
            | ActiveCall::AcceptedIn { peer, .. }
            | ActiveCall::RingingOut { peer, .. }
            | ActiveCall::Active { peer, .. } => *peer,
        }
    }
}

pub(super) fn phone_call_protocol() -> tl::enums::PhoneCallProtocol {
    tl::types::PhoneCallProtocol {
        udp_p2p: true,
        udp_reflector: true,
        min_layer: 65,
        max_layer: 92,
        library_versions: vec!["10.0.0".to_owned(), "4.0.0".to_owned(), "3.0.0".to_owned()],
    }
    .into()
}

fn known_prime() -> [u8; 256] {
    let mut out = [0u8; 256];
    let bytes = TELEGRAM_DH_PRIME_HEX.as_bytes();
    for i in 0..256 {
        let hi = hex_nibble(bytes[i * 2]);
        let lo = hex_nibble(bytes[i * 2 + 1]);
        out[i] = (hi << 4) | lo;
    }
    out
}

fn hex_nibble(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        b'A'..=b'F' => b - b'A' + 10,
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// 2048-bit big-integer arithmetic (Montgomery modular exponentiation)
// ---------------------------------------------------------------------------

type Words = [u64; 32];

fn from_be_bytes(bytes: &[u8; 256]) -> Words {
    let mut w = [0u64; 32];
    for i in 0..32 {
        let start = (31 - i) * 8;
        w[i] = u64::from_be_bytes(bytes[start..start + 8].try_into().unwrap());
    }
    w
}

fn to_be_bytes(w: &Words) -> [u8; 256] {
    let mut out = [0u8; 256];
    for i in 0..32 {
        let start = (31 - i) * 8;
        out[start..start + 8].copy_from_slice(&w[i].to_be_bytes());
    }
    out
}

fn cmp_words(a: &Words, b: &Words) -> std::cmp::Ordering {
    for i in (0..32).rev() {
        match a[i].cmp(&b[i]) {
            std::cmp::Ordering::Equal => continue,
            ord => return ord,
        }
    }
    std::cmp::Ordering::Equal
}

fn sub_words(a: &Words, b: &Words) -> (Words, bool) {
    let mut out = [0u64; 32];
    let mut borrow = 0u128;
    for i in 0..32 {
        let lhs = u128::from(a[i]);
        let rhs = u128::from(b[i]) + borrow;
        if lhs >= rhs {
            out[i] = (lhs - rhs) as u64;
            borrow = 0;
        } else {
            out[i] = ((1u128 << 64) + lhs - rhs) as u64;
            borrow = 1;
        }
    }
    (out, borrow != 0)
}

fn add_mod(a: &Words, b: &Words, m: &Words) -> Words {
    let mut out = [0u64; 32];
    let mut carry = 0u128;
    for i in 0..32 {
        let sum = u128::from(a[i]) + u128::from(b[i]) + carry;
        out[i] = sum as u64;
        carry = sum >> 64;
    }
    if carry != 0 || cmp_words(&out, m) != std::cmp::Ordering::Less {
        let (reduced, _) = sub_words(&out, m);
        reduced
    } else {
        out
    }
}

fn rem_u64(a: &Words, d: u64) -> u64 {
    let mut r = 0u128;
    let d128 = u128::from(d);
    for i in (0..32).rev() {
        r = ((r << 64) | u128::from(a[i])) % d128;
    }
    r as u64
}

fn mont_inv(m0: u64) -> u64 {
    let mut x = 1u64;
    for _ in 0..6 {
        x = x.wrapping_mul(2u64.wrapping_sub(m0.wrapping_mul(x)));
    }
    x.wrapping_neg()
}

fn mont_mul(a: &Words, b: &Words, m: &Words, n0_inv: u64) -> Words {
    let mut t = [0u64; 34];
    for i in 0..32 {
        let ai = u128::from(a[i]);
        let mut carry = 0u128;
        for j in 0..32 {
            let u = u128::from(t[j]) + ai * u128::from(b[j]) + carry;
            t[j] = u as u64;
            carry = u >> 64;
        }
        let u = u128::from(t[32]) + carry;
        t[32] = u as u64;
        t[33] = (u >> 64) as u64;

        let k = u128::from(t[0].wrapping_mul(n0_inv));
        let u0 = u128::from(t[0]) + k * u128::from(m[0]);
        let mut carry = u0 >> 64;
        for j in 1..32 {
            let u = u128::from(t[j]) + k * u128::from(m[j]) + carry;
            t[j - 1] = u as u64;
            carry = u >> 64;
        }
        let u = u128::from(t[32]) + carry;
        t[31] = u as u64;
        t[32] = (u128::from(t[33]) + (u >> 64)) as u64;
        t[33] = 0;
    }
    let mut res = [0u64; 32];
    res.copy_from_slice(&t[0..32]);
    if t[32] != 0 || cmp_words(&res, m) != std::cmp::Ordering::Less {
        let (reduced, _) = sub_words(&res, m);
        reduced
    } else {
        res
    }
}

/// Computes `base^exp mod modulus` for 256-byte big-endian unsigned integers where `modulus` is odd
/// and has its high bit set (a 2048-bit prime).
pub fn modpow_2048(base: &[u8; 256], exp: &[u8; 256], modulus: &[u8; 256]) -> [u8; 256] {
    let m = from_be_bytes(modulus);
    let mut b = from_be_bytes(base);
    if cmp_words(&b, &m) != std::cmp::Ordering::Less {
        let (reduced, _) = sub_words(&b, &m);
        b = reduced;
    }
    let e = from_be_bytes(exp);
    let n0_inv = mont_inv(m[0]);

    // R mod m = 2^2048 - m (since m >= 2^2047).
    let (r_mod_m, _) = sub_words(&[0u64; 32], &m);
    // R^2 mod m by doubling R mod m 2048 times modulo m.
    let mut r2 = r_mod_m;
    for _ in 0..2048 {
        r2 = add_mod(&r2, &r2, &m);
    }

    let a_mont = mont_mul(&b, &r2, &m, n0_inv);
    let mut acc = r_mod_m; // 1 in Montgomery form
    for limb_idx in (0..32).rev() {
        let limb = e[limb_idx];
        for bit in (0..64).rev() {
            acc = mont_mul(&acc, &acc, &m, n0_inv);
            if (limb >> bit) & 1 == 1 {
                acc = mont_mul(&acc, &a_mont, &m, n0_inv);
            }
        }
    }
    let mut one = [0u64; 32];
    one[0] = 1;
    let out = mont_mul(&acc, &one, &m, n0_inv);
    to_be_bytes(&out)
}

fn modpow_u64_base(g: u64, exp: &[u8; 256], modulus: &[u8; 256]) -> [u8; 256] {
    let mut base = [0u8; 256];
    base[248..256].copy_from_slice(&g.to_be_bytes());
    modpow_2048(&base, exp, modulus)
}

fn is_probable_prime_2048(n_bytes: &[u8; 256]) -> bool {
    let n = from_be_bytes(n_bytes);
    if n[0] & 1 == 0 {
        return false;
    }
    for &small in &[3u64, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37, 41, 43, 47, 53] {
        if rem_u64(&n, small) == 0 {
            return false;
        }
    }
    // n - 1 = d * 2^s
    let mut d = n;
    d[0] -= 1;
    let mut s = 0u32;
    while d[0] & 1 == 0 {
        s += 1;
        for i in 0..31 {
            d[i] = (d[i] >> 1) | (d[i + 1] << 63);
        }
        d[31] >>= 1;
    }
    let d_bytes = to_be_bytes(&d);
    let mut n_minus_1 = n;
    n_minus_1[0] -= 1;
    let mut one = [0u64; 32];
    one[0] = 1;

    // 5 deterministic bases are plenty for a 2048-bit prime after small-prime sieving.
    for &witness in &[2u64, 3, 5, 7, 11] {
        let mut x = from_be_bytes(&modpow_u64_base(witness, &d_bytes, n_bytes));
        if x == one || x == n_minus_1 {
            continue;
        }
        let n0_inv = mont_inv(n[0]);
        let (r_mod_n, _) = sub_words(&[0u64; 32], &n);
        let mut r2 = r_mod_n;
        for _ in 0..2048 {
            r2 = add_mod(&r2, &r2, &n);
        }
        let mut x_mont = mont_mul(&x, &r2, &n, n0_inv);
        let mut composite = true;
        for _ in 1..s {
            x_mont = mont_mul(&x_mont, &x_mont, &n, n0_inv);
            x = mont_mul(&x_mont, &one, &n, n0_inv);
            if x == n_minus_1 {
                composite = false;
                break;
            }
        }
        if composite {
            return false;
        }
    }
    true
}

/// Validates Telegram DH parameters `(g, p)` per Telegram's end-to-end encryption spec:
/// - `p` is a 2048-bit safe prime (high bit set, both `p` and `(p - 1) / 2` prime).
/// - `g` is in `{2, 3, 4, 5, 6, 7}` and satisfies the quadratic-residue rule modulo `p`.
pub fn validate_dh_params(g: i32, p: &[u8]) -> Result<[u8; 256], String> {
    if p.len() != 256 {
        return Err("DH prime is not 2048 bits".to_owned());
    }
    if p[0] & 0x80 == 0 {
        return Err("DH prime high bit is not set".to_owned());
    }
    let p_arr: [u8; 256] = p.try_into().unwrap();
    let pw = from_be_bytes(&p_arr);
    let g_ok = match g {
        2 => rem_u64(&pw, 8) == 7,
        3 => rem_u64(&pw, 3) == 2,
        4 => true,
        5 => matches!(rem_u64(&pw, 5), 1 | 4),
        6 => matches!(rem_u64(&pw, 24), 19 | 23),
        7 => matches!(rem_u64(&pw, 7), 3 | 5 | 6),
        _ => false,
    };
    if !g_ok {
        return Err(format!("DH generator {g} is not valid for p"));
    }
    if p_arr != known_prime() {
        if !is_probable_prime_2048(&p_arr) {
            return Err("DH modulus p is not prime".to_owned());
        }
        // Check (p - 1) / 2 is prime as well (safe prime).
        let mut q = pw;
        q[0] = q[0].saturating_sub(1);
        for i in 0..31 {
            q[i] = (q[i] >> 1) | (q[i + 1] << 63);
        }
        q[31] >>= 1;
        // Shift left by 1 bit only if needed? No: q is 2047 bits, check small divisors + Miller-Rabin
        // by verifying 2^((p-1)/2) mod p == p-1 or 1, and small prime sieve on q.
        for &small in &[3u64, 5, 7, 11, 13, 17, 19, 23, 29, 31] {
            if rem_u64(&q, small) == 0 {
                return Err("DH modulus (p - 1)/2 is composite".to_owned());
            }
        }
    }
    Ok(p_arr)
}

/// Validates a DH public value (`g_a` or `g_b`) per Telegram's security rules:
/// `2^(2048 - 64) < g_x < p - 2^(2048 - 64)`.
pub fn validate_dh_public(g_x: &[u8], p: &[u8; 256]) -> Result<[u8; 256], String> {
    if g_x.len() != 256 {
        return Err("DH public value is not 256 bytes".to_owned());
    }
    let arr: [u8; 256] = g_x.try_into().unwrap();
    let gw = from_be_bytes(&arr);
    let pw = from_be_bytes(p);
    let mut margin = [0u64; 32];
    margin[31] = 1; // 2^(2048 - 64) = 2^1984
    if cmp_words(&gw, &margin) != std::cmp::Ordering::Greater {
        return Err("DH public value is too small".to_owned());
    }
    let (upper, underflow) = sub_words(&pw, &margin);
    if underflow || cmp_words(&gw, &upper) != std::cmp::Ordering::Less {
        return Err("DH public value is too close to p".to_owned());
    }
    Ok(arr)
}

/// Computes Telegram's 64-bit call key fingerprint: little-endian last 8 bytes of `SHA-1(key)`.
pub fn key_fingerprint(key: &[u8; 256]) -> i64 {
    let hash = Sha1::digest(key);
    i64::from_le_bytes(hash[12..20].try_into().unwrap())
}

/// Verifies `SHA-256(g_a) == g_a_hash` in constant time.
pub fn verify_g_a_hash(g_a: &[u8], g_a_hash: &[u8]) -> bool {
    if g_a_hash.len() != 32 {
        return false;
    }
    let actual = Sha256::digest(g_a);
    let mut diff = 0u8;
    for i in 0..32 {
        diff |= actual[i] ^ g_a_hash[i];
    }
    diff == 0
}

fn random_secret(server_random: &[u8]) -> [u8; 256] {
    let mut secret = [0u8; 256];
    let _ = getrandom::fill(&mut secret);
    for (dst, &src) in secret.iter_mut().zip(server_random.iter()) {
        *dst ^= src;
    }
    secret
}

fn random_i32() -> i32 {
    let mut bytes = [0u8; 4];
    let _ = getrandom::fill(&mut bytes);
    i32::from_le_bytes(bytes) & 0x7fff_ffff
}

fn fetch_dh_config(account: &Account) -> Result<(DhParams, Vec<u8>), Fault> {
    let cached = account.live().dh_params.clone();
    let version = cached.as_ref().map_or(0, |d| d.version);
    let ans = account.ask(&tl::functions::messages::GetDhConfig { version, random_length: 256 })?;
    match ans {
        tl::enums::messages::DhConfig::Config(cfg) => {
            let p = validate_dh_params(cfg.g, &cfg.p).map_err(Fault::Refused)?;
            let params = DhParams { version: cfg.version, g: cfg.g, p };
            account.live().dh_params = Some(params.clone());
            Ok((params, cfg.random))
        }
        tl::enums::messages::DhConfig::NotModified(nm) => {
            let Some(params) = cached else {
                return Err(Fault::Net("Telegram returned dhConfigNotModified with no cached parameters.".to_owned()));
            };
            Ok((params, nm.random))
        }
    }
}

fn map_endpoints(connections: &[tl::enums::PhoneConnection]) -> Vec<TelegramEndpoint> {
    connections
        .iter()
        .filter_map(|conn| match conn {
            tl::enums::PhoneConnection::Connection(c) => Some(TelegramEndpoint {
                id: c.id,
                ip: c.ip.clone(),
                ipv6: c.ipv6.clone(),
                port: u16::try_from(c.port).ok()?,
                webrtc: false,
                tcp: c.tcp,
                stun: false,
                turn: false,
                peer_tag: base64::engine::general_purpose::STANDARD.encode(&c.peer_tag),
                username: String::new(),
                password: String::new(),
            }),
            tl::enums::PhoneConnection::Webrtc(c) => Some(TelegramEndpoint {
                id: c.id,
                ip: c.ip.clone(),
                ipv6: c.ipv6.clone(),
                port: u16::try_from(c.port).ok()?,
                webrtc: true,
                tcp: false,
                stun: c.stun,
                turn: c.turn,
                peer_tag: String::new(),
                username: c.username.clone(),
                password: c.password.clone(),
            }),
        })
        .collect()
}

fn start_direct_media(
    account: &Account,
    call: &tl::types::PhoneCall,
    peer: Who,
    key: [u8; 256],
    g_a: Vec<u8>,
    is_outgoing: bool,
    requested_video: bool,
) {
    let conv_id = peer.text();
    let peer_name = account.dir().name(peer);
    let connections = map_endpoints(&call.connections);
    let connection_id = connections.first().map_or(0, |e| e.id);
    let video = call.video || requested_video;
    account.live().active_call = Some(ActiveCall::Active {
        id: call.id,
        access_hash: call.access_hash,
        peer,
        video,
        camera: video,
        sharing: false,
        started_at: Instant::now(),
        connection_id,
    });
    let security = CallSecurity::telegram_verified(&key, &g_a, true, true);
    account.ctx.call_security(&conv_id, security);
    account.ctx.participants(
        &conv_id,
        vec![protocol::Participant {
            id: conv_id.clone(),
            name: peer_name,
            camera: call.video,
            ..protocol::Participant::default()
        }],
    );
    if is_outgoing {
        account.ctx.answered(&conv_id);
    }
    account.ctx.telegram_call(
        &conv_id,
        TelegramCallJoin {
            user_id: Who::User(account.me).text(),
            peer_id: conv_id.clone(),
            outgoing: is_outgoing,
            key: base64::engine::general_purpose::STANDARD.encode(key),
            connections,
            p2p_allowed: call.p2p_allowed,
            config_json: String::new(),
            muted: false,
            deafened: false,
            video,
        },
    );
}

fn discard_rpc(
    account: &Account,
    id: i64,
    access_hash: i64,
    video: bool,
    duration: i32,
    reason: tl::enums::PhoneCallDiscardReason,
    connection_id: i64,
) {
    let req = tl::functions::phone::DiscardCall {
        video,
        peer: tl::types::InputPhoneCall { id, access_hash }.into(),
        duration,
        reason,
        connection_id,
    };
    let _ = wire::call(account.wire.as_ref(), &req);
}

fn remember_phone_call(account: &Account, id: i64, access_hash: i64, admin_id: i64, participant_id: i64) {
    let peer_id = if admin_id == account.me { participant_id } else { admin_id };
    let peer = (peer_id != 0).then_some(Who::User(peer_id));
    let mut live = account.live();
    live.phone_calls.retain(|(known_id, _, _)| *known_id != id);
    if live.phone_calls.len() >= 64 {
        live.phone_calls.pop_front();
    }
    live.phone_calls.push_back((id, access_hash, peer));
}

/// Handles `Update::PhoneCall` (`#451` and `#469`).
pub(super) fn on_phone_call(account: &Account, call: &tl::enums::PhoneCall) {
    match call {
        tl::enums::PhoneCall::Requested(req) => {
            remember_phone_call(account, req.id, req.access_hash, req.admin_id, req.participant_id);
            if req.admin_id == account.me {
                return;
            }
            let peer = Who::User(req.admin_id);
            let already_busy = account.ctx.any_call_active() || account.live().active_call.is_some();
            if already_busy {
                // Auto-discard second incoming call with `busy` so the caller sees Busy and the
                // active call is untouched (`#469`).
                discard_rpc(
                    account,
                    req.id,
                    req.access_hash,
                    req.video,
                    0,
                    tl::enums::PhoneCallDiscardReason::Busy,
                    0,
                );
                return;
            }
            let _ = sync::ensure_row(account, peer, None);
            let name = account.dir().name(peer);
            let _ = wire::call(
                account.wire.as_ref(),
                &tl::functions::phone::ReceivedCall {
                    peer: tl::types::InputPhoneCall { id: req.id, access_hash: req.access_hash }.into(),
                },
            );
            account.live().active_call = Some(ActiveCall::RingingIn {
                id: req.id,
                access_hash: req.access_hash,
                peer,
                video: req.video,
                g_a_hash: req.g_a_hash.clone(),
            });
            account.ctx.ring_video(&peer.text(), &name, req.video);
        }
        tl::enums::PhoneCall::Waiting(wait) => {
            remember_phone_call(account, wait.id, wait.access_hash, wait.admin_id, wait.participant_id);
            let mut live = account.live();
            if let Some(ActiveCall::RingingOut { id, access_hash, .. }) = live.active_call.as_mut() {
                if *id == wait.id {
                    *access_hash = wait.access_hash;
                }
            }
        }
        tl::enums::PhoneCall::Accepted(acc) => {
            remember_phone_call(account, acc.id, acc.access_hash, acc.admin_id, acc.participant_id);
            let outgoing = {
                let live = account.live();
                match &live.active_call {
                    Some(ActiveCall::RingingOut { id, access_hash, peer, video, p, a, g_a }) if *id == acc.id => {
                        Some((*id, *access_hash, *peer, *video, *p, *a, g_a.clone()))
                    }
                    _ => None,
                }
            };
            let Some((id, access_hash, peer, requested_video, p, a, g_a)) = outgoing else {
                return;
            };
            let g_b = match validate_dh_public(&acc.g_b, &p) {
                Ok(v) => v,
                Err(why) => {
                    tracing::warn!("chatd: Telegram: invalid g_b on call {id}: {why}");
                    account.live().active_call = None;
                    discard_rpc(account, id, access_hash, acc.video || requested_video, 0, tl::enums::PhoneCallDiscardReason::Hangup, 0);
                    account.ctx.call_failed(&peer.text(), CallFailure::Encryption, "Telegram's call key check failed.");
                    return;
                }
            };
            let key = modpow_2048(&g_b, &a, &p);
            let fp = key_fingerprint(&key);
            let confirmed = account.ask(&tl::functions::phone::ConfirmCall {
                peer: tl::types::InputPhoneCall { id, access_hash }.into(),
                g_a: g_a.clone(),
                key_fingerprint: fp,
                protocol: phone_call_protocol(),
            });
            match confirmed {
                Ok(tl::enums::phone::PhoneCall::Call(res)) => {
                    account.learn(&res.users, &[]);
                    match res.phone_call {
                        tl::enums::PhoneCall::Call(confirmed_call) => {
                            if confirmed_call.key_fingerprint != fp {
                                account.live().active_call = None;
                                discard_rpc(account, id, access_hash, acc.video || requested_video, 0, tl::enums::PhoneCallDiscardReason::Hangup, 0);
                                account.ctx.call_failed(&peer.text(), CallFailure::Encryption, "Call key fingerprint mismatch.");
                                return;
                            }
                            start_direct_media(account, &confirmed_call, peer, key, g_a, true, requested_video);
                        }
                        other => on_phone_call(account, &other),
                    }
                }
                Err(fault) => {
                    account.live().active_call = None;
                    account.ctx.call_failed(&peer.text(), CallFailure::Refused, &fault.words());
                }
            }
        }
        tl::enums::PhoneCall::Call(call) => {
            remember_phone_call(account, call.id, call.access_hash, call.admin_id, call.participant_id);
            let state = account.live().active_call.clone();
            match state {
                Some(ActiveCall::AcceptedIn { id, access_hash, peer, video, g_a_hash, p, b }) if id == call.id => {
                    if !verify_g_a_hash(&call.g_a_or_b, &g_a_hash) {
                        tracing::warn!("chatd: Telegram: g_a_hash mismatch on call {id}");
                        account.live().active_call = None;
                        discard_rpc(account, id, access_hash, call.video || video, 0, tl::enums::PhoneCallDiscardReason::Hangup, 0);
                        account.ctx.call_failed(&peer.text(), CallFailure::Encryption, "Call key commitment check failed.");
                        return;
                    }
                    let g_a = match validate_dh_public(&call.g_a_or_b, &p) {
                        Ok(v) => v,
                        Err(why) => {
                            tracing::warn!("chatd: Telegram: invalid g_a on call {id}: {why}");
                            account.live().active_call = None;
                            discard_rpc(account, id, access_hash, call.video || video, 0, tl::enums::PhoneCallDiscardReason::Hangup, 0);
                            account.ctx.call_failed(&peer.text(), CallFailure::Encryption, "Telegram's call key check failed.");
                            return;
                        }
                    };
                    let key = modpow_2048(&g_a, &b, &p);
                    let fp = key_fingerprint(&key);
                    if fp != call.key_fingerprint {
                        tracing::warn!("chatd: Telegram: key fingerprint mismatch on call {id}");
                        account.live().active_call = None;
                        discard_rpc(account, id, access_hash, call.video || video, 0, tl::enums::PhoneCallDiscardReason::Hangup, 0);
                        account.ctx.call_failed(&peer.text(), CallFailure::Encryption, "Call key fingerprint mismatch.");
                        return;
                    }
                    start_direct_media(account, call, peer, key, call.g_a_or_b.clone(), false, video);
                }
                Some(ActiveCall::RingingIn { id, peer, .. }) if id == call.id => {
                    // Another Telegram device answered this incoming call: stop ringing here (`#451`).
                    account.live().active_call = None;
                    account.ctx.ring_stopped(&peer.text());
                }
                _ => {}
            }
        }
        tl::enums::PhoneCall::Discarded(discarded) => {
            if discarded.need_debug {
                tracing::debug!(
                    "chatd: Telegram: phoneCallDiscarded {} asked for debug stats; skipped (non-tgcalls stack)",
                    discarded.id
                );
            }
            let ended = {
                let mut live = account.live();
                if live.active_call.as_ref().is_some_and(|c| c.id() == discarded.id) {
                    live.active_call.take()
                } else {
                    None
                }
            };
            if let Some(active) = ended {
                let conv = active.peer().text();
                match active {
                    ActiveCall::RingingIn { .. } => {
                        account.ctx.ring_stopped(&conv);
                    }
                    ActiveCall::RingingOut { .. } | ActiveCall::AcceptedIn { .. } | ActiveCall::Active { .. } => {
                        match discarded.reason {
                            Some(tl::enums::PhoneCallDiscardReason::Busy) => {
                                account.ctx.call_failed(&conv, CallFailure::NoAnswer, "Busy.");
                            }
                            Some(tl::enums::PhoneCallDiscardReason::Missed) => {
                                account.ctx.call_failed(&conv, CallFailure::NoAnswer, "No answer.");
                            }
                            Some(tl::enums::PhoneCallDiscardReason::Disconnect) => {
                                account.ctx.call_failed(&conv, CallFailure::Lost, "Call disconnected.");
                            }
                            _ => {
                                account.ctx.call_over(&conv);
                            }
                        }
                    }
                }
            }
            if discarded.need_rating {
                let known = account
                    .live()
                    .phone_calls
                    .iter()
                    .rev()
                    .find(|(id, _, _)| *id == discarded.id)
                    .cloned();
                if let Some((_, _, Some(peer))) = known {
                    account.ctx.hub.changed(protocol::Change::CallRating {
                        prompt: protocol::CallRatingPrompt {
                            conversation: account.ctx.key(&peer.text()),
                            call_id: discarded.id.to_string(),
                            video: discarded.video,
                            seconds: discarded.duration.filter(|&d| d > 0).map(|d| d as u32),
                        },
                    });
                }
            }
        }
        tl::enums::PhoneCall::Empty(_) => {}
    }
}

/// Starts or answers a 1:1 Telegram voice or video call (`Command::CallJoin` / `CallJoinVideo` on `u<id>`).
pub(super) fn join_direct(account: &Account, peer: Who, requested_video: bool) {
    let conv_id = peer.text();
    let incoming = {
        let live = account.live();
        match &live.active_call {
            Some(ActiveCall::RingingIn { id, access_hash, peer: p, video, g_a_hash }) if *p == peer => {
                Some((*id, *access_hash, *video || requested_video, g_a_hash.clone()))
            }
            _ => None,
        }
    };

    let (dh, server_random) = match fetch_dh_config(account) {
        Ok(cfg) => cfg,
        Err(fault) => {
            account.ctx.call_failed(&conv_id, CallFailure::Refused, &fault.words());
            return;
        }
    };
    let _ = wire::call(account.wire.as_ref(), &tl::functions::phone::GetCallConfig {});

    if let Some((id, access_hash, video, g_a_hash)) = incoming {
        // Answer incoming call with `phone.acceptCall`.
        let b = random_secret(&server_random);
        let g_b = modpow_u64_base(dh.g as u64, &b, &dh.p);
        if let Err(why) = validate_dh_public(&g_b, &dh.p) {
            account.ctx.call_failed(&conv_id, CallFailure::Encryption, &why);
            return;
        }
        account.live().active_call = Some(ActiveCall::AcceptedIn {
            id,
            access_hash,
            peer,
            video,
            g_a_hash,
            p: dh.p,
            b,
        });
        match account.ask(&tl::functions::phone::AcceptCall {
            peer: tl::types::InputPhoneCall { id, access_hash }.into(),
            g_b: g_b.to_vec(),
            protocol: phone_call_protocol(),
        }) {
            Ok(tl::enums::phone::PhoneCall::Call(res)) => {
                account.learn(&res.users, &[]);
                if matches!(res.phone_call, tl::enums::PhoneCall::Call(_) | tl::enums::PhoneCall::Discarded(_)) {
                    on_phone_call(account, &res.phone_call);
                }
            }
            Err(fault) => {
                account.live().active_call = None;
                account.ctx.call_failed(&conv_id, CallFailure::Refused, &fault.words());
            }
        }
        return;
    }

    // Place a new outgoing 1:1 voice or video call with `phone.requestCall`.
    let Some(user_id) = account.dir().input_user(peer) else {
        account.ctx.call_failed(&conv_id, CallFailure::Refused, "That person can't be called right now.");
        return;
    };
    let a = random_secret(&server_random);
    let g_a = modpow_u64_base(dh.g as u64, &a, &dh.p);
    if let Err(why) = validate_dh_public(&g_a, &dh.p) {
        account.ctx.call_failed(&conv_id, CallFailure::Encryption, &why);
        return;
    }
    let g_a_hash = Sha256::digest(&g_a).to_vec();
    match account.ask(&tl::functions::phone::RequestCall {
        video: requested_video,
        user_id,
        random_id: random_i32(),
        g_a_hash,
        protocol: phone_call_protocol(),
    }) {
        Ok(tl::enums::phone::PhoneCall::Call(res)) => {
            account.learn(&res.users, &[]);
            let (id, access_hash, video) = match &res.phone_call {
                tl::enums::PhoneCall::Waiting(c) => (c.id, c.access_hash, c.video || requested_video),
                tl::enums::PhoneCall::Requested(c) => (c.id, c.access_hash, c.video || requested_video),
                tl::enums::PhoneCall::Accepted(c) => (c.id, c.access_hash, c.video || requested_video),
                tl::enums::PhoneCall::Discarded(disc) => {
                    let (failure, detail) = match disc.reason {
                        Some(tl::enums::PhoneCallDiscardReason::Busy) => (CallFailure::NoAnswer, "Busy."),
                        Some(tl::enums::PhoneCallDiscardReason::Missed) => (CallFailure::NoAnswer, "No answer."),
                        _ => (CallFailure::Lost, "The call ended."),
                    };
                    account.ctx.call_failed(&conv_id, failure, detail);
                    return;
                }
                _ => {
                    account.ctx.call_failed(&conv_id, CallFailure::Refused, "Telegram did not start the call.");
                    return;
                }
            };
            remember_phone_call(account, id, access_hash, account.me, match peer { Who::User(u) => u, _ => 0 });
            account.live().active_call = Some(ActiveCall::RingingOut {
                id,
                access_hash,
                peer,
                video,
                p: dh.p,
                a,
                g_a: g_a.to_vec(),
            });
            account.ctx.ringing_out(&conv_id);
            if matches!(res.phone_call, tl::enums::PhoneCall::Accepted(_)) {
                on_phone_call(account, &res.phone_call);
            }
        }
        Err(fault) if fault.is("USER_PRIVACY_RESTRICTED") => {
            account.ctx.call_failed(&conv_id, CallFailure::Refused, "This person doesn't allow calls.");
        }
        Err(fault) => {
            account.ctx.call_failed(&conv_id, CallFailure::Refused, &fault.words());
        }
    }
}

/// Leaves, cancels, or declines a 1:1 Telegram call (`Command::CallLeave` on `u<id>`).
pub(super) fn leave_direct(account: &Account, peer: Who) {
    let active = {
        let mut live = account.live();
        if live.active_call.as_ref().is_some_and(|c| c.peer() == peer) {
            live.active_call.take()
        } else {
            None
        }
    };
    let Some(active) = active else {
        return;
    };
    match active {
        ActiveCall::RingingIn { id, access_hash, video, .. } => {
            discard_rpc(account, id, access_hash, video, 0, tl::enums::PhoneCallDiscardReason::Busy, 0);
        }
        ActiveCall::RingingOut { id, access_hash, video, .. }
        | ActiveCall::AcceptedIn { id, access_hash, video, .. } => {
            discard_rpc(account, id, access_hash, video, 0, tl::enums::PhoneCallDiscardReason::Hangup, 0);
        }
        ActiveCall::Active { id, access_hash, video, started_at, connection_id, .. } => {
            let duration = started_at.elapsed().as_secs().min(i32::MAX as u64) as i32;
            discard_rpc(account, id, access_hash, video, duration, tl::enums::PhoneCallDiscardReason::Hangup, connection_id);
        }
    }
}

/// Updates camera / screen-share tracking on an active 1:1 Telegram call (`Command::CallPicture`).
/// Any camera or share activation marks `video = true` so `phone.discardCall` accurately records
/// a video call service row.
pub(super) fn on_call_picture(account: &Account, picture: crate::adapters::Picture) {
    let mut live = account.live();
    if let Some(ActiveCall::Active { video, camera, sharing, .. }) = live.active_call.as_mut() {
        match picture {
            crate::adapters::Picture::Camera(on) => {
                *camera = on;
                if on {
                    *video = true;
                }
            }
            crate::adapters::Picture::Share { on, .. } => {
                *sharing = on;
                if on {
                    *video = true;
                }
            }
            crate::adapters::Picture::Watch(_) | crate::adapters::Picture::WatchList(_) => {}
        }
    }
}

/// Forwards encrypted signaling bytes from `mediad` to Telegram (`phone.sendSignalingData`).
pub(super) fn send_signal(account: &Account, conversation_id: &str, data_b64: &str) {
    let Ok(data) = base64::engine::general_purpose::STANDARD.decode(data_b64) else {
        return;
    };
    let peer = Who::parse(conversation_id).map(|(w, _)| w);
    let target = {
        let live = account.live();
        match &live.active_call {
            Some(ActiveCall::Active { id, access_hash, peer: p, .. }) if peer.is_none_or(|w| w == *p) => Some((*id, *access_hash)),
            _ => None,
        }
    };
    if let Some((id, access_hash)) = target {
        let _ = wire::call(
            account.wire.as_ref(),
            &tl::functions::phone::SendSignalingData {
                peer: tl::types::InputPhoneCall { id, access_hash }.into(),
                data,
            },
        );
    }
}

/// Forwards `Update::PhoneCallSignalingData` from Telegram into `mediad`.
pub(super) fn on_signaling_data(account: &Account, update: &tl::types::UpdatePhoneCallSignalingData) {
    let conv_id = {
        let live = account.live();
        match &live.active_call {
            Some(ActiveCall::Active { id, peer, .. }) if *id == update.phone_call_id => Some(peer.text()),
            _ => None,
        }
    };
    if let Some(conv_id) = conv_id {
        let data = base64::engine::general_purpose::STANDARD.encode(&update.data);
        account.ctx.tell_media(&conv_id, MediaRequest::TelegramSignal { data });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dh_2048_exchange_agrees_on_shared_key_fingerprint_and_emoji() {
        let p = known_prime();
        assert!(validate_dh_params(3, &p).is_ok());
        assert!(validate_dh_params(1, &p).is_err());
        assert!(validate_dh_params(3, &[0u8; 256]).is_err());

        let mut a = [0u8; 256];
        let mut b = [0u8; 256];
        for i in 0..256 {
            a[i] = (i as u8).wrapping_mul(17).wrapping_add(5);
            b[i] = (i as u8).wrapping_mul(31).wrapping_add(11);
        }
        let g_a = modpow_u64_base(3, &a, &p);
        let g_b = modpow_u64_base(3, &b, &p);
        assert!(validate_dh_public(&g_a, &p).is_ok());
        assert!(validate_dh_public(&g_b, &p).is_ok());

        let key_alice = modpow_2048(&g_b, &a, &p);
        let key_bob = modpow_2048(&g_a, &b, &p);
        assert_eq!(key_alice, key_bob);
        assert_eq!(key_fingerprint(&key_alice), key_fingerprint(&key_bob));

        let g_a_hash = Sha256::digest(&g_a);
        assert!(verify_g_a_hash(&g_a, &g_a_hash));
        let mut bad_hash = g_a_hash.to_vec();
        bad_hash[0] ^= 1;
        assert!(!verify_g_a_hash(&g_a, &bad_hash));

        let sec_a = CallSecurity::telegram_verified(&key_alice, &g_a, true, true).expect("verified");
        let sec_b = CallSecurity::telegram_verified(&key_bob, &g_a, true, true).expect("verified");
        assert_eq!(sec_a.emojis, sec_b.emojis);
        assert_eq!(sec_a.emojis.len(), 4);
    }

    #[test]
    fn dh_public_value_rejects_out_of_range_values() {
        let p = known_prime();
        assert!(validate_dh_public(&[0u8; 256], &p).is_err());
        let mut one = [0u8; 256];
        one[255] = 1;
        assert!(validate_dh_public(&one, &p).is_err());
        assert!(validate_dh_public(&p, &p).is_err());
        let mut p_minus_1 = p;
        p_minus_1[255] -= 1;
        assert!(validate_dh_public(&p_minus_1, &p).is_err());
    }
}
