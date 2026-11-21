//! Community-wide E2EE root key and signed policy (`GET`/`PUT /v1/e2ee/community`).
//!
//! See `docs/e2ee-community-policy.md` for the policy format, key storage, and pinning rules.

use super::super::{asked, Saved};
use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use p256::ecdsa::signature::hazmat::{PrehashSigner, PrehashVerifier};
use p256::ecdsa::{DerSignature, SigningKey, VerifyingKey};
use p256::pkcs8::{DecodePrivateKey, EncodePrivateKey};
use protocol::{CommunityPolicy, CommunityRootStatus};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;

const DOMAIN: &[u8] = b"tiny-e2ee-v1 community-policy";
const PIN_FILE: &str = "community_pin.json";
const KEY_FILE: &str = "community_root.pkcs8";

pub(crate) const ROOT_EXISTS: &str =
    "A community root key is already set. Use Replace root to replace it — every member will be warned.";
pub(crate) const BAD_E2EE: &str =
    "The server rejected the community encryption key or policy signature.";

/// Wire payload stored inside `policy` (UTF-8 JSON, schema v1).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PolicyWire {
    #[serde(default = "policy_v1")]
    v: u32,
    #[serde(default)]
    require_invite_signing: bool,
    #[serde(default)]
    require_e2ee_new: bool,
}

fn policy_v1() -> u32 {
    1
}

/// What `GET /v1/e2ee/community` and `PUT /v1/e2ee/community` return.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct RemoteCommunity {
    #[serde(default)]
    pub root_key: Option<String>,
    #[serde(default)]
    pub policy: Option<String>,
    #[serde(default)]
    pub policy_signature: Option<String>,
    #[serde(default)]
    pub version: u64,
    #[serde(default)]
    pub updated_at: Option<String>,
}

/// Locally pinned community root key on this device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PinnedRoot {
    /// Base64 of the 65-byte uncompressed P-256 public key.
    pub root_key: String,
    pub version: u64,
}

/// Compute SHA-256(`"tiny-e2ee-v1 community-policy" || policy_bytes`).
fn policy_digest(policy_bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(DOMAIN);
    hasher.update(policy_bytes);
    hasher.finalize().into()
}

/// Short human-readable fingerprint of a 65-byte root public key.
pub(crate) fn root_fingerprint(root_key_bytes: &[u8]) -> String {
    let hash: [u8; 32] = Sha256::digest(root_key_bytes).into();
    let hex: String = hash[..8].iter().map(|b| format!("{b:02X}")).collect();
    hex.as_bytes()
        .chunks(4)
        .map(|chunk| std::str::from_utf8(chunk).unwrap_or(""))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Encode `CommunityPolicy` as canonical v1 JSON bytes.
pub(crate) fn encode_policy(policy: &CommunityPolicy) -> Vec<u8> {
    let wire = PolicyWire {
        v: 1,
        require_invite_signing: policy.require_invite_signing,
        require_e2ee_new: policy.require_e2ee_new,
    };
    serde_json::to_vec(&wire).unwrap_or_else(|_| b"{}".to_vec())
}

/// Decode `policy` JSON bytes into `CommunityPolicy`, ignoring unknown fields.
pub(crate) fn decode_policy(bytes: &[u8]) -> CommunityPolicy {
    serde_json::from_slice::<PolicyWire>(bytes)
        .map(|w| CommunityPolicy {
            require_invite_signing: w.require_invite_signing,
            require_e2ee_new: w.require_e2ee_new,
        })
        .unwrap_or_default()
}

/// Generate a fresh P-256 signing key.
pub(crate) fn generate_root_key() -> Result<SigningKey, String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|err| format!("random: {err}"))?;
    SigningKey::from_bytes((&bytes).into()).map_err(|err| format!("key: {err}"))
}

