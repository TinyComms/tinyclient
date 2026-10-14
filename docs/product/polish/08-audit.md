# 08. Rough-edge audit

What the build is today, measured from about 95 offscreen shots (about 80 distinct states) of the debug shell against scratch `chatd --seed-demo` daemons
(default, `fresh`, `expired`, `reconnecting`, `attention`, `many`; chatd absent), plus a read of
`crates/tinychat/src` and `crates/chatd/src`. Shots are at 1120×720 unless stated otherwise. Shot names in `code` are
the states below; refs are to `docs/design/01-system.md` (§) or another doc. Each finding is on one line, in this form: severity · area · where · what → expected.
**S1** looks broken or loses data · **S2** looks unfinished · **S3** refinement.

Caveat: 3 of the first 60 shots came back as a blank, canvas-coloured frame when three shells ran at once. All three were
fine when re-run one at a time, so the display was awake and the render was a timing race in `--shot`. These are not counted as findings.

## Owner-reported issues: causes

- **S1 · Inbox · no preview line on real accounts.** The row draws `Conversation.preview` (`gui/sidebar.rs:744`), but chatd only sets it when it ingests a message (`chatd/src/store.rs:1025,1045`). Discord (`adapters/discord/ready.rs:629`), Slack (`slack/tree.rs:369`) and Teams (`teams/tree.rs:151`) report unread counts through `set_counts` (`store.rs:917`) without ingesting the newest message, and only about 5 unread conversations are warmed. Telegram ingests the top message, and the demo ingests everything, which is why the demo looks right. Likely trigger today: a `SCHEMA` bump drops every table (`store.rs:22,512-521`), so yesterday's previews were wiped and nothing refills them. → The adapters should send the last message (Teams already has `lastMessage`, `teams/tree.rs:50`) or ingest it.
- **S1 · Rail · own avatar shows a letter.** `rail.rs:153` → `own_avatar` (`state.rs:624`) → `media_path` (`state.rs:712`) only reads a path that is already known and never asks for the file to be fetched. The own menu (`people.rs:309`) and the rows (`sidebar.rs:1001`) do ask. So the picture appears only if something else happens to fetch the same id, and it falls back to the letter when `Shell.media` is cleared at 2000 entries (`state.rs:750`). The rail also takes the *first* connected account (`rail.rs:148`), which may have no picture. → The rail should ask for the file through the hub as the menu does, and pick the account that has a picture.
- **S1 · People / Inbox · app and bot DMs listed among people.** `Person.bot` exists (`protocol/src/lib.rs:443`), but `people/list.rs:145-175` and `store.rs:1482` never filter on it. Slack and Teams turn every DM partner into `Relation::Contact` (`slack/tree.rs:347`, `teams/tree.rs:205`), and `Conversation` has no bot flag. → 05-people-presence: People is for people; apps belong under their own heading.
- **S1 · Conversation · SVG shows "Couldn't load image · Retry".** The adapters map any `image/*` MIME type to `MediaKind::Image` (`slack/message.rs:255`, `discord/content.rs:406`, `telegram/content.rs:248`). The shell trusts that (`conversation/media.rs:117`), and then the magic-byte check rejects the SVG (`images.rs:182`). → stories.md: "anything tinychat cannot draw is one labelled line". Treat SVG, HEIC and PDF as File, or fall back to a file row when decoding fails.
- **S1 · App · window closed on its own.** There is no panic hook, `catch_unwind` or `panic=` profile anywhere. A Rust panic inside a GPUI callback crosses AppKit's `extern "C"` boundary and aborts the whole process. A second path ends at the same "chatd is gone" screen: one poisoned `Mutex` in chatd (183 `lock().unwrap()`) panics every later request while the process stays alive. The riskiest sites are listed below.

## 1. Consistency across areas

