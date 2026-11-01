# 08. First run, sign-in, accounts, settings

Mockup: `mockups/08-first-run-settings.html`. Requirements: `docs/product/08-sign-in-accounts.md`. Sizes are px at 1x. Colors are `theme.rs` tokens unless listed under New tokens. Strings in quotes are final copy. Buttons, switches, segmented controls, fields and the sheet are the visual-system agent's components; the sizes below are what these screens need from them.

## 1. First launch

Main window minimum 540 × 720 is not required; the window minimum is 540 × 440. With zero accounts there is no rail, sidebar or list: `canvas` fills the window and the top 52 still drags it. One centered column, 440 wide. The card has seven rows:

| Element | Spec |
|---|---|
| App mark | 64, then 16 gap |
| Headline | "Welcome to tinyclient", 22 semibold, `text` |
| Line | "Sign in to one network to start. Add the others whenever you like." 14 `text_soft`; 6 above, 28 below |
| Network card | 440 wide, `raised`, 1 `hairline` border, radius 14, seven rows split by hairlines |
| Row | padding 0 16; network icon 32; 14 gap; name 14 semibold over a 12 `text_faint` line; quiet button "Sign in" (30 high, radius 15, `selected` fill) |
| Footnote | the platform sentence in `host::SIGN_IN_KEPT`, 12 `text_faint`, 16 above |

| Network | Second line |
|---|---|
| Discord | Servers, direct messages and voice |
| Slack | Every workspace from one sign-in |
| Telegram | Scan a code with your phone, or use your number |
| Microsoft Teams | Work and school accounts; calls open in Teams |
| Signal | Link this computer from your phone; text for now |
| Messages | Pair from your Android phone. It has to stay online. |
| Tiny server | A server you already run |

The footnote is the platform sentence in `host::SIGN_IN_KEPT` (macOS keychain, Windows Credential Manager, or the system keyring).

- **Focus.** The first available row has keyboard focus at launch: 1.5 `accent` inset ring, `hover` fill, and its button turns accent. Up/Down move, Return signs in. The whole row is the click target. This is the only accent on the screen.
- **Every row can sign in.** The "Not available yet" branch remains in the code and is unused while `AVAILABLE` lists all seven.
- **Path to the first message:** launch, Return, the network's own login, then the inbox. The list has focus with the newest row highlighted; Return opens it and puts the caret in the composer. Nothing is opened automatically, because opening marks a conversation read.

## 2. Sign-in

**Helper window (Discord, Slack, Teams).** 520 × 760, clamped to screen height minus 80, minimum 400 × 600, resizable, standard title bar. Titles: "Sign in to Discord", "Sign in to Slack", "Sign in to Microsoft Teams". It opens centered on the main window and stays above it (child window).

**Main window meanwhile.** The row keeps its place with `hover` fill; its second line becomes "Finish signing in in the Discord window." in `text_soft`; the button becomes quiet "Cancel". Clicking the row raises the helper. The other rows go to 40% opacity and do not respond. No spinner and no timeout. When adding from the rail (section 3) the same row state shows inside the add popover.

| Outcome | What the row shows (12 `danger` under the second line; button back to "Sign in") |
|---|---|
| Helper closed, or Cancel | Nothing. The screen is exactly as before. |
| Page did not load | "Couldn't reach Discord. Check your connection and try again." |
| Helper exited on its own | "The sign-in window closed unexpectedly. Try again." |
| Signed in, session not found after 30 s | "You're signed in, but tinyclient couldn't pick up the session. Try again." |
| Network rejected the session | "Discord didn't accept that sign-in. Try again." |
| Keychain write failed | "tinyclient couldn't save the sign-in to your keychain." |

**Success.** The helper closes and the shell appears in the same frame: rail (All chats, the network, "+", settings at the bottom), sidebar, empty conversation pane. At the top of the sidebar, under the heading, a progress list (padding 20, rows 13 text, 12 gap, 16 leading mark): finished steps have a `positive` check and `text`; the current step has its number and `text_soft`; later steps `text_faint`. Steps, with real numbers: "Signed in as mira.k", "Connected to Discord", "Loading conversations: 38 so far", then per extra workspace "Connecting to Lumen Labs". Rows appear below a hairline as they arrive. The list is removed when the last step finishes. If a step fails it turns `warning`: "Couldn't load conversations. Retrying at 10:42."

