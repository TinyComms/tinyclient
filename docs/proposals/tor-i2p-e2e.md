# Proposal: Anonymity transport (Tor, I2P) and overlay end-to-end encryption

Status: **proposal**. Per [CONTRIBUTING.md](../../CONTRIBUTING.md), a choice the product documents leave
open is settled by agreement before the code. This document is the agreement to settle. Nothing in this
document is built: not the Tor or I2P dialer, not overlay MLS for text, and not a direct onion or I2P
network. Call encryption is separate and already in the tree (`mediad`: Discord voice is DAVE, audio; a
Tiny room seals Opus and H.264 when the server offers it). That does not implement this proposal. Claims
about the world outside the repository stay marked *verified* (checked October 2026, source linked at the
bottom) or *unverified* (known, not re-checked).

## 0. Summary

Three features, one foundation:

1. **Tunnel everything** — an opt-in SOCKS5 dialer under every socket chatd opens, so all of an
   account's traffic (messages, media, files, transfers) can go through Tor or an I2P router.
2. **Overlay E2E** — MLS (RFC 9420, OpenMLS) between tinyclient users, riding as ciphertext through
   networks that have no end-to-end encryption of their own (Discord, Slack, Teams, Telegram cloud
   chats, Messages, and Tiny server text), so nobody but the participants — not Discord, not Slack,
   not Microsoft, not that Tiny server — can read or alter a message.
3. **Direct transport** — an eighth network: tinyclient talking to tinyclient over a Tor onion service
   or an I2P destination. No third party carries the bytes. This network is not in `Network::ALL`.
   The seven that are there stay Discord, Slack, Telegram, Teams, Signal, Messages, and Tiny server.

The foundation is one module: a dialer that every adapter asks for its socket. Tor and I2P are two
configurations of it, MLS sits on top of any transport, and the direct adapter is just another pair
of transports under the same MLS groups.

**Everything is the user's choice, per service.** Each account picks its own tunnel — off, a preset
for the system Tor, a preset for an I2P router, or a custom SOCKS5 address — and the choice is
enforced account-whole (rule 10a): no half-tunneled account. Encryption is a per-conversation
decision, taken by an explicit action in the composer, never offered or started automatically. The
effective state of both is one row per account in Settings → Privacy (§3.4).

Honest cost, stated up front: routing Discord or Slack through Tor raises the odds of a captcha or
an account flag, because the exit IP breaks the IP/ASN pattern the networks' anti-abuse systems
weigh. That is why [`07-budgets-and-risk.md`](../product/07-budgets-and-risk.md) §5 rule 10 says
"no proxying" today. This proposal asks to replace that rule, not to ignore it (§3.7).

## 1. Where the project stands (verified in this repository)

- Read against `main` at `f97935e` (release 0.2.36). The v0.2.5 / `docs/windows-and-voice` / PR #59
  checkout this section was written from is history.