/// 65-byte uncompressed SEC1 public key for a P-256 signing key.
pub(crate) fn public_key_bytes(signing: &SigningKey) -> Vec<u8> {
    signing.verifying_key().to_encoded_point(false).as_bytes().to_vec()
}

/// Sign `policy_bytes` with DER ECDSA P-256 over SHA-256(`DOMAIN || policy_bytes`).
pub(crate) fn sign_policy(signing: &SigningKey, policy_bytes: &[u8]) -> Result<Vec<u8>, String> {
    let digest = policy_digest(policy_bytes);
    let sig: DerSignature = signing
        .sign_prehash(&digest)
        .map_err(|err| format!("sign: {err}"))?;
    Ok(sig.as_bytes().to_vec())
}

/// Verify a DER ECDSA P-256 signature over SHA-256(`DOMAIN || policy_bytes`).
pub(crate) fn verify_policy(root_key_bytes: &[u8], policy_bytes: &[u8], sig_bytes: &[u8]) -> bool {
    if root_key_bytes.len() != 65 {
        return false;
    }
    let Ok(verifying) = VerifyingKey::from_sec1_bytes(root_key_bytes) else {
        return false;
    };
    let Ok(sig) = DerSignature::from_bytes(sig_bytes) else {
        return false;
    };
    let digest = policy_digest(policy_bytes);
    verifying.verify_prehash(&digest, &sig).is_ok()
}

/// Check `remote` against `pinned`, updating `pinned` on first sight or on a newer version under
/// the same root key. When a previously pinned root is replaced by a different key, `pinned` is
/// left unchanged and `root_changed = true` is returned so every member is warned.
pub(crate) fn evaluate_pin(
    pinned: &mut Option<PinnedRoot>,
    remote: &RemoteCommunity,
    has_local_key: bool,
) -> (CommunityRootStatus, bool) {
    let Some(root_b64) = remote.root_key.as_deref().filter(|s| !s.is_empty()) else {
        return (
            CommunityRootStatus {
                set: false,
                version: remote.version,
                has_local_key,
                ..CommunityRootStatus::default()
            },
            false,
        );
    };
    let root_bytes = STANDARD.decode(root_b64).unwrap_or_default();
    let policy_bytes = remote
        .policy
        .as_deref()
        .and_then(|s| STANDARD.decode(s).ok())
        .unwrap_or_default();
    let sig_bytes = remote
        .policy_signature
        .as_deref()
        .and_then(|s| STANDARD.decode(s).ok())
        .unwrap_or_default();
    let signature_valid = verify_policy(&root_bytes, &policy_bytes, &sig_bytes);
    let policy = decode_policy(&policy_bytes);
    let fingerprint = if root_bytes.is_empty() {
        String::new()
    } else {
        root_fingerprint(&root_bytes)
    };

    let mut dirty = false;
    let mut root_changed = false;
    let mut previous_fingerprint = String::new();

    match pinned {
        None => {
            if signature_valid {
                *pinned = Some(PinnedRoot {
                    root_key: root_b64.to_owned(),
                    version: remote.version,
                });
                dirty = true;
            }
        }
        Some(existing) if existing.root_key == root_b64 => {
            if signature_valid && remote.version > existing.version {
                existing.version = remote.version;
                dirty = true;
            }
        }
        Some(existing) => {
            root_changed = true;
            let prev_bytes = STANDARD.decode(&existing.root_key).unwrap_or_default();
            previous_fingerprint = root_fingerprint(&prev_bytes);
        }
    }

    (
        CommunityRootStatus {
            set: true,
            version: remote.version,
            fingerprint,
            updated_at: remote.updated_at.clone().unwrap_or_default(),
            policy,
            signature_valid,
            root_changed,
            previous_fingerprint,
            has_local_key,
        },
        dirty,
    )
}