- S2 · Badges · rail vs rows · The rail shows accent counts (32, 22, 10) that add up DM unreads, while the same DM rows show neutral counts (3, 12) (`components.rs:156` `rollup_badge`). → Ruling 2: plain unread is a neutral count; accent means "for you".
- S2 · Badges · `inbox`, `space0` · Mention badges read "@1" / "@2" in rows (`components.rs:78`) but "5" on a Space rollup and "1" on Pixel Foundry. The muted mention is a grey "@1" pill. That is three looks for the same signal. → §3 Badge: the numeral only, mention = accent fill.
- S2 · Path separator · `open0` header "Moss & Lichen > Trail", Telegram sheet "Settings > Devices > …" vs "›" in Inbox rows, the switcher and System Settings copy. → one glyph, "›".
- S2 · Menus · `menu-add` items are 2-line and about 30 tall; `menu-row`, `ctxmenu` and `menu-net` items are 28. The own menu mixes 28 rows with a 2-line header and an inline status field. → §3 Context menu: item 28; one menu style.
- S2 · Section labels · The emoji picker ("RECENT", "THIS CONVERSATION", "EMOJI", `emoji.rs:401` `to_uppercase`), the call devices menu ("SPEAKER", "MICROPHONE", `call/devices.rs:152`), the call people menu ("· 4 PEOPLE", `call/people.rs:199-213`) and the "NEW" divider (`draw.rs:93`) are all caps. → §2 Type: "no all-caps"; §3 Divider: "New" in `accent_text`, no fill.
- S2 · Choice controls · The status menu uses checkmarks, the devices menu uses radio circles, and segmented controls use fills. → one idiom for "current choice" inside menus: the check.
- S2 · Request buttons · `s6-people-pending`: Accept is a green-tinted square and Decline a bare ×. The call dock's Accept is a green filled button with a green card border. → §3 Button: primary is `accent`; green is only for presence and success icons.
- S2 · Pill shapes · Header "Leave" (`call-joined_channel`), the dock hang-up, the jump-to-bottom disc and the viewer's next disc are full-radius. → §6 Delete: "No control is a pill".
- S2 · Header strips · The main header has a hairline at 32 pt. The thread and members panes have the hairline, but the People and Discord titles do not, and the right-pane title (16/600 + grey count) is a different size from the sidebar's "Inbox" title. → one title strip spec per pane.
- S2 · Initials · The call dock shows "M" for Mara Okafor at 32 and the incoming card one letter, while the sidebar shows "AV" or "OB" at 32. → §3 Avatar: two letters from 32.
- S3 · Profile avatar is about 80 pt in the profile card (`s6-profile`). → §3 Avatar: 64.
- S3 · Danger and warning banners use tinted fills (`exp-inbox`, `att-inbox`, components sheet) and wrap to 2 or 3 lines. → §3 Inline banner: 32 high, `raised` + hairline, colour only in the glyph.
- S3 · People search field (≈245 wide) vs Inbox search (≈320) on the same sidebar; the People scope icons sit beside the field, while the Inbox compose icon sits in the same slot. → one header grid.
- S3 · "Delete…" (own message menu) vs "Delete" (pending-message menu, `ctxmenu-own`). → an ellipsis only when a dialog follows, consistently.
- S3 · Two time formatters: `gui/time.rs` and `gui/conversation/when.rs` each define "Yesterday" and the day words.

## 2. Hierarchy and noise

- S2 · Inbox rows · `inbox` · The Space label is truncated before the channel: "Moss... › #showcase", "H... › Mara Okafor, Dev...", "Fe... › Imogen Castellanos". It keeps only 1 to 2 letters, and the cut moves as the time widens ("Harbor Labs" one run, "Harbor L..." the next). → Drop or abbreviate the Space when space is short; never truncate both halves. 3 text shapes per line (§4.8).
- S2 · Inbox rows · Each row carries a network icon + Space + channel + time + badge + preview. Previews of mention rows all start "@Robin Vale …", which is me, so the first 12 characters say nothing. → Strip a leading mention of me from the preview.
- S2 · People rows · `people` · "Online" printed under every online person while the dot already says it, under a section header that also says "Online — 21". → Show a status text only when set; else nothing (§1 "say a state only while it is abnormal").
- S2 · Profile card · `s6-profile-body` · "Online · shipping…", then "Friend", then a Discord chip, then Message / Call / ⓘ / … then About / Mutual servers / Local time / "Open in Discord". About 11 elements before the body, and "Friend" is a lone line. → fold relation into the subtitle; one secondary action row.
- S2 · Viewer · `viewer` · The name appears twice (top-left and bottom caption); 9 toolbar controls including text buttons "Fit" and "1:1" next to icons; an eye icon with no clear meaning. → §3: one name, icon buttons with tooltips.
- S3 · Switcher · `switcher-mo` · "Fernhill Collective › Direct messages" as the context for a DM; the network word "Discord" as context for a server.
- S3 · Thread summary · "Last reply 1h ago" (relative) beside absolute times everywhere else, and "2 new" in accent beside a neutral reply count.
- S3 · Own menu · "Online on 3 accounts" when two of the three are Slack workspaces; "Return sets it." under the status field reads as a fragment.

## 3. Alignment and rhythm

