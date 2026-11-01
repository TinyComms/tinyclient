# 08. Getting in and staying in

Sign-in, first run, accounts, sign-out. Settings are `09-settings.md`. Researched as a plan on 2026-10-13; the client on main is 0.2.35. **[V]** = checked against a source (listed at the end). **[U]** = not verified; test before relying on it.

| Network | How you sign in | Who can |
| --- | --- | --- |
| Discord | `tinyclient-login`, the real login page. Captcha, 2FA, and passkeys are the page's. | A Discord account. The session uses the web client's API shape. |
| Slack | The login page. One `d` cookie, one `xoxc` token per workspace. The browser User-Agent is stored and reused. | Ordinary workspaces. Enterprise Grid is still a risk. |
| Telegram | QR, or phone, code, and 2FA. grammers. No web page. The `api_id` is a build-time secret, not in the repo. | Yes. Telegram's terms allow a third-party client and forbid a ghost mode. Typing and read receipts stay on. |
| Teams | Microsoft's login page, then the Teams chat token. Personal accounts and work accounts whose tenant allows this client. | A tenant that requires a compliant device or an approved app fails on Microsoft's own page. Say so, and do not retry in a loop. |
| Signal | QR from Linked devices. | The phone stays in the pair. |
| Messages | Continue with Google in an isolated Chrome, Edge, or Chromium window, then the matching emoji on the phone. | The phone is the account and has to stay online. |
| Tiny | System browser. Invite code, or the host for an account that already exists. PKCE, one-shot loopback. The provider name comes from the server. | Whoever that server invited. Another server is added beside the first. |