fn load_pin(dir: &Path) -> Option<PinnedRoot> {
    let bytes = std::fs::read(dir.join(PIN_FILE)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn save_pin(dir: &Path, pin: &PinnedRoot) {
    if let Ok(bytes) = serde_json::to_vec(pin) {
        let _ = std::fs::write(dir.join(PIN_FILE), bytes);
    }
}

fn load_key(dir: &Path) -> Option<SigningKey> {
    let bytes = std::fs::read(dir.join(KEY_FILE)).ok()?;
    SigningKey::from_pkcs8_der(&bytes).ok()
}

fn save_key(dir: &Path, key: &SigningKey) -> Result<(), String> {
    let der = key.to_pkcs8_der().map_err(|err| format!("encode key: {err}"))?;
    std::fs::write(dir.join(KEY_FILE), der.as_bytes()).map_err(|err| format!("save key: {err}"))
}

/// Fetch `GET /v1/e2ee/community` and evaluate/update this device's local pin.
/// Returns `None` if the server does not support the route (e.g. 404).
pub(crate) fn read_status(saved: &Saved, dir: &Path) -> Option<CommunityRootStatus> {
    let (200, body) = asked("GET", saved, "/v1/e2ee/community", None).ok()? else {
        return None;
    };
    let remote: RemoteCommunity = serde_json::from_value(body).ok()?;
    let mut pinned = load_pin(dir);
    let has_local_key = dir.join(KEY_FILE).exists();
    let (status, dirty) = evaluate_pin(&mut pinned, &remote, has_local_key);
    if dirty {
        if let Some(pin) = pinned.as_ref() {
            save_pin(dir, pin);
        }
    }
    Some(status)
}

/// Acknowledge a replaced community root key by re-pinning what the server currently serves.
pub(crate) fn accept_root(saved: &Saved, dir: &Path) -> Result<(), String> {
    let (200, body) = asked("GET", saved, "/v1/e2ee/community", None)? else {
        return Err("Could not read the community encryption record.".to_owned());
    };
    let remote: RemoteCommunity =
        serde_json::from_value(body).map_err(|_| "Could not read the community encryption record.".to_owned())?;
    let Some(root_b64) = remote.root_key.filter(|s| !s.is_empty()) else {
        return Err("No community root key is set.".to_owned());
    };
    save_pin(dir, &PinnedRoot { root_key: root_b64, version: remote.version });
    Ok(())
}

/// Map server error responses from `PUT /v1/e2ee/community` to clear user-facing words.
pub(crate) fn explain_put_error(status: u16, body: &Value) -> String {
    let code = body.get("error").and_then(Value::as_str).unwrap_or("");
    match (status, code) {
        (409, "root_exists") | (409, _) => ROOT_EXISTS.to_owned(),
        (400, "bad_e2ee") | (400, _) => BAD_E2EE.to_owned(),
        (403, _) => "You don't have permission to change the community encryption policy.".to_owned(),
        _ => format!("The server refused the community encryption update ({status})."),
    }
}

/// Publish or update the community's root key and signed policy (`PUT /v1/e2ee/community`).
pub(crate) fn publish_policy(
    saved: &Saved,
    dir: &Path,
    policy: &CommunityPolicy,
    replace: bool,
) -> Result<(), String> {
    let signing = if replace {
        generate_root_key()?
    } else if let Some(existing) = load_key(dir) {
        existing
    } else {
        generate_root_key()?
    };
    let root_bytes = public_key_bytes(&signing);
    let policy_bytes = encode_policy(policy);
    let sig_bytes = sign_policy(&signing, &policy_bytes)?;
    let mut payload = json!({
        "root_key": STANDARD.encode(&root_bytes),
        "policy": STANDARD.encode(&policy_bytes),
        "policy_signature": STANDARD.encode(&sig_bytes),
    });
    if replace {
        payload["replace"] = json!(true);
    }
    match asked("PUT", saved, "/v1/e2ee/community", Some(payload))? {
        (200, body) => {
            save_key(dir, &signing)?;
            let remote: RemoteCommunity = serde_json::from_value(body).unwrap_or_default();
            if let Some(root_b64) = remote.root_key.filter(|s| !s.is_empty()) {
                save_pin(dir, &PinnedRoot { root_key: root_b64, version: remote.version });
            }
            Ok(())
        }
        (status, body) => Err(explain_put_error(status, &body)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signed_remote(signing: &SigningKey, policy: &CommunityPolicy, version: u64) -> RemoteCommunity {
        let root_bytes = public_key_bytes(signing);
        let policy_bytes = encode_policy(policy);
        let sig_bytes = sign_policy(signing, &policy_bytes).unwrap();
        RemoteCommunity {
            root_key: Some(STANDARD.encode(&root_bytes)),
            policy: Some(STANDARD.encode(&policy_bytes)),
            policy_signature: Some(STANDARD.encode(&sig_bytes)),
            version,
            updated_at: Some("2026-11-21T00:00:00Z".into()),
        }
    }

    #[test]
    fn pin_first_sight_same_root_update_and_replacement_warning() {
        let key_a = generate_root_key().unwrap();
        let key_b = generate_root_key().unwrap();
        let mut pinned = None;

        // 1. Unset server record.
        let (unset, dirty) = evaluate_pin(&mut pinned, &RemoteCommunity::default(), false);
        assert!(!unset.set);
        assert!(!unset.root_changed);
        assert!(!dirty);
        assert!(pinned.is_none());

        // 2. First sight pins the root key.
        let p1 = CommunityPolicy { require_invite_signing: false, require_e2ee_new: true };
        let r1 = signed_remote(&key_a, &p1, 1);
        let (s1, dirty) = evaluate_pin(&mut pinned, &r1, true);
        assert!(s1.set && s1.signature_valid && !s1.root_changed);
        assert_eq!(s1.version, 1);
        assert_eq!(s1.policy, p1);
        assert!(dirty);
        assert_eq!(pinned.as_ref().unwrap().version, 1);

        // 3. Same root with a new policy version updates silently without warning.
        let p2 = CommunityPolicy { require_invite_signing: true, require_e2ee_new: true };
        let r2 = signed_remote(&key_a, &p2, 2);
        let (s2, dirty) = evaluate_pin(&mut pinned, &r2, true);
        assert!(s2.set && s2.signature_valid && !s2.root_changed);
        assert_eq!(s2.version, 2);
        assert_eq!(s2.policy, p2);
        assert!(dirty);
        assert_eq!(pinned.as_ref().unwrap().version, 2);

        // 4. Different root triggers root_changed warning and preserves the old pin until accepted.
        let r3 = signed_remote(&key_b, &p2, 3);
        let (s3, dirty) = evaluate_pin(&mut pinned, &r3, false);
        assert!(s3.set && s3.signature_valid && s3.root_changed);
        assert!(!s3.previous_fingerprint.is_empty());
        assert_ne!(s3.previous_fingerprint, s3.fingerprint);
        assert!(!dirty);
        assert_eq!(pinned.as_ref().unwrap().version, 2);
    }

    #[test]
    fn bad_signature_is_flagged_and_not_pinned() {
        let key_a = generate_root_key().unwrap();
        let mut remote = signed_remote(&key_a, &CommunityPolicy::default(), 1);
        remote.policy_signature = Some(STANDARD.encode(b"not-a-valid-der-sig"));
        let mut pinned = None;
        let (status, dirty) = evaluate_pin(&mut pinned, &remote, false);
        assert!(status.set);
        assert!(!status.signature_valid);
        assert!(!dirty);
        assert!(pinned.is_none());
    }

    #[test]
    fn error_wording_covers_root_exists_and_bad_e2ee() {
        assert_eq!(explain_put_error(409, &json!({"error": "root_exists"})), ROOT_EXISTS);
        assert_eq!(explain_put_error(400, &json!({"error": "bad_e2ee"})), BAD_E2EE);
    }
}