- S1 · Context menu · `ctxmenu-mine`, `ctxmenu-own` · The menu runs to x = 1398 of 1400 and is cut off by the window edge, and the hover toolbar stays drawn underneath it, half covered. → §3 Popover: 6 from the anchor, flipped or shifted to stay inside the window; the toolbar hides while the menu is open.
- S1 · Emoji picker · `picker` · The last emoji row touches the window bottom with no padding (the popover overflows). → flip above or clamp inside the window.
- S2 · Row context menu · `menu-row` · "Open ↵" has no glyph, so its label starts 30 px left of "Pin to top of Inbox" and "Show in its Space"; the row that owns the menu is not highlighted. → §3 Context menu: aligned labels; the target row keeps `selected`.
- S2 · Mention chips · `w800`, `open0` · A chip that wraps ("@Tomasz / Brandt") splits the accent fill across lines, and the next line starts with a stray leading space (" has the keys"). → keep a chip on one line; trim the wrap space.
- S2 · Failed send line · `w800`, `thread` · "Not sent · Slow mode: wait 8 s befor…" truncates the reason and keeps "Retry Delete". → the reason wraps; actions follow on a new line.
- S2 · Group-DM avatar · Inbox "Foray carpool" and "H… › Mara Okafor, Dev…" · The two letter discs are about 20 and drawn without the 2 pt ring, so they read as clip art next to 32 photos. → §3 Avatar Group.
- S3 · Discord home · `discord` · "Direct messages" has a section label, but the 50 servers below have none, and "Show 3 more" is indented differently from server chevrons.
- S3 · Toast · `toast` · The 340-wide toast sits 16 from the edges but over the conversation canvas with no shadow visible at 2x. → §2 Elevation.
- S3 · System lines · `open0` · Centered call/pin lines between left-aligned messages carry no time.

## 4. States

- S1 · Modal · `confirm2` · The scrim dims only the conversation pane. The "Delete this message?" dialog overlaps the undimmed sidebar, so the sidebar stays bright and looks clickable. → §3 Modal: `scrim` over the whole window.
- S1 · No daemon · `nodaemon` · The whole window is "chatd isn't running — Start it with `cargo run -p chatd`". The same screen replaces the UI whenever the socket drops mid-session (`state.rs:249` → `layout.rs:757-767`). The shell never starts chatd except from the call dock (`call.rs:289-319` `ensure_daemon`). → The shell launches chatd and keeps the last snapshot on screen behind a 06-live-signals banner ("Reconnecting…") while retrying.
- S1 · Hung daemon · chatd binds its socket, then reads the keychain synchronously in `Chat::new` before accepting (`chatd/src/main.rs:23`, `server.rs:175-184`, `session.rs:31`). The shell's request sits in the backlog with no timeout (`client.rs:6-10`), so the window is a blank canvas with no words. → Accept first, answer `Status` with "Waiting for the keychain…"; the shell times out at about 3 s and says "tinychat is waiting for macOS to allow keychain access. Look for the prompt."
- S1 · Data loss · A denied keychain prompt reads as "no session" (`session.rs:31` `.ok()?`), and then `forget_strangers` deletes that account's store and blobs (`server.rs:185,207-216`). → treat "denied" and "locked" as unknown, never as signed out.
- S2 · Narrow window · `narrow2` (700 wide) · The sidebar hides itself and the empty state says "Pick a conversation" with no list on screen; `--open-first` opens nothing. → the sidebar floats on demand, or the empty state offers ⌘K / the sidebar toggle.
- S2 · Reconnecting · `rec-inbox` · In the Inbox (a mixed list) a reconnecting Slack account is never mentioned (`sidebar.rs:1262` requires `!mixed`), and in Slack mode it appears only after 5 s. → 06-live-signals: the Inbox says which network is late.
- S2 · Pending send · `open0` vs `thread`/`float` · "Batteries too." (pending) is grouped under the failed message in one render and is a separate "You" group after Tomasz in the next. Its time ticks with the clock (10:20 → 10:21 → 10:22 → 10:25). Grey text is its only pending cue. → a fixed position at send time, a fixed time, and a visible "Sending…" (rule 9).
- S2 · Avatars · `ctxmenu-mine` (deleted message by Wren) · The initials are drawn under the image and show through any translucent picture ("WH" over a dimmed photo). Real transparent PNG avatars and bot logos will show letters behind them. → draw the initials only when there is no image.
- S2 · Members · `s6-members2` · The skeleton rows never resolve in a staged People view; no empty or error state was found for a member list that fails. → §3 Loading + Empty state.
- S2 · Spoiler · `up3300` · A 500×375 empty box with "Spoiler · click to show" in the middle. → keep the box but show size/type; it reads as a failed image.
- S3 · Failed image · `up2200` · "Couldn't load image · Retry": Retry is plain text inside the caption, not a control (§3 Button ghost).
- S3 · Presence arrives a frame late (`sif-other` drew Inbox avatars with no dots), so the corner pops in.
- S3 · Open conversation keeps its "@1" badge in the Inbox while it is on screen (`open0`). This is correct while the window is inactive, but there is no visual tie between the open row and its state.