- **Proxy today: none, on purpose.** §5 rule 10 of the budgets document ("No proxying, and save
  Slack's `d` cookie when the server reissues it") protects accounts from IP/ASN anomalies.
  [`09-settings.md`](../product/09-settings.md) lists "Proxy (SOCKS5 for Telegram)" under **Later**,
  waits on nothing but the rule. The settings window answers a search for "proxy" with the honest
  line "tinyclient has no proxy setting yet." (`crates/tinyclient/src/gui/settings/rows.rs`,
  `crates/protocol/src/settings.rs` tests use `proxy` as the example of an unknown key.)
- **No plugin system was ever planned.** The architecture has no adapter trait on purpose; an
  adapter is a module under `crates/chatd/src/adapters/` that exports three functions and speaks
  `Command`/`Context` ([architecture.md](../architecture.md), CONTRIBUTING). The ownership table's
  "extension points" are for contributor slices, not for runtime plugins. The only "plugin" in the
  docs is a link to an unrelated third-party project (`product/06-calls.md`). If user-installable
  network plugins are ever wanted, that is a new decision with an ABI to design; this proposal does
  not need one — everything below is ordinary adapter work in-tree.
- **The network layer is two functions and a struct.** `crates/chatd/src/adapters/net.rs` builds one
  `ureq::Agent` (system TLS) and one `Socket` (tungstenite, system TLS). `crates/chatd/src/blobs/fetch.rs`
  has its own agent and its host allow-list. Discord, Slack, Teams, and Messages HTTP in this tree go
  through `net.rs` `agent()` or `blobs/fetch.rs` `agent()`. Tiny does not: `crates/chatd/src/adapters/tiny.rs`
  builds its own `ureq::Agent`. A tunnel has to wrap that agent too. Signal's `reqwest` client is the
  separate case in the Signal bullet.
- **Telegram** runs on grammers; `adapters/telegram/real.rs` already constructs
  `ConnectionParams`, which has a `proxy_url` field behind grammers' `proxy` feature (socks5).
  That is a lead, not a shipped proxy. `crates/chatd/Cargo.toml` pins `grammers-client` 0.10 with
  `default-features = false` and does not enable a `proxy` feature. This checkout does not set a proxy.
- **Signal** runs on presage/libsignal-service; its `PushService` builds an internal
  `reqwest::Client` and exposes no proxy knob (*verified*; a small fork or an upstream patch is
  needed, §3.2).
- **`chat.sqlite` and `media/` are unencrypted at rest** (README, §6 of the budgets document), by
  decision, relying on FileVault/BitLocker/LUKS. Session material in the keychain; Signal's state
  file sealed with AES-256-GCM-SIV under a keychain-held key (`adapters/signal/vault.rs`). New key
  material added by this proposal follows the sealed-file pattern, not a second database.
- The protocol crate is frozen except for optional fields; the settings store already has global
  and per-account scopes, validity-checked keys, and `Change::Settings`.

## 2. Goals, and what each mechanism is for

The three features defend against different adversaries. Keeping them distinct keeps the design
honest:

| The user is afraid of | Mechanism | What it does not do |
| --- | --- | --- |
| Their ISP, café Wi-Fi, or local network seeing *that* and *with whom* they chat | **Tor / I2P transport** | Does not hide the account from Discord/Slack (they see the exit IP and the session), does not encrypt message content beyond the network's own TLS. |
| The network's servers (and anyone who subpoenas or breaches them) reading messages | **Overlay E2E (MLS)** | Does not hide who talks to whom, when, or how much — the network still carries the ciphertext and its metadata. |
| Both at once, plus the network itself being the enemy | **Direct transport over onion/I2P** | Needs both sides online (or a relay, §5); no offline delivery by itself. |

Explicitly out of scope, with reasons:

- **Anonymizing voice calls.** mediad speaks RTP over UDP; Tor carries no UDP and I2P's UDP
  (SAM datagrams) is unusable for real-time audio. Calls stay direct. Discord voice is already
  end-to-end encrypted with DAVE, so the content is protected even on a direct path; the IP is not.
  This is stated in the UI, never silently (a call started while tunneling shows "direct connection").
- **Captcha solving, identity imitation, or any change to how an account identifies** (§5 rules 1–9
  of the budgets document stay exactly as they are).
- **Hiding the user from their own operating system**, or from anyone who has their disk
  (that is FileVault's job, plus §4.6 for what more we seal).

## 3. Transport: everything through Tor and I2P

### 3.1 The tunnel layer

One new module owns every outbound TCP connection of chatd:

```
chatd/src/tunnel/mod.rs      Dialer: Direct | Socks5 {addr, auth, name}
chatd/src/tunnel/socks5.rs   SOCKS5 client handshake (CONNECT; remote DNS is the default)
```

It is small on purpose — a few hundred lines, no new dependency except the `socks` crate ureq
already brings when its `socks-proxy` feature is on (*verified*: ureq 3.4 has `socks-proxy`
supporting `socks4/4a/5`, and its SOCKS connector resolves at the proxy, not locally — the
`socks5h` behaviour Tor needs). Users of the dialer:

| Consumer | Today | Change |
| --- | --- | --- |
| `net::agent()` (Discord, Slack, Teams, Messages REST) | `ureq::Agent` | `.proxy(...)` on the config builder; one agent per tunnel instead of one global. |
| `adapters/tiny.rs` `agent()` | `ureq::Agent` (native TLS, platform roots) | The same proxy wrap. A dialer that only wraps `net.rs` and `blobs/fetch.rs` misses Tiny. |
| `blobs/fetch.rs::agent()` (all media) | `ureq::Agent` | Same; the allow-list in `Hosts::trust` stays in force — the tunnel changes *how* a host is reached, never *which* hosts. |
| `net::Socket::connect` (Discord gateway, Slack socket mode) | `tungstenite::connect` | SOCKS5 CONNECT → TLS wrap (native-tls, same as today) → tungstenite handshake over the wrapped stream. |
| Telegram (`grammers`) | `ConnectionParams` | Enable the `proxy` feature; set `proxy_url` from the account's tunnel. |
| Signal (presage) | internal `reqwest` | Fork pinned in `[patch]` or upstream patch adding a proxy to `PushService::new` (§3.2). |
| mediad | direct UDP | **No change.** Calls stay direct (§2). |

I2P needs nothing extra: an i2pd router exposes a SOCKS5 interface (i2pd 2.61+ also accepts
torsocks-style use and UDP ASSOCIATE, *verified*), so from the dialer's point of view I2P is
"SOCKS5 at `127.0.0.1:4447`, clearnet only through outproxies, `.i2p` hosts native". That asymmetry
matters: **Tor is the tool for reaching the existing networks; I2P is the tool for the direct
transport** (§5), because I2P outproxies are unreliable and Tor onion services are the mature way to
receive inbound connections without a port forward.

Circuit isolation: Tor's `IsolateSOCKSAuth` is on by default — streams that present different
SOCKS5 username/password pairs never share a circuit (verified: `tor(1)`, torspec
socks-extensions). The dialer therefore sends `tinyclient-<network>-<account>` as the username,
which gives each account its own circuits without any Tor-side configuration. Two refinements:

- Since Tor 0.4.9, SOCKS auth also carries a structured isolation token (proposal 351, magic
  prefix `<torS0X>`, format `[30]`: the password field holds the isolation parameter, and a
  non-token username means legacy isolation). A SocksPort may reject legacy isolation. The dialer
  speaks the token format against Tor endpoints and plain RFC 1929 credentials elsewhere
  (i2pd, generic SOCKS5).
- The sign-in webview cannot present credentials on Linux (§3.3), so it gets its own port instead:
  streams from different SocksPorts are always isolated from one another. One no-auth port for
  login, the authenticated default port for everything else.

DNS: the target hostname is sent to the proxy (default in ureq's SOCKS connector and in the
handshake here), so nothing resolves locally. Tor's SOCKS5 carries no `UDP ASSOCIATE` — one more
reason voice stays direct (§2).

### 3.2 Per-network reality

| Network | Reachable over Tor | Cost / risk |
| --- | --- | --- |
| **Telegram** | Yes, first-class. Official clients ship proxy settings; grammers has the `proxy` feature (*verified*). | Lowest. Telegram does not treat proxied clients as anomalous. |
| **Signal** | Yes. Signal Desktop itself takes SOCKS5 via `HTTPS_PROXY`/`WSS_PROXY`/`ALL_PROXY` (merged in Signal-Desktop PR #1855, still current behaviour — *verified*). The phone apps accept only Signal's own TLS proxies, which does not matter here: presage talks to the service the way Desktop does. presage's `PushService` builds its reqwest client without `.no_proxy()`, so the environment variables very likely already apply — **to be tested** as S3's first step — and the explicit per-account setting is preferred over ambient environment variables either way (a one-line upstream patch or a pinned fork). | Low, once the path is explicit. |
| **Discord** | Technically yes (HTTP + gateway both tunnel), practically hard — **verified**: a Discord anti-abuse engineer states logins from Tor exits are "always captcha[ed]" and content from Tor users is weighed "with more stringency" (HN, 2019); users report being unable to complete Discord's captcha through Tor at all (r/TOR, January 2026); Tor Project support pages describe the same for shared exits. Captcha rule 9 then stops the account. | High. The client's honest fingerprint (§5 of budgets) already stands out; an exit IP adds a second anomaly on top. |
| **Slack** | Technically yes. Enterprise anomaly events include `session_fingerprint` and `unexpected_user_agent`; a session that moves IP mid-life is the named anomaly. (*Verified list of event names; whether an exit IP triggers them is unverified.*) | Medium to high, org-dependent. |
| **Teams** | Not offered. Tenant-managed, conditional-access, org-owned. | Out of scope by policy, not by technology. |
| **Messages** | Rides the same dialer (its relay traffic is ureq), but the paired Android phone sits on a clearnet IP anyway; tunneling the desktop leg hides little. | Not advertised. |
| **Direct** (§5) | Native: onion service or I2P destination. | This is where anonymity is real, not best-effort. |

### 3.3 The sign-in webviews

A tunnel under chatd is worthless if the sign-in window bypasses it: the session's first IP would be
the real one. Per platform, in `crates/login`:

- **Windows** (WebView2 through wry): wry already carries a `ProxyConfig` (HTTP and SOCKS5) that it
  turns into `--proxy-server=socks5://host:port` for WebView2, and
  `WebViewBuilderExtWindows::with_additional_browser_args` reaches the same place directly
  (*verified* in wry's source and docs). Chromium resolves DNS locally for plain `socks5://`, so the
  args also carry the host-resolver rule that maps every host except the proxy to NOTFOUND —
  otherwise the login page leaks DNS around the tunnel.
- **Linux** (WebKitGTK through wry): WebKitGTK has the API for this:
  `webkit_website_data_manager_set_network_proxy_settings` with
  `WEBKIT_NETWORK_PROXY_MODE_CUSTOM` and a `WebKitNetworkProxySettings` whose default proxy may be
  a `socks://` URI (*verified*; available since 2.32, the WebContext variant before that). wry may
  not expose the hook, in which case `login` does for Linux what it already does for macOS — a
  couple of direct calls into the WebKitGTK API before the webview is built. One caveat from the
  field (*verified* reports, GNOME Discourse): SOCKS **authentication** through WebKitGTK/libsoup
  has been unreliable — which is exactly why §3.1 gives the login helper its own no-auth SocksPort,
  isolated from the authenticated default port by being a different port.
- **macOS** (WKWebView through objc2): no per-app proxy API exists. Honest answer: with tunneling
  on, macOS sign-in shows "sign-in uses the system network" and offers to wait for a system-level
  route (network location with a SOCKS proxy, or a system VPN). Signing in clearnet and *then*
  switching the session to Tor is allowed but warned about: the IP change is itself a signal.

The sign-in helper reads the tunnel from the same settings store; chatd passes `--proxy ...` to it
as it passes `--at` today.

### 3.4 Settings: the per-service control model

The user decides per service, not globally. The settings store already has global and per-account
scopes with validity-checked keys (verified: `crates/protocol/src/settings.rs`), so the model is:

| Scope | Key | Values | Meaning |
| --- | --- | --- | --- |
| account | `tunnel` | `off` (default) \| `tor` \| `i2p` \| `socks5://[user:pass@]host:port` | This account's transport. The presets resolve to `127.0.0.1:9050` (Tor) and `127.0.0.1:4447` (i2pd); a preset that is unreachable reads `unreachable`, never falls back to direct. |
| account | `e2e.composer` | `never` (default) \| `ask` | Whether the composer of this account shows the lock action that can turn a conversation encrypted (§4.3). It never enables anything by itself. |

No global key. A machine-wide default would turn one preference into six accounts' worth of
changed behaviour at once, which is exactly what rule 10 exists to prevent; if a user wants the
same tunnel everywhere, they set it twice and paste it five times, and the rows say so plainly.

Settings → Privacy shows one row per account, the matrix the user actually reasons about:

| Column | Content |
| --- | --- |
| Account | Network, account name, the account's link state |
| Tunnel | `off` · `connecting` · `on (Tor)` · `on (I2P)` · `on (SOCKS5)` · `unreachable` — what chatd observes, not what was configured |
| Encryption | `native` (Signal, DAVE voice) · `available` (overlay offerable, `e2e.composer: ask`) · `—` (no tinyclient peer known yet) · per-conversation state on the conversation rows themselves |
| Risk | The one plain sentence from §3.2's matrix, per network |

The row is honest in both directions: a tunnel that died is `unreachable` with the account offline,
never a silent fall-back to a clearnet connection; and encryption states are facts about
conversations, not promises about networks. The "has no proxy setting yet" honest line in
`rows.rs` is replaced by these rows when S1 ships; until then the Later entry in
`09-settings.md` moves to "build now".

Embedded Tor (arti) is a later, feature-gated option, not the default: system Tor and i2pd are
already installed by the people who want this, keep chatd small, and keep us out of the
artificial-IP-persistence business. arti-client 0.47 is real and embeddable (*verified*), but it
pulls a tokio runtime of its own into a process whose budget is 10 MB RSS, and the client warns
that embedding arti means following its upgrade cadence or losing the network. A `tor-embedded`
feature behind the same `tunnel` key can come after the dialer proves the seams (§7, S9).

### 3.5 Rule 10, amended

Replace §5 rule 10 ("No proxying") with two rules that keep its intent — account safety — while
allowing an explicit, informed choice:

> 10. **No proxying by default.** A connection to a network goes from the user's own address. An
> account's traffic is tunneled only when the user turned the tunnel on for that account, and
> the account row says, in words, what that does to the account's risk (§3.2). Nothing proxies
> silently, no proxy is shared, no default proxy ships.
> 10a. **A tunnel covers an account whole.** Every socket of an account — REST, websocket, media —
> uses the same tunnel. A half-tunneled account (media clearnet, messages tunneled) is the
> inconsistency that anomaly systems flag, and a leak besides.

The README's privacy table and `product/07-budgets-and-risk.md` §5 carry the same amendment; the
"Later" row for the proxy setting in `09-settings.md` is promoted.

### 3.6 Egress beyond Tor: what else can carry Discord & Co

The dialer speaks SOCKS5, so every egress option is a preset value of the same setting, not new
code. The honest matrix, per option:

| Egress | What it is | For Discord & Co | Anonymity |
| --- | --- | --- | --- |
| **Tor exits** | Shared, public, free | Captcha-challenged, verified (§3.2) | Highest — nobody can tie the exit to you |
| **Self-hosted relay** (VPS with WireGuard + SOCKS5) | Your own static IP | The pragmatic best: a stable, boring IP that is not a known exit; no captcha wall | Low — the VPS provider knows you (payment), the IP is yours. It separates the account from your home IP, it does not anonymize you. |
| **Anonymous-account VPN with SOCKS5** (Mullvad: SOCKS5 on `10.64.0.1:1080` inside the WireGuard tunnel, one static exit IP per proxy, account is a number) | *Verified* (Mullvad help, July 2026): "The amount of CAPTCHAs you will get is often reduced when using the SOCKS5 proxy" — the static proxy IP avoids the rotating-VPN-exit reputation problem. | Good middle path: stable IP, no email on file, kill-switch semantics (tunnel down = traffic stops, never falls back). Requires the VPN app running; from the dialer's chair it is one more `socks5://` address. | Medium — the provider knows the account number and the payment path. |
| **Local circumvention stack** (sing-box / Xray / Hysteria2 giving a local SOCKS5 inbound) | *Verified* as the ecosystem norm: Reality (VLESS), Hysteria2+Salamander, Shadowsocks-2022 etc. are how censored regions reach clearnet; every serious client is "protocol engine → local SOCKS5". | Works like any other preset. tinyclient never implements these protocols — that is an arms race (one field report: even Reality gets fingerprinted on strict whitelist-DPI carriers; Hysteria2 still passed). | Depends entirely on the server and protocol; we do not vouch for any. |
| **I2P outproxies** | i2pd's SOCKS toward clearnet through volunteer outproxies | Not recommended: few outproxies, unreliable, often blocked. I2P is for the direct transport (§5). | High toward `.i2p`, poor toward clearnet. |
| **Generic VPN / WARP** | Changes IP, no SOCKS granularity | Identical to any VPN exit; often flagged similarly | Low-medium; payment or app-account ties it to you |

Two rules this table adds, beyond §3.5's:

> 10b. **The dialer carries no circumvention protocol.** Tor (system or, later, arti), i2pd and any
> sing-box/Xray/Hysteria2 instance are outside chatd, reached as SOCKS5. Embedding an obfuscation
> arms race would break the size budget, the audit story, and the neutrality of the client.
> 10c. **A local tunnel listener is authenticated.** Public post-mortems of 2026 proxy clients
> (unauthenticated local SOCKS inbounds exploited by other apps on the same machine) are the
> warning: anything chatd listens on binds to loopback and requires a credential, and the settings
> URL's credentials are kept in the keychain, not the settings table.

Presets in the UI name these honestly: `tor`, `i2p`, and one free-form `socks5://` whose row says
"traffic leaves wherever this server says" — the client cannot know what a custom endpoint is.

## 4. Overlay end-to-end encryption between users

### 4.1 What the networks already do

| Network | Text | Groups | Files |
| --- | --- | --- | --- |
| Signal | E2E (libsignal, PQXDH) | E2E | E2E |
| Telegram | cloud (TLS to Telegram) | cloud | cloud; secret chats E2E but device-bound and weakly supported in third-party libraries |
| Discord | server-side | server-side; **voice is E2E (DAVE, audio)** | server-side |
| Slack, Teams, Messages | server-side (Messages: phone relay) | server-side | server-side |
| Tiny server | server-side (that server can read the text) | server-side | server-side; a room call seals Opus and H.264 when the server offers `e2ee_calls` |

Servers can read text on Discord, Slack, Teams, Messages, Telegram cloud chats, and Tiny. Signal text is already end-to-end. Discord voice (DAVE, audio) and Tiny room calls are already end-to-end for the media they seal. The overlay in this proposal would cover the readable text. It is not built. It would not touch the networks: the ciphertext would be ordinary message content as far as they are concerned. Teams stays out of the Tor dialer by policy. That policy is not a claim that Teams text is encrypted.

### 4.2 The protocol: MLS, not a hand-rolled ratchet

**Recommendation: MLS (RFC 9420) through OpenMLS, one mechanism for DMs and groups.**

Why MLS over reusing libsignal for the overlay:

- **Store-and-forward is MLS's home turf.** Discord and Slack deliver out of order, batch, and
  sometimes re-deliver. MLS commits and application messages are self-contained and verifiable out
  of order; a Double-Ratchet session assumes ordered delivery and pays per skip. We do not control
  the transport's ordering on any of these networks.
- **DMs and groups are the same object.** A DM is a two-member group (there is even a two-party
  profile draft at the IETF, *verified*). One state machine, one UI story ("this conversation is
  end-to-end encrypted"), one verification model.
- **Audited.** OpenMLS 0.9.0 (August 2026) carries the Sovereign Tech Agency audit by SRLabs; the
  one high finding and all but one low finding are fixed in 8.1+/7.3+ (*verified*).
- **Post-quantum is arriving as a standard**, not a fork: ML-KEM/hybrid cipher suites for MLS are at
  IESG publication as of October 2026 (*verified*). The migration path is a group-level ciphersuite
  bump, which MLS was designed for.
- **libsignal stays in the tree for Signal** (and mediad's DAVE), so we are not adding a second
  implementation of the *same* protocol — we are adding the protocol that fits the overlay's shape.

Cost, honestly: `mediad` already depends on OpenMLS 0.8.1 because `davey` 0.1.5 does, for Discord DAVE and Tiny call encryption. An overlay that wants OpenMLS 0.9.0 (the August 2026 line in the audit note above) is a second version, or an upgrade `davey` has to accept. It is not a new crate. Measure the binary either way. Feature-gate the overlay until that measurement is done. MLS group state
(ratchet trees, secrets) is more state than a DM ratchet. The state is sealed on disk like Signal's
vault.

### 4.3 The envelope: how ciphertext rides an ordinary message

MLS ciphertext is binary, up to a few KB, and every network here has its own text limits and
markup mangling. Two carriers, chosen per message at send time:

1. **Inline** for small messages: the envelope is base64url inside a fixed frame the message text
   carries, e.g. `Ⓐ1<base64>`. The frame is detected before rich-text parsing; a user who types
   that shape themselves gets it escaped (one rule, tested like the span ranges are).
2. **Attachment** for everything else — MLS application messages beyond the inline budget, and
   every file, image or voice note sent under the overlay (§4.5). Binary blob, no mangling, no
   fragmentation, no size games; the networks' existing upload path (SendWith) is reused as is.
   Tiny is the exception already in the client: when the server advertises `file_scan`, the composer
   holds Send until the scan settles ("Scanning that file before it sends."), and a server that
   disallows archives refuses them in the client. Reusing SendWith on Tiny has to include that hold.
   This does not build the overlay.

The **protocol addition is all optional fields**, per the freeze rules:

- `Message.e2e` — `sealed` (stored as the network sent it, rendered from the plaintext cache only),
  `pending_keys` (arrived before the group state was ready; kept, shown as one grey line, never
  auto-deleted), `failed {reason}`.
- `Conversation.e2e` — `on` | `off` | `offered` (we sent our key package; the other side has not
  answered), plus a `fingerprint` for the safety number (§4.4).
- `Capabilities.e2e` — whether the other side's client speaks the overlay (advertised in profile
  presence, so "offer" is only sent to clients that can answer).

chatd stores and relays ciphertexts like any message body; it holds the MLS state **sealed** under
a keychain key, exactly the Signal vault pattern (`e2e/<account>.state`). It never needs the
plaintext to do its job. The window renders plaintext, decrypts nothing itself: decryption happens
in chatd (it owns the sealed state), and the shell's memory of decrypted text is the same memory it
already keeps for ordinary messages.

Encryption is a **per-conversation** decision, taken by the people in it, never by a network-wide
switch: a user may have an encrypted DM on Discord next to a plain one, and Slack groups that never
encrypt. The composer of an account with `e2e.composer: ask` carries a lock action; pressing it
sends the offer (the KeyPackage). That is the only way encryption starts — the rules say nothing is
sent the user did not ask for, and an automatic offer would be exactly that.

Group lifecycle, concretely: an "offer" is one MLS **KeyPackage** sent into the conversation (a
normal message the other client consumes silently). A "turn on" is an MLS **Commit** adding both
members. From then on every message is an MLS application message; who may read is decided by the
group, not by the network. Turning it off is an explicit, confirmed act that processes a removal
commit and says so on the record ("encryption ended by <name> at <time>", stored as a system line).

### 4.4 Verification

MLS credentials carry Ed25519 identity keys; the safety number is the hash of the sorted credential
identities in the group, rendered the way Signal renders one (60 digits, 8 groups, or the emoji
rendering — pick one and keep it everywhere, including the direct transport, §5). Shown from
Settings → conversation header → "Encryption", with:

- `verified` (user compared or scanned the other side's number — QR carries the credential bundle),
- `unverified` (default; shown as "compare before trusting"),
- `changed` — an identity key changed under the same name. MLS makes this visible by construction
  (a new leaf joins the tree); the UI must never smooth it over. This is the one place the overlay
  refuses convenience: a changed identity is announced, the group shows it, and the user re-verifies.

The planned step beyond manual comparison is **Key Transparency**: the IETF keytrans working group's
architecture document is at IESG last call as of October 2026, and its protocol draft is being built
with MLS integration named in the charter (*verified*). KT replaces "compare numbers by hand once"
with an auditable, globally consistent log of identity-key bindings — a server cannot silently swap
a key without the change becoming visible. When the protocol reaches Proposed Standard and an
implementation exists, the overlay adopts it behind the same verification UI; until then, safety
numbers and QR comparison are the mechanism, and the UI must not pretend otherwise.

### 4.5 Files, images, voice notes under the overlay

A file sent in an encrypted conversation is encrypted **before** upload: one content key per file
(AES-256-GCM), the key (wrapped by the MLS group secret, with the file's hash as AAD) travels in
the message that references the file, the bytes upload through the network's ordinary attachment
path — which may itself be tunneled (§3). This is the model of the IETF MLS-attachments draft
(*verified*, draft-sullivan-mls-attachments), kept simpler: no padding policy beyond the existing
thumbnail rules, chunking only where a network caps attachment size, resumable only where the
network already resumes. Sign-out purges the sealed overlay state and the plaintext cache the way
Signal's rows are purged today; the ciphertext blobs on the network's servers stay there and stay
unreadable.

### 4.6 At rest, and what chatd may keep

- Sealed (keychain key, AES-256-GCM-SIV): MLS group state, identity keys, the plaintext message
  cache of encrypted conversations. The plaintext cache follows the same retention as everything else:
  the newest 2,000 messages per conversation, and `keep_days` (7, 30, or 90; the default is 90). The
  file-cache size is a separate setting. The cache dies with sign-out.
- Unsealed: ciphertexts in `chat.sqlite` (they are, from chatd's chair, ordinary message bodies),
  encrypted media in the media cache until rendered — both already covered by the disk-encryption
  footnote in the README, unchanged.
- Full-database encryption (SQLCipher) is **not** part of this proposal. It changes the build
  (bundled SQLCipher vs. bundled SQLite), the license story, and the budgets, and disk encryption
  already covers the threat it answers. Revisit it as its own proposal if that decision changes.

### 4.7 Metadata restraint, and what the feature set is

Encryption hides content; it does not hide behaviour. In an encrypted conversation the network can
still see that you type, when you read, which previews exist. So the overlay ships with a
**strict mode** per encrypted conversation, default on, that is explicit about the trade:

| Signal | Normal | Strict mode |
| --- | --- | --- |
| Typing indicator | Sent (the network sees it) | Suppressed — nothing sent while the draft changes |
| Read marker (seen) | Sent when the rules say so | Suppressed; seen-state comes only from the overlay's own, encrypted seen messages |
| Link previews | The network's | None rendered, none requested |
| Attachment sizes | Actual byte size | Bucketed padding (8/64/512 KB, 4/32/256 MB) before upload |

Turning strict mode off is allowed and says what it gives away; nothing about strict mode is
invisible — the other side's client sees which mode you are in, because it is part of the group
state.

The rest of the feature set, as one list: per-conversation lock with offer/accept; safety number
with emoji rendering and QR verification; identity-change alerts with a hard stop; encrypted
attachments and voice notes (§4.5); disappearing messages as an envelope field plus client-side
deletion — honest label: the other party's disk keeps what their retention keeps, the same promise
Signal makes and with the same limits; one identity across networks (the same MLS key and hence the
same safety number on Discord, Slack and Direct — the person, not the platform, is verified); and
groups whose add/remove actions appear as commit lines on the record. Deliberately not features:
key updates without notice, silent member changes, or any overlay state that moves without a
visible line.

## 5. Direct transport: tinyclient ↔ tinyclient over onion/I2P

An eighth network, `direct`, for conversations that never touch a company's servers at all. It is not in `Network::ALL`. The seven there stay Discord, Slack, Telegram, Teams, Signal, Messages, and Tiny server.

- **Address = identity.** At first run of a direct account, chatd generates one Ed25519 identity
  (the MLS credential, §4.4) and one receive address: a Tor onion service v3 and/or an I2P
  destination, both from sealed keys. The address is a ~56-character string; sharing it is sharing
  a Signal username. Nothing is published anywhere — an onion service needs no directory, an I2P
  destination no outproxy; inbound works behind NAT and firewall by construction.
- **Sessions.** The dialer from §3.1 gains a listener role: `tunnel::serve` accepts on the onion
  service (arti's onion-service feature, or system Tor's control port) / SAMv3 STREAM ACCEPT
  (yosemite crate, *verified* active, sync API fits chatd's thread model). The wire protocol is
  one thing: length-prefixed MLS envelopes — the same envelope §4.3 defines, no second format.
- **Offline delivery.** A direct conversation delivers when both ends are up. The client queues
  outbound MLS messages (they are already store-and-forward objects) and flushes on contact.
  Optional, later: a self-hosted mailbox (run `chatd --mailbox` on a server; it holds ciphertext
  and nothing else, and its own address is just another direct account). This mirrors what Cwtch
  learned on the same ground — metadata-resistant messaging over Tor onion services, with the
  honest label "both online or a relay" (*verified* as prior art).
- **File transfers.** The reason this adapter exists for many people: a 2 GB video goes chunked and
  resumable over the direct session, content-encrypted like §4.5, at whatever the tunnel carries.
  No attachment cap, no CDN, no expiry. Progress rides the existing `Change::Upload` path; the
  conversation row is a normal conversation; the composer is the composer.
- **What it is not:** not a Discord replacement — no presence, no servers, no search beyond the
  local cache, no push notifications when both ends are down. It is a quiet, very hard to observe,
  point-to-point channel inside the same window. The Stories document's rule "a complete
  participant, not an app platform" holds: direct is one more network, judged by the same budgets.

Tor or I2P first? **I2P for throughput and native two-way addressing, Tor for ubiquity.** The dialer
abstraction means the first slice ships onion-only (system Tor required), I2P lands as a second
listener behind the same settings key. Embedded arti (no system Tor) stays optional and late.

## 6. Architecture: what touches what

New code, by the ownership table's rules (new files in own directories, declared from own files;
dependency lines added; frozen files touched only through their documented seams):

| Path | What it is |
| --- | --- |
| `crates/chatd/src/tunnel/*` | The dialer, the SOCKS5 handshake, the listener, per-account state machine (`off → connecting → on → unreachable`), reported through `Context::` notes as account `detail`. |
| `crates/chatd/src/e2e/*` | MLS session lifecycle, envelope encode/decode, sealed state file, safety numbers, offer/commit handling behind `Command::Send`/`SendWith`. |
| `crates/chatd/src/adapters/direct.rs` (+ dir) | The eighth adapter, speaking to `tunnel::serve` and the MLS layer; the demo gets a `direct` variant so the UI is testable without a network. `direct` is not in `Network::ALL`. |
| `crates/protocol` | Optional fields only: `Message.e2e`, `Conversation.e2e`, `Capabilities.e2e`, tunnel state in `AccountStatus`, `Settings` keys' documentation. Older shells read every frame unchanged. |
| `crates/login` | Proxy args per platform (§3.3). |
| `crates/chatd/src/adapters/net.rs`, `blobs/fetch.rs`, `tiny.rs` | Those agents and the websocket connect gain the dialer. Tiny builds its own `ureq::Agent` and has to be wrapped too. These are the seams the freeze rules call extension points; a `// CONTRACT:` note where integrator agreement is needed. |

Dependencies added, all optional-feature-gated: `socks` (already in tree via ureq when enabled),
`yosemite` (I2P SAMv3, slice S8), optionally arti (S9). OpenMLS is already a `mediad` dependency at
0.8.1 for calls. An overlay on 0.9.0 is a second version, or an upgrade `davey` has to accept, not a
new crate.
**No async runtime enters chatd** beyond the one tokio thread grammers already owns; arti, if ever,
gets its own thread like Telegram has.

Budgets, to be measured before each merge per §1 of `07-budgets-and-risk.md`: chatd RSS with a
tunneled account must stay inside its budget (SOCKS5 dialer is bytes); with `e2e` on, measure
OpenMLS's footprint and group-open cost; binary size deltas are stated in the PR, feature-gated
until they pass. The idle budget is untouched: the tunnel adds no timers; the listener blocks on
accept like every other idle wait.

Tests, house style: a local SOCKS5 fake server in `tests/` (like `tests/fixtures/` fakes) proving
every socket class (REST, websocket, media fetch, grammers, presage) actually traverses it; a
no-network `direct` pair in one process exchanging envelopes through two demo accounts; MLS
lifecycle tests against the scripted wire; the login proxy args asserted per platform. `cargo test
--workspace` stays GPU-free.

## 7. Delivery plan

Slices, each shippable alone, smallest first. Every slice updates the docs it makes stale
(README privacy table, `09-settings.md`, `07-budgets-and-risk.md` §5 amendment).

| # | Slice | Ships when |
| --- | --- | --- |
| S1 | Tunnel crate + settings keys + **Telegram** over SOCKS5 (grammers `proxy` feature) | A Telegram account with `tunnel: socks5://…` makes zero non-tunnel connections (verified by the SOCKS fake + connection accounting in tests). Rule 10 amendment merged. |
| S2 | Tunnel under `net.rs` + `blobs/fetch.rs` + `tiny.rs` `agent()` + websocket connect; **Discord and Slack** opt-in with the risk row from §3.2 | Same proof for Discord REST/gateway/media and Slack, and for Tiny's own agent; the per-account state machine shows in the rail. |
| S3 | **Signal**: first prove the environment-variable path presage likely already honors, then make it the explicit per-account key (upstream patch preferred, pinned fork accepted) | The Signal session — attachments included — is tunneled because the row in Settings says so, not because an environment variable leaked in. |
| S4 | Sign-in webviews: Windows args, Linux resolver, macOS warning | Sign-in traffic demonstrably traverses the tunnel on Windows and Linux; macOS shows the honest notice. |
| S5 | **E2E DMs**: OpenMLS, envelope inline carrier, offer/commit, safety number display, `e2e` protocol fields | Two tinyclients on one Discord DM exchange messages Discord cannot read, including out-of-order redelivery; `cargo test --workspace` passes with `e2e` off (default). |
| S6 | **E2E groups** + attachment carrier (files/images/voice through §4.5) | A three-member Discord group encrypted end to end; a 100 MB file round-trips encrypted. |
| S7 | **Direct over onion**: listener, eighth adapter, queued delivery, resumable transfers | Two fresh installs exchange messages and a file with no third-party server in the path (tcpdump in tests proves it). `direct` stays out of `Network::ALL` until that slice. |
| S8 | **I2P** listener (yosemite/SAMv3) and i2pd SOCKS preset | Same proof with an i2pd router in CI-optional tests. |
| S9 | Optional `tor-embedded` (arti) feature | Only if demand shows; budgets re-measured; arti's exit-on-obsolete-consensus behaviour contained in its own thread. |

Sequencing notes: S1–S4 are independent of S5–S6; S7 depends on S5 (it reuses the envelope) and on
S1's dialer; S8 is strictly additive. S2 must not ship before the rule-10 amendment lands — the
docs are the spec.

## 8. Honest limits

- **Discord over Tor is high-risk, verified.** Discord's own anti-abuse engineer: logins from Tor
  exits are always challenged, Tor-origin content is weighed "with more stringency"; users report
  not being able to complete the login captcha through Tor at all (2026). The product says so in
  the account row, ships the switch anyway, and does not bend rules 1–9 to make the tunnel more
  survivable.
- **MLS interop is closed-loop by design** (tinyclient ↔ tinyclient); it cannot interoperate with
  Telegram's or anyone else's encryption. The networks that already have E2E keep theirs; the
  overlay is never stacked on top of Signal's.
- **Metadata is not solved** by the overlay; it is reduced by the direct transport. A tunneled
  Discord conversation still tells Discord a session exists, from an exit IP, at a cadence.
- **Verified-so-far external facts** with dates and sources are in §9; anything marked *unverified*
  needs a check before the slice that relies on it.
- arti's embedding notes (possible `exit(1)` on obsolete consensus; MSRV 1.89; upgrade cadence) are
  the reason it is S9, not S1 (*verified*).

### What "bulletproof" means here, and what it takes

Nothing in this space is bulletproof, and pretending otherwise is how security claims rot. Three
things can never be bulletproofed, only honestly bounded: Discord's and Slack's right to challenge
an account is policy, not cryptography; metadata (that a session exists, its volume, its timing) is
visible to the network that carries it; and direct transport delivers only when both ends — or a
relay — are up. What **can** be held to a bulletproof standard is the code, and that standard is a
checklist, not an adjective:

1. **No hand-rolled crypto.** Every primitive comes from OpenMLS (audited) or the crates it
   audited. One pinned ciphersuite, no algorithm agility, PQ suite adopted as an RFC'd migration,
   never as an option toggle.
2. **The envelope is treated as attacker input.** A deterministic, versioned encoding; the parser
   fuzzed (`cargo-fuzz`) against the same corpus CI runs; malformed input quarantined as one grey
   line, never a panic — the same bar `rich.rs` spans and `input.rs` IME ranges already hold.
3. **Independent review before the word "encrypted" ships.** An external audit of the envelope,
   the group lifecycle and the sealed state file (the OpenMLS and Sovereign Tech pattern, applied
   to our glue), plus published test vectors so another client could interoperate.
4. **No auto-trust anywhere.** New identities start unverified; a changed identity stops the
   conversation's trust on screen until re-verified. (This is stricter than what the Signal adapter
   inherits from presage today — auto-trusting new identity keys is listed in the README as a known
   gap, and the overlay must not copy it.)
5. **Sealed by default.** MLS state, identity keys and plaintext caches sealed under the keychain
   key; sign-out vacuums; retention caps apply to decrypted material exactly as to everything else.
6. **The local surface is defended.** Loopback-only listeners, credentials on every local endpoint
   (§3.6, rule 10c), no ambient environment variables as the interface (§3.2), tunnel state shown
   as observed, never assumed.
7. **Failure stays visible.** An unreachable tunnel is an offline account, never a silent
   clearnet fall-back; a pending key package is one grey line, never a silently dropped message;
   strict mode states what it suppresses.

Each item lands in the slice that touches it (S5's acceptance includes 1, 2 and 4; S1's includes 6)
and the README's privacy table links the checklist.

## 9. Sources (checked 2026-10-20)

- ureq 3.4 — `socks-proxy` feature, proxy-at-DNS default, `Proxy` docs: docs.rs/ureq
- grammers-mtsender 0.10 — `proxy` feature (`tokio-socks`), `ConnectionParams::proxy_url`,
  SOCKS4/HTTP-CONNECT support reverted, SOCKS5 kept: docs.rs, codeberg.org/Lonami/grammers
- Arti 2.7.0 (2026-10-01), arti-client 0.47, onion-service feature status, embedding warnings,
  `arti-ureq`: blog.torproject.org, docs.rs/arti-client, onionservices.torproject.org
  (implementations matrix: vanguards, PoW defense, restricted discovery)
- OpenMLS 0.9.0 (2026-08-25) with SRLabs audit summary; supported cipher suites; X-Wing marked
  experimental: blog.openmls.tech, openmls docs
- MLS standardization state: RFC 9420; draft-ietf-mls-pq-ciphersuites-06 at IESG publication
  (2026-10-19); two-party profile, attachments, opportunistic channels drafts:
  datatracker.ietf.org/wg/mls
- I2P SAMv3 stable; library table (i2p-rs "early development", yosemite active); i2pd 3.0.0
  (2026-10-19) full SAM 3.3, i2pd 2.61 torsocks support in SOCKS proxy, PQ transports:
  i2p.net/en/docs/api/samv3, github.com/PurpleI2P/i2pd (ChangeLog), github.com/eepnet/yosemite
- Mullvad SOCKS5 proxy on WireGuard servers (static exit IP, "reduced CAPTCHAs", reachable only
  inside the tunnel, multihop): mullvad.net/en/help/socks5-proxy (2026-07-30)
- Local circumvention-stack norm and its limits: sing-box docs (Hysteria2), RouteHarden
  "sing-box and Xray architecture" (2026-05), an iOS messenger's field report on Reality being
  fingerprinted by whitelist DPI while Hysteria2 passed (dev.to, 2026-05); 2026 local-unauthenticated-
  SOCKS leak post-mortems (NetGuard security notes)
- Key Transparency: draft-ietf-keytrans-architecture-10 at IESG last call (ends 2026-10-23),
  draft-ietf-keytrans-protocol-05, charter naming MLS/MIMI collaboration, IETF 125/126 minutes on
  MLS-relevant credentials: datatracker.ietf.org/wg/keytrans
- presage/libsignal-service `PushService` internal reqwest client, no proxy knob:
  github.com/whisperfish/presage, whisperfish.github.io (push_service source)
- Signal proxy reality: Signal-Desktop PR #1855 (`HTTPS_PROXY`/`WSS_PROXY`/`ALL_PROXY`, incl.
  socks5) — github.com/signalapp/Signal-Desktop; support.signal.org Proxy Support (TLS proxies on
  Android/iOS; Desktop "exploring options" for that technique); Signal-Android #13045 (the phone
  proxy setting is TLS-proxies only, by a Signal staff member)
- Discord and Tor: Hacker News thread 20791163 — a Discord anti-abuse engineer on always-challenged
  Tor-exit logins (2019); r/TOR "Does Discord no longer run on TOR?" (January 2026); Tor Project
  support, "Why do websites make me solve CAPTCHAs"
- Tor stream isolation: tor(1) (SocksPort flags, `IsolateSOCKSAuth` on by default,
  `PreferSOCKSNoAuth`, per-port isolation, no SOCKS5 UDP ASSOCIATE); torspec
  socks-extensions and path-spec/stream-isolation; proposal 351 (auth extensions, `<torS0X>`
  token format, legacy isolation)
- wry: docs.rs `WebViewBuilderExtWindows::with_additional_browser_args`, and wry's own
  `ProxyConfig` (Http/Socks5) rendering to `--proxy-server` (src/webview2/mod.rs)
- WebKitGTK: `webkit_website_data_manager_set_network_proxy_settings` and
  `WebKitNetworkProxySettings` (socks:// support) — webkitgtk.org reference; GNOME Discourse
  "WebKitGtk proxy settings" (SOCKS auth unreliability)
- Signal CLI proxy history (torsocks workaround, no first-class API in 2020): signal-cli #261 —
  superseded for our purposes by the reqwest path above

## 10. Decisions to settle before code

1. **Rule 10 amendment** — accept "opt-in, per account, honestly labelled" as the replacement?
   (Recommendation: yes; without it the transport work has no spec to satisfy.)
2. **MLS/OpenMLS as the overlay** (vs. libsignal reuse) — recommendation §4.2: MLS.
3. **Envelope frame** — the `Ⓐ1…` inline marker (or a different constant); attachment carrier for
   large payloads. Decide the constant before S5; it is forever.
4. **Scope of the first PR series** — S1+docs first (small, self-contained, Telegram-only), or
   S1+S2 together? Recommendation: S1 alone; it proves the settings model and the amendment.
5. **Teams and Messages stay untunneled** — confirm the policy call, not just the technical one.
6. **Direct transport: onion first, I2P second** — confirm, since it fixes S7/S8's order.
7. **Signal: explicit setting vs. environment variables.** Test first whether presage's reqwest
   honors `ALL_PROXY` today; even if it does, ship the explicit per-account key and treat the
   environment as a fallback, not the interface (§3.2).
8. **`e2e.composer` default.** `never` ships as the default everywhere (§3.4). Whether the S5
   release note suggests users turn it on per account, or the docs merely show it, is a product
   voice decision — recommendation: docs only; tinyclient never nudges.
