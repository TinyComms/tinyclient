# 08. Getting in and staying in

Sign-in for the networks, first run, accounts, settings, sign-out, packaging. Researched
2026-10-13. **[V]** = checked against a source this session (listed at the end). **[U]** = not
verified; from memory or a single report, test before relying on it.

## Verdict per network

| Network | How you sign in | "Anyone can sign in"? | Evidence |
| --- | --- | --- | --- |
| Discord | Web login page in `tinyclient-login`; token read from the page. Captcha, 2FA and passkeys are the page's own. | Yes. Works today against a real account. | Repo state. Token lasts until password change or remote logout [U]. |
| Slack | Web login page; one `d` cookie for the whole browser session plus one `xoxc` token per workspace. | Yes for ordinary workspaces. Enterprise Grid is at risk. | "Each workspace has its own token, the cookie is the same for all" [V]. One 2026 report: on Grid, `localConfig_v2.teams` is `{}` when signed in and tokens rotate within hours [V that the report exists, U that it is general]. Our probe reads exactly that key. |
| Telegram | Native: QR code scanned from the phone, or phone number, code, optional 2FA password. No web page. | Yes. The only sanctioned one. | API terms welcome third-party clients; one `api_id` per app, registered by the owner, shipped in the binary [V]. |
| Teams, personal account | Microsoft login page; client id `8ec6bc83-…`, tenant `consumers`, skype token from `teams.live.com/api/auth/v1.0/authz/consumer`. | Probably yes. | Constants from purple-teams `teams_login.c` [V]. Not run by us [U]. |
| Teams, work or school | Microsoft login page, signing in as Teams' own client id `1fec8e78-…`, scope `api.spaces.skype.com`, skype token from `teams.microsoft.com/api/authsvc/v1.0/authz`, push over trouter. | **No.** Expected to work in tenants with default policy [U, not run by us]. Fails wherever the tenant requires a managed or compliant device, an approved app, or blocks the flow. The user cannot fix that; only their admin can. | purple-teams issues #28 (conditional access block, open since 2022) and #78 (AADSTS 53003, Apr 2026, 27 comments, fixed Jun 2026 by adding request headers and a browser user agent) [V]. Microsoft now ships a managed policy that blocks device-code flow [V]. |
| Messages (Android) | Native QR. The phone scans it from Messages › Device pairing. No Google account in this app, and no phone number to type. | The pairing itself is what messages.google.com does [U, not run by us against a phone]. Google does not publish the protocol and can change it. The phone stays the account and has to stay online. | Field layout of the Bugle relay as published by third-party clients [V for the field numbers used in code]. A scripted phone covers the adapter. A real pair has not been run [U]. |

### Telegram, in detail

- **Terms that bind the product [V]:** own `api_id` (the sample id in Telegram's source returns
  `API_ID_PUBLISHED_FLOOD`); the app name must not contain "Telegram" (ours does not) and must not
  use its logo as the app icon; a client must not tamper with read statuses, typing statuses,
  online/last-seen, or self-destructing content; a client that shows channels must show official
  sponsored messages; breach gets 10 days' notice, then the `api_id` is cut off for every user.
  So the "send typing / read receipts" toggles cannot apply to Telegram: a ghost mode there
  risks every user's access, not one account.
- **`api_id` in an open-source app:** clients embed their own id and hash in the shipped binary;
  it is extractable and Telegram knows that [U]. Keep it out of the public repo (build-time
  secret) so forks register their own, as the terms require of them [V].
- **TDLib:** no official prebuilt for macOS; it is built from source (CMake, OpenSSL, gperf).
  Community prebuilts exist (`prebuilt-tdlib` on npm, `tdjson` on PyPI: 14.2 MB compressed for
  macOS arm64) [V]. Uncompressed dylib about 25 to 35 MB and idle memory 20 to 50 MB [U, measure
  it]. It keeps its own encrypted SQLite, so Telegram messages would be cached twice unless
  chatd treats TDLib's database as the cache. It gives QR login, 2FA, update gap recovery,
  secret chats and file transfer, maintained by Telegram.
- **grammers (pure Rust MTProto):** GitHub repo archived 2026-02-10, moved to Codeberg; one
  maintainer [V]. A few MB linked, no second database, no C++ toolchain. No secret chats, no
  calls, and every Telegram layer change waits on that maintainer [U on current feature set].
- **Recommendation:** TDLib, loaded only when a Telegram account exists. Measure idle RSS first;
  if it more than doubles chatd, run it as a process that exists only while Telegram is signed in.
- **QR first:** no typing, no SMS. Phone number is the fallback. Creating an account is out.

### Teams, in detail

- **What works:** the Teams client's own chat service. An Entra access token for
  `api.spaces.skype.com` is exchanged for a skype token; messages go through the regional
  `*.ng.msg.teams.microsoft.com` chat service; new messages arrive over trouter (websocket).
  purple-teams does this and is active (issues filed Sep 2026) [V]. It has no calls; it links to
  the web client, which matches our hand-off rule in `stories.md`.