**Slack workspaces.** If the sign-in holds one workspace, there is no extra screen. If it holds more: a sheet, 440 wide, padding 28, `panel`, radius 16, over a `scrim`. Slack icon 28 and "3 workspaces found" (17 semibold); "Signed in as mira@halden.example. Choose which to add." Each workspace is a 56 row: 32 squared avatar, name 14 semibold, address 12 `text_faint`, a switch, all on. Footer: link "Sign in to another workspace" (reopens the helper on the same session) and primary "Add 2 workspaces" ("Add workspace" for one; disabled at none). Esc equals Cancel and adds nothing. None found: "Slack signed you in but listed no workspaces. Slack stays in the Slack app for this account." with "Done" only.

**Telegram** is a native sheet, same 440 shell, never a web page.
1. *QR (default).* "Sign in to Telegram". White tile 232, radius 14, 16 padding, 200 code. Steps, 13 `text_soft`, numbered discs 18: "Open Telegram on your phone." / "Go to Settings > Devices > Link Desktop Device." / "Point the phone at this code." Footer: link "Use a phone number instead", quiet "Cancel". The code swaps in place when Telegram rotates it; no countdown. Before the first code arrives the tile is `raised` with "Getting a code from Telegram". Offline: "Couldn't reach Telegram. Check your connection." and "Try again".
2. *Phone.* "Your phone number". Country field 96 and number field, both 40 high. Primary "Send code". Link "Scan a code instead". Errors: "That doesn't look like a phone number." / "No Telegram account uses this number. Create one in the Telegram app first." / "Telegram has blocked this number."
3. *Code.* "Enter the code". "Telegram sent a 5-digit code to the Telegram app on your other devices, for +1 555 010 0142." (or "by SMS to"). Five boxes 48 × 56, 8 gap, 24 semibold; it sends on the fifth digit; paste fills all. Link "Send the code by SMS". Errors, 12 `danger`, boxes get a `danger` border, clear and refocus: "That code isn't right. Check it and type it again." / "That code has expired." with link "Send a new code" / "Too many tries. Telegram allows another at 3:42 PM."
4. *Password*, only for accounts with two-step verification, also after a QR scan. "Two-step verification password", one 40 field, "Hint from Telegram: the old street" in 12 `text_faint`, primary "Continue". Errors: "That password isn't right." Link "Forgot it? Reset it in the Telegram app." Back arrow (20, top left) on steps 2 to 4. Success closes the sheet into the same loading list.

**Teams** uses the helper. Teams personal accounts are not built. The row says work and school accounts, and that calls open in Teams. Blocked by policy: the helper closes and a sheet (440, centered text) shows the Teams icon 44 with a 24 `building-2` badge; title "Brightwater Logistics doesn't allow this app" ("Your organization doesn't allow this app" when the name is unknown); "Teams stays in the Teams app for this account. Signing in again will not change this."; an inset card, "WHAT YOU CAN DO": "Ask your IT admin whether other Teams clients are allowed. The policy is theirs to change." / "Microsoft's code: AADSTS53003" in 12 `text_faint`. Footer: link "Use a different account", quiet "Open Teams", primary "Done". There is no retry. Afterwards the Teams row reads "Blocked by Brightwater Logistics" and offers only "Use a different account". The same sheet is used when a working Teams account later fails renewal on policy.

**Signal** uses the same sheet as Telegram's QR step, with Signal's three steps, and no phone-number path.

**Messages** opens the system browser for the Google account used by Messages on the Android phone, then asks you to confirm the matching emoji on the phone. The phone has to stay online.

**Tiny server** asks for the invite code, then opens your browser against that server's OpenID Connect provider.

## 3. Adding and reordering

- **Rail "+"** opens a popover to its right: 300 wide, `panel`, radius 12, 6 padding, 48 rows (icon 24, name, 12 second line). It lists networks that are signed out, then "Add a Slack workspace" when Slack is signed in, then "Add a Tiny server" ("Join with a code") when a Tiny server is signed in. A row starts that network's sign-in at once; the popover stays open in the waiting state (section 2).
- **Add a Slack workspace** reopens the helper on the kept session. Workspaces already added are listed with "Added" in place of the switch. Existing workspaces do not reconnect.
- **Reorder.** Drag a rail button, or drag the grip in Settings, Accounts. Keyboard: Option-Up and Option-Down on a focused account row. Slack workspaces reorder inside the Slack group; the same order sections the Slack list. A second account on one network (P2) is a second row in its group.

## 4. Session expired

Cached messages stay visible and readable; only sending stops. Nothing about other accounts changes.