## 5. Data honesty

- S2 · Inbox counts · The segment says "Mentions 9" (conversations), while the badges inside sum to 13 mentions, and the rail says 32 (DM unreads + mentions). That is three numbers with three meanings. → name what is counted, or show none.
- S2 · Thread names · `space0` · A thread whose root was deleted is listed as "↳ Deleted message" in the tree.
- S2 · Requests · "Requests 2" in the All scope vs "Pending 3" in the Discord scope (`s6-people-pending`): the label and count change with scope. → one word, one count (incoming).
- S3 · Voice message · `up2200` · The waveform is real metadata here; make sure an adapter without one draws a flat bar, not invented peaks (`conversation/media.rs:212`).
- S3 · Viewer · "1 of 22" counts every picture in the loaded pages; it grows as history pages in.

## 6. Copy

- S1 · `layout.rs:763-764` · "chatd isn't running / Start it with `cargo run -p chatd`" is a developer instruction with code ticks. → "tinychat is starting…" / "Couldn't start tinychat's background service. [Try again]".
- S2 · `signin.rs:227,445,483`, `conversation.rs:433` · "tinychat couldn't reach chatd", "chatd gave an answer tinychat did not expect", "chatd answered with something else". → never name chatd; say what to do.
- S2 · `composer.rs:560` · `format!("Not sent: unexpected {other:?}")` prints a Rust Debug value to the user.
- S2 · `layout.rs:613` · "Show in its Space": "Space" is internal vocabulary. → "Show in Moss & Lichen" (the real name).
- S2 · `att-inbox` · The 3-line "Discord wants you to verify your account. Open Discord, finish the check there, then come back." has no button. → a one-line banner + "Open Discord" action.
- S3 · "tinychat can't use the microphone" (sentence starts lowercase); "Wants to be friends · @otis.b…" truncates the handle; "Return sets it."; "Online on 3 accounts".
- S3 · The menu-add entries are inconsistent: "Add a Slack workspace" vs bare "Telegram / Sign in" and "Teams / Sign in" (first launch says "Microsoft Teams").
- S3 · `menu-net` · The network menu is the account name + "Sign out" only; Sign out is the one action on offer and sits in red at rest.

## 7. Robustness

- S1 · The shell has no panic containment (see above). → `panic = "abort"` is no worse, but a panic hook that writes `~/Library/Logs/tinychat/crash.log` and relaunches would make "closed on its own" diagnosable.
- S1 · `server.rs:1068,1076` · One `accept()` or thread-spawn error (for example running out of file descriptors) returns from `serve` and chatd exits.
- S2 · Frames · `protocol/src/lib.rs:1051-1072` · Reads are capped at 1 MiB but writes are not. One page of 60 long Slack or code messages can exceed the cap, and callers drop the error silently (`state.rs:491,511,587`). A bad frame from the shell makes chatd drop the connection without an `Error` reply (`server.rs:1052`). → cap on write, and answer `Error::TooLarge`.
- S2 · A shell and chatd on different versions (unknown serde variant) reads as "chatd isn't running".
- S2 · `client.rs:6-10` · A fresh `UnixStream` per request with no read or write timeout.

## 8. Performance feel

- S2 · `state.rs:144-150,479` · One new message reloads Spaces and refetches every loaded Inbox page, one socket connection each.
- S2 · `state.rs:426-431,607`, `sidebar.rs:322` · Each changed row rebuilds and compares the whole flattened tree (up to 4000 rows), once per row.
- S2 · `conversation.rs:630` → `refresh` · A reaction or edit refetches the visible page, and `pages.rs:96,102` merges it in O(n²) with deep comparisons on the main thread.
- S2 · `sidebar.rs:757-759` · `shape_line` runs 3 times per Inbox row per frame to measure the Space label.
- S3 · `draw.rs:552-555,629` · Rich text is re-parsed (`rich::blocks`, sort, allocations) on every frame for every visible message.
- S3 · `sidebar.rs:1000-1024` · Row closures call into the people hub (`watch_peers`, `want_media`) every frame; `rail.rs:152` clones a `Person` every render.
- Good: socket calls run off the main thread (`state.rs:311`), and image decodes are in the background (`images.rs:114`).