- **Sign-in shape:** show Microsoft's real login page in `tinyclient-login` (MFA, passkeys and
  federated SSO are the page's own), using the authorization-code flow with the native-client
  redirect that purple-teams' source also carries [U that it still works], not device code,
  which is the flow tenants are blocking. A native-client
  refresh token lasts 90 days rolling; a token lifted out of the web app is a single-page-app
  token that dies in 24 hours [V for both lifetimes], so reading the session out of
  `teams.microsoft.com` the way we do for Slack would mean signing in every day. Do not do that.
- **What stops it:** conditional access. A WKWebView in an unknown app has no device identity, so
  "require compliant device" fails even with correct credentials [U for WKWebView specifically].
  The user sees Microsoft's own block page. tinyclient must then say in words: "Your organization
  doesn't allow this app. Teams stays in the Teams app." and stop offering to retry.
- **Graph, the official route, does not rescue this.** Delegated `Chat.Read` does not itself
  need admin consent [V], but: personal accounts are "Not supported" [V]; there is no push to a
  desktop app, because change notifications need a public HTTPS endpoint and the all-chats
  subscription is application-only [V], so it would be polling; it needs our own multi-tenant app
  registration, and tenants that restrict user consent block it just as they block the other
  route [U on how common]; the same conditional access policies apply to it.
- **Guest tenants:** one account can sit in several tenants, each its own token set
  (purple-teams has an open bug past two [V]). Treat a tenant like a Slack workspace.

### Slack model

One sign-in window, one `d` cookie, every workspace that browser session holds. Each workspace
is an account under Slack with its own token, connection, unread count and sign-out. The rail
keeps **one** Slack button (the rail is 72 px; ten workspaces would take it over); its total is
the sum. Inside the Slack filter the list is sectioned by workspace, and in "All chats" a row's
second line carries the workspace name only when the person has more than one. An Enterprise
Grid org is one account whose channels span its workspaces. "Add another workspace" reopens the
window on the same cookie. SSO is the page's own; the user agent fix already exists for Google
and Okta.

## Stories

| # | P | Story | Observable check |
| --- | --- | --- | --- |
| 1 | P0 | I double-click tinyclient and it works. I never start a daemon. | With no chatd running, launching the app yields a `status` answer on the socket within 1 s. |
| 2 | P0 | On first launch I see one screen: four networks, each with a Sign in button, nothing else to configure. | Fresh data dir: `status` reports four networks `SignedOut`; the window shows four buttons and no list. |
| 3 | P0 | I sign in to one network and see real conversations without restarting. | After the login window closes, `list` is non-empty within 5 s and the first open returns messages. |
| 4 | P0 | Networks I have not added stay out of my way: a single "+" in the rail, not three dead icons. | With only Discord signed in, the rail has All chats, Discord, and "+". |
| 5 | P0 | I close the sign-in window and nothing changes. | Login exits 1; `status` is identical to before; no keychain item is written. |
| 6 | P0 | I sign in to Slack once and get all my workspaces. | With a browser session holding N workspaces, `status` lists N Slack accounts after one window. |
| 7 | P0 | When a network signs me out, I am told which one, in words, with one button to sign back in. My cached messages stay readable. | Revoke the session remotely: link becomes `SignedOut`, a banner names the account, `open` still returns cached rows, `send` refuses with a reason. |
| 8 | P0 | I sign out of one account and it is gone from this Mac and from the network's device list. | After `sign_out`: no keychain item for it, no rows for it in SQLite, files for it deleted, and the network's own logout was called (best effort). Other accounts untouched. |
| 9 | P0 | I sign in to Telegram by scanning a QR code with my phone. | QR shown within 2 s of pressing Sign in; after the scan `status` shows Telegram online; a 2FA account is asked for its password in a native field. |
| 10 | P0 | I install from a disk image and open it without a security warning. | A downloaded copy passes `spctl --assess` and opens on a Mac that has never run it. |
| 11 | P0 | I get notifications that are named tinyclient, and I am never asked for my keychain password after an update. | Notification shows the app name and icon; replacing the app with the next version produces no keychain dialog. |
| 12 | P1 | I sign in to Telegram with phone number and code when my phone is not nearby. | Number, code, optional password; wrong code says so and lets me retry without restarting. |
| 13 | P1 | I sign in to Teams with a personal or an unrestricted work account. | `status` shows Teams online; a message sent from the real Teams app appears without polling. |
| 14 | P1 | If my organization blocks tinyclient, I am told that plainly and not asked to try again. | A tenant with a compliant-device policy ends in the "organization doesn't allow this app" state, not a spinner or a loop. |
| 15 | P1 | tinyclient keeps receiving while its window is closed, and starts when I log in. | Quit the window: chatd is still up and notifications arrive. Toggle in Settings appears in System Settings, Login Items. |
| 16 | P1 | I open Settings with Cmd-comma and see every account, its state, and Sign out. | The account list matches `status`; Sign out here is the same action as story 8. |
| 17 | P1 | I can see how much disk tinyclient uses and clear it without signing out. | Size shown matches the data dir within 5%; after Clear, sessions remain and the list refills from the network. |
| 18 | P1 | tinyclient updates itself. | An older build offers the new version and relaunches into it with sessions intact. |
| 19 | P1 | I add a Slack workspace that was not in my browser session. | "Add workspace" yields one more Slack account; existing ones do not reconnect. |
| 20 | P2 | I use two Discord or Telegram accounts, or a Teams account that is a guest in other tenants. | Two accounts on one network in `status`, each with its own unread. |
| cut | | Creating an account on any network; pasting a token; importing a session from Chrome or the official app; a risk-acknowledgement screen (the README carries it). | |