- The live-signals indicators on the rail and sidebar lead here. Clicking either one, or the bar below, starts sign-in for that account directly: one click to the helper or the Telegram sheet.
- **Bar.** Replaces the composer in every conversation of that account: 56 minimum height, margin 0 20 20, radius 14, `warning_soft` fill, 1 border `warning` at 25%. Glyph `circle-alert` 18 `warning`; "Slack signed you out of Halden Studio" 14 semibold; "You can read what is saved here. Sign in again to send and receive." 12 `text_soft`; primary "Sign in again". Single-account networks: "Discord signed you out". This is a state bar, not a risk notice.
- Above it, centered 12 `text_faint`: "Saved on this Mac up to yesterday, 4:20 PM". Header second line: "Halden Studio · Signed out". A draft in the composer is kept and restored.
- Re-sign-in replaces that account's session only. Signed in as someone else: "That sign-in was for Lumen Labs, not Halden Studio." with "Try again"; a new workspace is offered through the found list.
- On success the bar is replaced by the composer and the loading list shows "Catching up Halden Studio".

## 5. Settings

Withdrawn. The Settings window is `09-settings.md`.

## 6. Sign-out, and quit versus close

**Sign-out sheet**, 420 wide, padding 24. Avatar 32 and "Sign out of Lumen Labs?" (17 semibold). "This removes from this Mac the saved sign-in, 18,204 saved messages and 212 MB of images and files for this workspace, and ends this session on Slack." Then "Nothing is deleted on Slack. Your other accounts are not touched." Buttons right-aligned: quiet "Cancel" (default, Return), `danger` "Sign out". Unknown counts are dropped from the sentence. If the network's own logout fails, the account still goes and one line stays in Accounts until dismissed: "Signed out here. Slack may still list this session; end it at slack.com." The header "Sign out" link in today's sidebar goes away.

**Quit versus close, said once.** After the first inbox finishes loading, a card sits at the foot of the sidebar (284 wide, `raised`, radius 12, padding 14): "Messages keep arriving when this window is closed" (14 semibold); "Closing the window leaves tinyclient running. Quit with ⌘Q to stop it." (12 `text_soft`); quiet "Not now". The button removes it for good. On macOS, with "Keep receiving when the window is closed" on, closing hides the window and leaves chatd receiving. Elsewhere, closing quits. ⌘Q stops an application-managed chatd. A systemd user unit is left running (`06` §8). There is no Open at login control. The macOS notification prompt is the system's own and is requested when that first load ends.

## 7. Identity

**Name:** "tinyclient", always lower case. **Icon:** 1024 canvas. Plate 824 square at 100,100, radius 186, vertical gradient `#4f8cff` to `#1f56d6` (the accent, lit from above), 4 inner stroke white at 10%. A white speech bubble 500 wide by 440 high at 262,282: three corners radius 160, the lower-left corner radius 18, and that sharp corner is the tail. "tc" is cut from it in the accent `#2f6feb`: round-capped strokes 64 wide, the t's stem curving into its foot, the c a circle open on the right. No network color or logo, and not a count of networks. For 32 and below the bubble and letters are drawn 24% larger with 74 strokes so the letters stay open. Menu bar item: the bubble with "tc" cut out, template image, 18. It is not drawn. The status item is not built (`06` §8).

**About** is the last Settings section; "About tinyclient" in the menu opens it. Icon 96; "tinyclient" 22 semibold; the running version, 12 `text_faint`; "One small app for Slack, Discord, Telegram, Teams, Signal, and Messages, and for Tiny servers you run."; buttons "Website", "Release notes", "Copy version". The window still draws "One small app for Slack, Discord, Telegram, Teams and Signal." until that copy is updated. Acknowledgements group: "Network icons: Dashboard Icons (homarr-labs/dashboard-icons), Apache-2.0. The logos are trademarks of their owners." / "Interface icons: Lucide, ISC." / "Open-source libraries" opens the bundled license list.

**Menus.** Conversation and View shortcuts yield to the navigation spec where they differ.