## 9. Mac citizenship

- S1 · There is no menu bar at all: no `set_menus` anywhere. So there is no About, no Settings (⌘,), no Edit menu (Undo, Cut, Copy and Paste exist only as key bindings), no Window menu, no Hide (⌘H), Minimize (⌘M) or Close (⌘W). → 08-first-run-settings §menus.
- S2 · Closing the window quits the app (`mod.rs:533`). → Ruling 11: closing keeps chatd receiving; the app stays in the Dock.
- S2 · Window size and position are always centred at 1120×720 and not restored (`mod.rs:506-511`); there is no double-click-to-zoom on the drag strips (`layout.rs:38`); the title is the fixed "tinychat", not the conversation name.
- S2 · No Reduce Motion: menus and toasts animate unconditionally (`overlay.rs:192`, `draw.rs:437`). → §2 Motion: durations 0. No Increase Contrast.
- S2 · No accessibility labels; the GPUI revision exposes none, so VoiceOver reads nothing. Call it a known gap in the README rather than leave it silent.
- S3 · Dark-only under a light system: traffic lights and the drag strip stay dark (by ruling 12; still worth a screenshot check).
- S3 · Text input implements `EntityInputHandler` (IME, Dictation, ⌃⌘Space all work) but has no spell check.
- S3 · No ⌘+ / ⌘- text size; no ⌘F in-conversation search; ⌘N (new message) is not bound although the compose button exists.
- S3 · The bundle is ad-hoc signed (`packaging/bundle.sh:55`), so every rebuild re-asks for keychain access, which feeds the hang above.

## The ten riskiest panic sites

1. `chatd/src/hub.rs:100`, `server.rs:71,152,199,236,948`: `lock().unwrap()` on the store and adapter maps. One panic while held poisons them and every later request panics.
2. Shell-wide: no panic hook; any panic in `render` or a callback aborts across AppKit's FFI boundary.
3. `tinychat/src/gui/conversation/rich.rs:360`: `text[range.clone()]` in `jumbo()`. A span with start > end on an emoji-only message (spans are never validated in `protocol/src/message.rs:399`).
4. `tinychat/src/gui/input.rs:666`: `self.editor.text()[range]`. An IME or Dictation range with start > end; `composer/edit.rs:534-548` clamps the length but not the order.
5. `chatd/src/server.rs:1068,1076`: `conn?` / spawn `?` ends `serve`, so chatd exits.
6. `tinychat/src/gui/mod.rs:529`: `.expect("the window opens")` at launch (no Metal device, headless session).
7. `chatd/src/adapters/teams/html.rs:483`: `self.out.text[start..]`. It is safe only while nothing rewrites earlier text; fragile under edits.
8. `tinychat/src/gui/conversation/pages.rs:91,108`: `messages[..before]`, `messages[len-after..]`. In range by construction (`:82`) today; one change to `merge` breaks that.
9. `tinychat/src/gui/sidebar/rank.rs:18-21`: `wanted.len() - 1`, guarded only by the `is_empty` check at `:10`.
10. `tinychat/src/gui/sidebar.rs:1164`: `unreachable!`, guarded at `:1206`. The adapters' slicing (`slack/text.rs:332`, `discord/content.rs:134-297`, `telegram/content.rs:87`) was checked and is bounded.

## What would most change the first impression in one day

1. Fill Inbox previews from the adapters' last message (`set_counts` + preview) so every row has a second line.
2. Make the shell start chatd itself, keep the last snapshot on screen behind a "Reconnecting…" banner, and stop showing `cargo run`.
3. Accept connections before the keychain read; say "waiting for keychain access" after 3 s; never `forget` on a denied read.
4. Add a panic hook and crash log; fix `rich.rs:360` and `input.rs:666`; replace `lock().unwrap()` on the store with recovery.
5. Add a real menu bar (About, Settings, Edit, Window, ⌘W hides the window and doesn't quit) and restore the window frame.
6. Have the rail avatar fetch the picture; draw initials only when there is no image.
7. Keep every popover inside the window (context menu, picker), and make the modal scrim cover the whole window.
8. Fix the badges: the numeral only, accent only for mentions, neutral for DM unread on the rail; one meaning for the Mentions count.
9. Inbox row titles: never cut the Space to 1 to 2 letters; drop it when short; strip the leading "@me" from previews.
10. Show bots and apps as apps, not people; show SVG, HEIC and PDF as file rows; remove all-caps labels and the filled "NEW".