## Session lifetime

| Network | Ends when | What I see |
| --- | --- | --- |
| Discord | Password change, "log out all devices", or Discord's own lock [U]. A sign-in from a new location may need an emailed approval first; the login page says so and I approve it in my mail, then press Log in again. | Story 7. |
| Slack | Workspace session-duration policy set by its admins [U on the range], SSO re-auth, or remote sign-out. One workspace can expire while the others stay up. | Story 7, naming the workspace. Re-sign-in must replace that workspace's token only. |
| Telegram | I terminate it from another device, or Telegram's inactivity timer (6 months by default, user-set) [U]. | Story 7. New sign-in also raises a login alert in my other Telegram apps; expected. |
| Teams | Refresh token unused for 90 days, password change, admin revocation, or a tenant sign-in-frequency policy [U on typical values]. The skype token is short and renewed silently [U on its life]. | Story 7. If renewal fails on policy, story 14. |

## Multiple accounts

In as a data model now: sessions, store rows and adapters keyed by `(network, account)`. Slack
needs it on day one; `session.rs` keys the keychain by network label alone, and a rejection in
`keep_connected` forgets the whole network, so one expired workspace would sign out the rest.
As a visible feature: Slack workspaces P0; Teams tenants and a second Discord or Telegram
account P2. `stories.md` should say "cut, except Slack workspaces".

## Settings

One window, four sections. Nothing here needs a restart.

- **Accounts.** Each account: name, state in words, Sign out. "+" to add.
- **Notifications.** All messages, or direct messages and mentions (default). Show message text
  in the banner, on by default. Sound on or off. Per-conversation mute lives on the conversation.
- **Appearance.** Light, dark, match system (default). Text size: three steps.
- **General.** Open at login (default on, asked once after the first sign-in). Cache: size, Clear.
  Send typing indicators and Send read receipts: two switches, labelled "Slack, Discord, Teams",
  with one line under them saying Telegram does not allow a client to turn these off.

Deliberately not settings: theme colors, per-network appearance, font choice, proxy, a
notification rules editor, custom keybindings, the cache location, an "experimental" tab, and
anything that hides presence on Telegram.

## Packaging

- **A signed, notarized `tinyclient.app`** holding the shell, `chatd`, `mediad` and
  `tinyclient-login`, shipped as a disk image. Without a Developer ID ($99 a year) and
  notarization, a downloaded copy is blocked until the person allows it in System Settings.
- **Why the bundle matters for behavior, not looks.** Notifications need a bundle identifier; a
  bare binary cannot post one with our name and icon. Keychain items are tied to the code
  signature of the process that wrote them: with a stable Developer ID signature chatd reads its
  sessions silently forever; with an unsigned or ad-hoc build, every new build is a new identity
  and macOS asks for the login password per item. Login Items shows the bundle's name and icon.
- **Open at login** registers chatd as a login agent from inside the bundle (`SMAppService`,
  macOS 13+), so it can be switched off in System Settings and goes when the app is deleted.
- **Updates:** Sparkle, signed, checked daily, installed on quit; the update restarts chatd and
  sessions survive in the keychain. Not the Mac App Store (review rules out three of the four
  sign-ins). Apple silicon first.

## Open questions for the owner

1. Work Teams will not work for some people and nothing in the client can change that. Ship it
   as "works unless your organization blocks it", or hold it until run against real tenants?
2. TDLib is the safe choice and may triple chatd's footprint; grammers is light and rests on one
   maintainer. Is 12 MB a hard number, or is "Telegram costs extra only if you use it" fine?
3. Will you pay for an Apple Developer ID and register the Telegram `api_id` under your own name
   and phone number? Both tie the app to you, and both are required for "anyone can install it".
4. Signing in to Teams as Microsoft's own client id presents tinyclient as a first-party app, a
   step past what we do for Slack and Discord. Accept that, or limit Teams to personal accounts?
5. Slack Enterprise Grid: if page storage no longer holds the token, the fallback is reading it
   from the page's own boot request. In scope for the first release, or a README gap?

## Sources

core.telegram.org/api/terms and /api/qr-login; tdjson (PyPI), prebuilt-tdlib (npm);
github.com/Lonami/grammers; github.com/EionRobb/purple-teams (`teams_login.c`, issues #28, #38,
#78); Microsoft Graph "List messages in a chat" and change-notification docs; Entra docs on
device-code blocking and refresh-token lifetimes; hraftery/slacktokens; claudeissues.com issue
60364 (the Enterprise Grid report, one reporter).