| Menu | Items |
|---|---|
| tinyclient | About tinyclient · Check for Updates… · Settings… ⌘, · Services · Hide tinyclient ⌘H · Hide Others ⌥⌘H · Show All · Quit tinyclient ⌘Q |
| File | New Message ⌘N · Add Network… · Add Slack Workspace… · Attach File… ⌘U · Close Window ⌘W |
| Edit | Undo ⌘Z · Redo ⇧⌘Z · Cut ⌘X · Copy ⌘C · Paste ⌘V · Paste and Match Style ⌥⇧⌘V · Select All ⌘A · Find in Conversation ⌘F · Search Everywhere ⇧⌘F · Emoji & Symbols ⌃⌘Space |
| View | All Chats ⌘1 · one item per network in rail order ⌘2 to ⌘8 · Quick Switcher ⌘K · Hide Sidebar ⌃⌘S · Bigger ⌘+ · Smaller ⌘− · Actual Size ⌘0 · Enter Full Screen ⌃⌘F |
| Conversation | Next ⌥↓ · Previous ⌥↑ · Next Unread ⌥⇧↓ · Previous Unread ⌥⇧↑ · Back ⌘[ · Forward ⌘] · Mark as Unread ⌥⌘U · Mute… · Start Call ⇧⌘C · Open in Slack ⌥⌘O (names the network) · Copy Link |
| Window | Minimize ⌘M · Zoom · tinyclient (reopens the main window) · Bring All to Front |
| Help | tinyclient Help · Keyboard Shortcuts ⌘/ · Release Notes · Report a Problem… · Copy Diagnostics (version, macOS, connection states; no message content) |

## 8. Update available

Updates download in the background and install on quit; no dialog, no toast. While one is waiting: a 6 `accent` dot on the rail's settings button with the tooltip "Update ready"; "Check for Updates…" becomes "Restart to Update"; About shows "Version 0.5.0 is ready. It installs when you quit." with quiet "Restart now". With automatic install off, the same dot and "Version 0.5.0 is available." with "Install". A failed update is said only in About: "Couldn't install 0.5.0. Will try again tomorrow."

## New tokens

| Token | Value | Use |
|---|---|---|
| `panel` | `#1b1c24` | Sheets and popovers; opaque, since there is no blur |
| `scrim` | `rgba(0,0,0,0.5)` | Behind a sheet |
| `hairline_strong` | `rgba(255,255,255,0.12)` | Field, sheet and window borders |
| `warning_soft` | `rgba(227,169,74,0.14)` | Signed-out bar |
| `switch_off` | `rgba(255,255,255,0.16)` | Switch track when off; knob is white |
| `qr_paper`, `qr_ink` | `#ffffff`, `#0c0d11` | The QR tile only |
| `icon_plate_hi`, `icon_plate_lo`, `icon_bubble` | `#4f8cff`, `#1f56d6`, `#ffffff` | The app icon only |

The visual-system spec wins where it names the same thing. New Lucide glyphs: `plus`, `settings`, `check`, `arrow-left`, `circle-alert`, `building-2`, `grip-vertical`, `chevron-down`.

## GPUI notes

- The first-run screen and the shell are two renders of the same root view, switched on "any account exists", so the change after sign-in is one frame with no window swap.
- Sheets are an absolutely positioned child of the root: a full-size `scrim` div that swallows clicks, then the `panel` with one box shadow (0 24 60, black 50%). The add popover is anchored to the "+".
- The QR code is one canvas of 29 to 37 filled squares per side at an integer module size (200 / 29 rounds to 6, so the code draws at 174 to 222 and is centered in the tile). Redraw only on rotation.
- The code boxes are one hidden single-line input with five drawn cells; the caret is the focused cell's border.
- Settings is a second GPUI window opened on demand; it subscribes to the same `status` as the shell.
- The helper needs two new things: a parent window position to center on, and distinct exit codes for "closed by the user", "page failed to load" and "blocked by policy", so each gets its own wording.
- Nothing on these screens animates at rest. The only motion is a 120 ms opacity fade on sheets.

## Deliberately absent

- Risk warnings, disclaimers, consent or "unofficial client" notices anywhere in the app. The README has them.
- A welcome tour, feature slides, a "what's new" window, tips, account creation, token paste, import from a browser or from the official apps.
- A spinner or progress ring for sign-in, and any countdown that ticks.
- A retry button on the Teams policy block. A Telegram switch for typing or read receipts.
- Settings for: accent or theme colors, per-network appearance, fonts, proxy, a notification rules editor, custom sounds, custom keybindings, cache location, "experimental" options, language (it follows macOS), per-network status, and anything about the networks' own account settings.

## Open points for the owner

1. Light theme: specified as a control but hidden until the light values exist. Ship dark-only first?
2. Settled in `06` §8. There is no status item. Quit is not "Cmd-Q stops receiving" on every platform.
3. `04-live-signals.md` R6 wants a Telegram "don't send read receipts" option; `08` says Telegram's terms forbid it. This design follows `08` and offers it for Teams only.
