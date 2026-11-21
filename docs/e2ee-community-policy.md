# Community Encryption Root Key & Policy

This document specifies the client-side policy format, root-key storage, and member pinning rules
for `GET /v1/e2ee/community` and `PUT /v1/e2ee/community` (TinyComms/tinyclient#294).

## 1. Policy format (`policy` JSON bytes)

The `policy` field sent to `PUT /v1/e2ee/community` is UTF-8 JSON (base64-encoded on the wire):

```json
{
  "v": 1,
  "require_invite_signing": false,
  "require_e2ee_new": false
}
```

- `v`: integer schema version (`1`).
- `require_invite_signing`: when `true`, new devices joining the community should be signed via an
  invite capability key (future enforcement by the server and invite flow).
- `require_e2ee_new`: when `true`, new conversations default to end-to-end encryption.
- Clients ignore unknown fields when reading a policy so newer fields remain forward-compatible.

## 2. Root key and signature

- `root_key`: 65-byte uncompressed NIST P-256 public key (`0x04 || X || Y`), base64-encoded.
- `policy_signature`: ASN.1 DER ECDSA P-256 signature over
  `SHA-256("tiny-e2ee-v1 community-policy" || policy_bytes)`, base64-encoded.
- Updating the policy under the same `root_key` increments `version`.
- Publishing a different `root_key` requires `replace: true`; otherwise the server returns
  `409 root_exists`. Invalid keys or signatures return `400 bad_e2ee`.

## 3. Where the owner's root private key lives

- The root private key is generated on the owner's device (NIST P-256 scalar, PKCS#8 DER).
- It is stored locally under `<support>/e2ee/Tiny-<account>/text/community_root.key` inside the
  account's owner-only (`0700` / owner-only ACL) E2EE directory, sealed with the account's keychain
  `StateKey` (never uploaded to the server, never logged).
- If the owner loses all devices holding the private key, another `manage_server` holder can
  publish a new root with `replace: true`; every member's app will warn about the replacement.

## 4. Member pinning and root-change warnings

Every member's client pins the community root key on first sight in
`<support>/e2ee/Tiny-<account>/text/community_pin.json`:

1. **No root set (`root_key == null`, `version == 0`):** show "No root key set".
2. **First sight (`root_key` present, valid signature, no local pin):** pin `root_key` and
   `version`, and show `Root key set · version N`.
3. **Same root, same or newer version, valid signature:** update pinned `version` silently.
4. **Different root (`pinned.root_key != server.root_key`):** flag `root_changed = true` and
   surface a prominent warning to the member showing both the previous and new root key
   fingerprints until the member explicitly acknowledges and re-pins the new root.
5. **Invalid signature:** mark `signature_valid = false` and warn the member that the server's
   policy signature did not verify.