Messages uses the same Google account as the phone. There is no phone number to type. Google account and emoji pairing replaces QR onboarding ([Google setup](https://support.google.com/messages/answer/7611075)). Google does not publish the relay protocol and can change it. Gaia/UKEY2 field layout checked against [mautrix-gmessages](https://github.com/mautrix/gmessages/tree/main/pkg/libgm). Crypto vectors, stream parsing, cancellation, and a scripted phone cover the adapter. A real pair has not been run [U].

Telegram is grammers in this process, not TDLib. The comparison that chose it is the comment at the top of `crates/chatd/src/adapters/telegram.rs`. Secret chats are out.

Tiny sign-in pastes an invite or types a host, reads `GET /v1/auth/config`, opens the system browser, and finishes PKCE on a one-shot loopback with `POST /v1/auth/token`. No name or password is typed in the app. The button says "Continue with {provider}", using the server's `provider_name`.

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
  secret) so forks register their own, as the terms require of them [V]. The names are
  `TINYCLIENT_TELEGRAM_API_ID` and `TINYCLIENT_TELEGRAM_API_HASH`.
- **grammers:** the choice at the top of `crates/chatd/src/adapters/telegram.rs` (2026-10-13).
  It runs in this process. It does not keep a second database. QR first: no typing, no SMS.
  Phone number, code, and 2FA are the fallback. Creating an account is out. Secret chats are out.
  Calls are `06-calls.md`.

### Teams, in detail

- **What works:** the shipped adapter talks to the Teams client's own chat service. An Entra access token for
  `api.spaces.skype.com` is exchanged for a skype token; messages go through the regional
  `*.ng.msg.teams.microsoft.com` chat service; new messages arrive over trouter (websocket).
  purple-teams does this and is active (issues filed Sep 2026) [V]. Calls are `06-calls.md`.
- **Sign-in shape:** show Microsoft's real login page in `tinyclient-login` (MFA, passkeys and
  federated SSO are the page's own), using the authorization-code flow with the native-client
  redirect that purple-teams' source also carries [U that it still works], not device code,
  which is the flow tenants are blocking. A native-client
  refresh token lasts 90 days rolling; a token lifted out of the web app is a single-page-app
  token that dies in 24 hours [V for both lifetimes], so reading the session out of
  `teams.microsoft.com` the way we do for Slack would mean signing in every day. Do not do that.
  Personal accounts use client id `8ec6bc83-…`, tenant `consumers`, and a skype token from
  `teams.live.com/api/auth/v1.0/authz/consumer` (constants from purple-teams `teams_login.c` [V]).
  Work or school uses client id `1fec8e78-…`, scope `api.spaces.skype.com`, and a skype token from
  `teams.microsoft.com/api/authsvc/v1.0/authz`.
- **What stops it:** conditional access. A window in an unknown app has no device identity, so
  "require compliant device" fails even with correct credentials [U for that window specifically].
  The user sees Microsoft's own block page. tinyclient must then say in words: "Your organization
  doesn't allow this app. Teams stays in the Teams app." and stop offering to retry.
  purple-teams issues #28 (conditional access block, open since 2022) and #78 (AADSTS 53003, Apr 2026, 27 comments, fixed Jun 2026 by adding request headers and a browser user agent) [V]. Microsoft now ships a managed policy that blocks device-code flow [V].
- **Graph, the official route, does not rescue this.** Delegated `Chat.Read` does not itself
  need admin consent [V], but: personal accounts are "Not supported" [V]; there is no push to a
  desktop app, because change notifications need a public HTTPS endpoint and the all-chats
  subscription is application-only [V], so it would be polling; it needs our own multi-tenant app
  registration, and tenants that restrict user consent block it just as they block the other
  route [U on how common]; the same conditional access policies apply to it.
- **Guest tenants:** one account can sit in several tenants, each its own token set
  (purple-teams has an open bug past two [V]). Treat a tenant like a Slack workspace. Not re-tested here.

### Slack model

One sign-in window, one `d` cookie, every workspace that browser session holds. Each workspace
is an account under Slack with its own token, connection, unread count and sign-out. The rail
keeps **one** Slack button (the rail is 72 px; ten workspaces would take it over); its badge is a dot, and the Inbox holds the number. Inside the Slack filter the list is sectioned by workspace, and in the Inbox a row's
second line carries the workspace name only when the person has more than one. An Enterprise
Grid org is one account whose channels span its workspaces. That Grid path was not re-tested
(`localConfig_v2.teams` empty, rotating tokens). "Add another workspace" reopens the
window on the same cookie. SSO is the page's own; the browser User-Agent from sign-in is stored and reused on later calls.

## First run

Launching the app starts chatd. The person does not start a daemon by hand. Networks that are signed out are not a row of dead rail icons. The rail is Inbox, People, one button per signed-in network, pinned spaces, and add.

Signing out of one account removes that account's session material and its rows. Other accounts stay. The keychain (or keyring) item is still one per network (`session::account`). Slack workspaces and Tiny servers are several accounts inside that item. Whether `keep_connected` still drops every Slack workspace when one token dies was not re-read.

## Stories

| # | P | Story | Observable check |
| --- | --- | --- | --- |
| 1 | P0 | I double-click tinyclient and it works. I never start a daemon. | With no chatd running, launching the app yields a `status` answer on the socket within 1 s. |
| 2 | P0 | On first launch I can add Discord, Slack, Telegram, Teams, Signal, Messages, and a Tiny server. Signed-out networks are not a row of dead icons. Tiny is add-a-server. | Fresh data dir: the rail is Inbox, People, and add. `status` can report a network `SignedOut` without a rail icon for it. |
| 3 | P0 | I sign in to one network and see real conversations without restarting. | After the login window closes, the conversation list is non-empty within 5 s and the first open returns messages. |
| 4 | P0 | Networks I have not added stay out of my way: one add button, not a dead icon per network. | With only Discord signed in, the rail is Inbox, People, Discord, and add. |
| 5 | P0 | I close the sign-in window and nothing changes. | Login exits 1; `status` is identical to before; no keychain item is written. |
| 6 | P0 | I sign in to Slack once and get all my workspaces. | With a browser session holding N workspaces, `status` lists N Slack accounts after one window. |
| 7 | P0 | When a network signs me out, I am told which one, in words, with one button to sign back in. My cached messages stay readable. | Revoke the session remotely: link becomes `Expired`, not `SignedOut`. A banner names the account, `open` still returns cached rows, `send` refuses with a reason. |
| 8 | P0 | I sign out of one account and it is gone from this Mac and from the network's device list. | After sign-out: that account's session material is gone, no rows for it in SQLite, files for it deleted, and the network's own logout was called (best effort). Other accounts untouched. |
| 9 | P0 | I sign in to Telegram by scanning a QR code with my phone. | QR shown within 2 s of pressing Sign in; after the scan `status` shows Telegram online; a 2FA account is asked for its password in a native field. |
| 10 | P0 | I install from a disk image and open it without a security warning. | A downloaded macOS copy passes `spctl --assess` and opens on a Mac that has never run it. Notarization was not re-checked on this pass. |
| 11 | P0 | I get notifications that are named tinyclient, and I am never asked for my keychain password after an update. | Notification shows the app name and icon; replacing the app with the next version produces no keychain dialog. macOS check. |
| 12 | P1 | I sign in to Telegram with phone number and code when my phone is not nearby. | Number, code, optional password; wrong code says so and lets me retry without restarting. |
| 13 | P1 | I sign in to Teams with a personal or an unrestricted work account. | `status` shows Teams online; a message sent from the real Teams app appears without polling. |
| 14 | P1 | If my organization blocks tinyclient, I am told that plainly and not asked to try again. | A tenant with a compliant-device policy ends in the "organization doesn't allow this app" state, not a spinner or a loop. |
| 15 | P1 | tinyclient keeps receiving while its window is closed. It does not register an open-at-login item. | macOS, G2 on: quit the window and chatd still answers, and a DM notifies. There is no Login Items toggle. Linux: the optional `tinyclient-chatd.service` user unit can leave chatd up after the GUI quits. See `09`. |
| 16 | P1 | I open Settings with Cmd-comma and see every account, its state, and Sign out. | The account list matches `status`; Sign out here is the same action as story 8. The window is `09-settings.md`. |
| 17 | P1 | I can see how much disk tinyclient uses and clear it without signing out. | Size shown matches the data dir within 5%; after Clear, sessions remain and the list refills from the network. |
| 18 | P1 | tinyclient updates itself. | An older build offers the new version and relaunches into it with sessions intact. `G4` looks at GitHub's public latest release. |
| 19 | P1 | I add a Slack workspace that was not in my browser session. | "Add workspace" yields one more Slack account; existing ones do not reconnect. |
| 20 | P2 | I use two Discord or Telegram accounts, or a Teams account that is a guest in other tenants. | Not in this commit for Discord or Telegram: the keychain item is one per network. Slack workspaces and Tiny servers are the multiple accounts that exist. Teams guest tenants were not re-tested. |
| cut | | Creating an account on any network; pasting a token; importing a session from Chrome or the official app; a risk-acknowledgement screen (the README carries it). | |

## Session lifetime

| Network | Ends when | What I see |
| --- | --- | --- |
| Discord | Password change, "log out all devices", or Discord's own lock [U]. A sign-in from a new location may need an emailed approval first; the login page says so and I approve it in my mail, then press Log in again. | Story 7. |
| Slack | Workspace session-duration policy set by its admins [U on the range], SSO re-auth, or remote sign-out. One workspace can expire while the others stay up. | Story 7, naming the workspace. Re-sign-in must replace that workspace's token only. |
| Telegram | I terminate it from another device, or Telegram's inactivity timer (6 months by default, user-set) [U]. | Story 7. New sign-in also raises a login alert in my other Telegram apps; expected. |
| Teams | Refresh token unused for 90 days, password change, admin revocation, or a tenant sign-in-frequency policy [U on typical values]. The skype token is short and renewed silently [U on its life]. | Story 7. If renewal fails on policy, story 14. |

## Packaging

Published releases are macOS, Windows, and Linux (deb, rpm, AppImage, tar.gz, x86_64 and aarch64), built from `.github/workflows/release.yml`. Updates look at GitHub's public latest release (`G4`). The site is https://tinyclient.io. The repo is https://github.com/TinyComms/tinyclient.

`release.yml` says the programs are packaged and signed. Notarization, the certificate name, and a `spctl` result were not checked on this pass. Story 10 and story 11 stay macOS checks. They are not the whole packaging story.

Notifications need a bundle identifier. Keychain items are tied to the signature of the process that wrote them. After chatd has read or written an item, its access list is what keeps an update from prompting. The login keychain is still locked when the person is logged out.

There is no open-at-login switch (`G1`). Closing the window's effect on chatd is `G2` on macOS, and the optional `tinyclient-chatd.service` user unit on Linux (see `09`).

## Uncertainties

- Slack Enterprise Grid (`localConfig_v2.teams` empty, rotating tokens) was not re-tested. One 2026 report describes that shape [V that the report exists, U that it is general]. The probe reads that key.
- A live Messages pair has not been run [U]. The Google-and-emoji flow and the adapter tests are in the tree.
- `keep_connected` dropping every Slack workspace when one token dies was not re-read. The keychain item is still one per network.
- Notarization was not checked. A second Discord or Telegram login was not found as its own keychain account.

## Sources

core.telegram.org/api/terms and /api/qr-login; the grammers note in `crates/chatd/src/adapters/telegram.rs`; github.com/EionRobb/purple-teams (`teams_login.c`, issues #28, #38,
#78); Microsoft Graph "List messages in a chat" and change-notification docs; Entra docs on
device-code blocking and refresh-token lifetimes; hraftery/slacktokens; claudeissues.com issue
60364 (the Enterprise Grid report, one reporter); [Google Messages setup](https://support.google.com/messages/answer/7611075); [mautrix-gmessages](https://github.com/mautrix/gmessages/tree/main/pkg/libgm).
