# 05 · Profile and info pane

What the right pane (and its small version, the card) says about a person, a conversation and a Space. Requirements:
`product/polish/06-profile-info.md`, ruled by `00-rulings.md`. Frame and following: `polish/07-panes.md`. Tokens and parts:
`design/01-system.md` and `polish/01-system.md`. Mockup: `mockups/05-profile-info.html`. Sizes in pt.

## 1. Pane frame
300 wide (`PANE`), fill `sidebar`, hairline left. Strip 52 per `07-panes`: title "Profile" (person) or "Details"
(conversation, Space), close `x`; `‹ Members` / `‹ Thread` crumb when pushed. Body scrolls under the strip; padding 16,
content width 268. The presence ring and every opaque fill use `sidebar`.

## 2. The person: fixed order
Everything above the first hairline paints in the first frame from the row (`Person`), so it never moves when the
profile answers. A section is drawn only with a value: no label without a value, no "N/A", no placeholder words.

| # | Section | Anatomy (y from the strip's hairline) |
|---|---|---|
| 1 | **Band** | 0–48, full pane width, the network's accent for this person (Discord `accent_color`, Telegram peer colour) as `band(c)` = c at 40% over `sidebar`. No colour: nothing drawn, same geometry, so offsets never change. |
| 2 | **Picture** | avatar 64 at x 16, y 16 (overlaps the band by 32), dot 14 ring 2 `sidebar`. Bots never get a dot. |
| 3 | **Name + mark** | y 92, 16/22 600 `text`, one line, ellipsis. Mark tag 6 after: 16 high, radius 4, padding 5, 11/500. Bot · App · Guest · External: `hover` fill, `text_soft`. Verified: `badge-check` 14 `text_soft`, no tag. Scam · Fake: `danger_soft` fill, `danger`. |
| 4 | **Handle · pronouns** | y 114, 13/18 `text_soft`: `@juno.c · they/them`. Teams (no handle): title here instead. Absent: the line collapses. |
| 5 | **Presence + status** | y 132, 13/18: 10 presence glyph, word `text`, then ` · ` emoji + status `text_soft`, then ` · until 5 PM` `text_faint`. Wraps to 2 lines. Telegram buckets are words with no glyph ("last seen recently"); an exact last seen is the hollow ring + "Last seen today at 8:12 PM" (no relative times, `polish/01` §2). Unknown and no status: line absent. Busy uses the glyph `polish/01-system` gives rule 6. |
| 6 | **Actions** | 16 below, row of 28 buttons, gap 8 (§3). |
| 7 | **Notifications** | DM pane only: 12 below, 28 row, "Notifications" 13 `text` left, switch 32 × 18 right (on = not muted). |
| 8 | **Heads-up** | 12 below, at most one, the banner of `polish/01`: 32 (40 when the network's text needs 2 lines, 12/16), `raised` + `hairline`, radius 6, glyph 16. Priority: Scam/Fake (`shield-alert` `danger`: "Telegram flags this account as a scam. Never send codes or money.") › Blocked (`ban` `danger`, "You blocked Vex", ghost **Unblock**) › Out of office (`plane` `warning`, "Out of office until Mon, Oct 19" + their note on line 2) › Not friends (`info` `text_soft`, "Not friends · they may not see your message"). |
|   | — hairline, 16 margin above and below — | |
| 9 | **Right now** | no label. Fact rows: min 28, glyph 16 `text_soft` at 0, text 13/18 at 24, wraps. In voice (`volume-2`): "In voice · #trail · Moss & Lichen", click opens the channel (hover fill, inset −8, radius 6). Playing (`gamepad-2`) "Playing Outer Wilds · 42 minutes"; Listening (`music`) "Listening to Nils Frahm · Says"; Huddle (`headphones`); Meeting (`calendar`) "In a meeting · until 3:30 PM"; local time (`clock`) "4:12 PM local · 3 h behind you". At most 3 rows. |
| 10 | **Role** | Section header (28, first 24; label 11/500 `text_faint`). Slack/Teams: title 13 `text`, department 13 `text_soft`. Discord / Telegram group, only when opened from that Space: "Ju in Moss & Lichen" (Space icon 16, nickname 600) then role chips: 20 high, radius 4, `hover` fill, 8 dot in the role's colour, 12/16 `text_soft`, 4 apart, wrap, at most 6 then "+3". |
| 11 | **About** | Header, then bio 13/18 `text`, links `accent_text`, 4 lines then "more" (ghost, `accent_text`). Discord note (P2) follows inside About: 11/500 `text_faint` "Note · only you see this", then the text as a borderless field (hover `border`). |
| 12 | **Contact** | Header, then fact rows: `mail` + address; `phone` + masked "+385 •• ••• ••17" with a small ghost **Show** at the right. Click a row copies (toast "Email copied"); Show unmasks until the pane closes. Never on Discord. |
| 13 | **In common** | Header. Mutual Spaces as place rows (28, avatar 20 rounded, name 13, click opens the Space), at most 5, then "2 more" (ghost row). Telegram before the +1 ask: one row "4 groups in common ›", click loads names into the same place. |
| 14 | **Shared** | Hairline, then tabs (§4). Always present where a DM with them exists. |
| 15 | **Foot** | 16 below, 12/16 `text_faint`, wraps: "On Discord since Dec 2017 · Friends since Mar 2021 · In Moss & Lichen since 2021". Then a 28 ghost row: network icon 16 (colour) + "Open in Discord" + `external-link` 14. Bottom padding 16. |

### Per-network versions (frames P1–P6)
| | Discord friend | Slack colleague | Telegram contact | Teams colleague |
|---|---|---|---|---|
| Band | accent colour | none | peer colour | none |
| Line 4 | `@juno.c · they/them` | `@tess.o · she/her` | `@nadia_k` | "Product Designer" |
| Presence | Online · 🌿 foraging all weekend | Away · 🎧 Heads down · until 5 PM | last seen today at 8:12 PM | Busy · reviewing Q4 plans |
| Heads-up | — | External: tag, no banner | — (scam: banner) | Out of office until Mon + note |
| Right now | In voice; Playing | In a huddle; local time | — | In a meeting · until 3:30 PM |
| Role | nickname + roles (from a server only) | Staff Engineer, Payments | — | title · Design Systems |
| About | bio + note (P2) | — | bio | — |
| Contact | — | email; phone masked | phone masked | email |
| In common | 3 servers | — | 4 groups ›(+1) | — |
| Shared | cache + "Search all on Discord" | files complete on click; links cache | exact counts, full list | cache only, "Open in Teams" |
| Foot | since / friends since / in server since | Open in Slack | Open in Telegram | Open in Teams |

| | Signal | Messages | Tiny server |
|---|---|---|---|
| Notifications | "Set on your phone" | "Stays on your phone" | "Not available on Tiny" |

The other rows for these three follow the same rule as the four: a section is drawn only with a value. Bands and role chips were not re-specified here.

A Tiny server member's name in the server page menu opens `tiny/menus.rs` `Modal::Profile`. That modal is the server page. It is not this right-hand profile pane. Do not merge the two.

**Bot / app** (P5): tag "App", no dot, no presence line unless it has a status; actions Message (if DM-able) and "…";
About = its description; In common; foot "In Moss & Lichen since Apr 2024". No Call, no Contact, no Right now.
**Sparse person** (P6, a Telegram user with no username, bio or phone): head, presence words, actions, Notifications,
Shared (with its honest empty state), "Open in Telegram". It reads complete because the head and actions are fixed and
everything below is earned; there is never an empty labelled box.

## 3. Actions
Row: **Message** primary (flex, `message-square`), **Call** secondary (flex, `phone`), "…" icon button 28. One primary.
- In their DM: no Message; Call becomes the only flex button; Notifications row below carries mute.
- Call hand-off: label "Huddle in Slack" / "Call in Teams" + `external-link` 14 trailing; no network calls: absent.
- Pending request (Discord): **Accept** small primary + **Ignore** small secondary replace Message and Call.
- "…" menu (`polish/01` menu, 220): Mute (when not the DM) · — · Copy username · Copy link · Copy ID (⌥ only) ·
  Open in Discord · — · Add friend (P2) · **Remove friend** · **Block** · **Leave group** (`danger`, last, after a
  separator). Each destructive item opens a 360 modal: "Block Lior Ashby on Discord?", one line on what follows,
  Cancel · Block (danger). Nothing in the menu writes to a network without that click.

## 4. Shared
- Tabs 32 (01 §3): Media · Files · Links · Pinned (P1; hidden on Teams chats). A tab with nothing in any source is still
  shown; Pinned hides when the network has none. Count 11/500 `text_faint` tabular 4 after the word, **only** when the
  answer carries `total` (Telegram at open, Discord/Slack after a server search).
- Source line 12/16 `text_faint`, 8 below the tabs: "From messages loaded here · 14" (cache) or "All on Telegram"
  (complete). Cut-off lists end with **Search all on Discord** (secondary, full width, `search` 16) — one request, on
  click only; then the line reads "All on Discord · 37". Teams: "The rest is in Teams" + **Open in Teams**.
- Media: 3 columns of 88 square tiles, gap 2 (fills 268), radius 4 outer corners only via the group's radius 8 clip;
  video: 11/500 duration chip bottom-left on `media_label`. Click opens the viewer (`03`) at that item, arrows walk the tab.
- Files: two-line rows 48: 32 `raised` tile radius 6 with the extension 11/600, name 13, "184 KB · Ines · Oct 2" 12.
- Links: same rows: 32 tile with `link` 16, title 13 (else host + path), host 12 `text_soft`.
- Pinned: rows with avatar 20, name 12/600, text 13 two lines.
- Empty from cache: no centred empty state; one 13 `text_soft` line "None in the messages loaded here" + the search
  button. Empty and complete: "No files in this chat". Never "yet", never "shared here" for a partial list.

## 5. Conversation details ("Details", from the header's `info`)
| | Group DM | Channel (Discord, Slack) | Telegram group / channel | Teams channel |
|---|---|---|---|---|
| Head | group picture or two faces at 64; name or "Ines, Dita, Haruto"; "Group · 4 people" | 64 place tile (`raised`, radius 16, `#` 28 or lock); `#trail-cams`; "Moss & Lichen › Trail" | photo 64 round; name + verified; "@riverbend_news · Channel · 12,408 subscribers" / "Group · 214 members · 18 online" | tile; "Design reviews"; "Contoso Design › Design reviews" |
| Marks | — | Private (lock tag), Shared (Slack Connect: "External · Northwind"), Read-only | Public / Private | Shared |
| Actions | Call · "…" | Join voice (voice channels) · "…" | "…" | "…" |
| Notifications | switch | segmented All · Mentions · Nothing (row: label left, segmented right) | switch | segmented where Teams allows |
| About | — | topic / purpose in full, 6 lines then "more" | about, links | description |
| Members | inline person rows (36, avatar 24) ≤ 10 | row: 5 faces 20 overlapped 6, "Members 128 · 12 online", chevron → pushes Members | count row when listable, else count stays in the head | row "Members 46" |
| Shared | tabs | tabs (+ Pinned) | tabs, exact counts | Files, cache |
| Foot | "Created Mar 2024" | "Created by Dev Patel · Mar 2024" | — | — |
| "…" | Copy link · Leave group (confirm) | Copy link · Open in Slack | Copy link · Leave (P2, confirm) | Open in Teams |

**Thread**: no details of its own. The thread strip's subtitle gains up to 5 faces 20 (overlap 6) + "4 people" 13
`text_faint` after "6 replies"; a face opens the card (P2).

## 6. Space details (from the Space name above its tree)
Same frame, title "Details". Head: icon 64 rounded square (radius 16); name 16/600; line 4: Discord "1,204 members ·
● 230 online" (dot 8 `online` inline), Teams "Team · 46 members", Slack the workspace host "harborlabs.slack.com".
Then: Notifications (segmented, Discord only), About (description, Discord / Teams), Role ("Robin in Moss & Lichen" +
role chips, Discord), foot "You joined Jun 2021" + "Open in Discord". No invite, settings or leave.

## 7. The card
The pane's top third at the same 268 content width: 300 wide, `overlay`, radius 10, popover shadow, padding 16; the band
(when given) is the card's top 48 clipped by its radius. Order: picture, name + mark, handle, presence (2 lines max),
actions (Message + Call, or Call alone in their DM; no "…"), heads-up as one 12/16 line with glyph 14 (no banner), one of
Role or About (Role wins; 2 lines), In common as up to 5 Space icons 20 + "3 servers in common" 12. Hairline, then a 40
foot: **View profile** ghost (`panel-right` 16) left, network icon 16 right. View profile pushes the pane with the same
answer (no second request) and closes the card. No Contact, no phone, no email, no bio over 2 lines on a card.
- **Anchor**: right of the clicked avatar or name, 8 gap, top aligned with its top; flips left past W − 8; then shifts
  up to stay 8 from the bottom; at H − 16 the body scrolls. From the pane (member rows) it opens to the pane's left.
- **Top never moves after the first frame**: the answer only extends the card downward (to max height).

## 8. Loading, memory, staleness
- First frame: strip, band slot, head, actions and Notifications from the row. The answer fills sections 8–15 in one
  update; until then nothing is drawn below the actions, and after 150 ms the region shows static `hover` skeletons in
  the commonest geometry (three bars 10 high at 70 / 90 / 50% and a 3-tile row). Shared waits for the profile (or 1 s)
  so the answer appends rather than inserts. No spinner, no fade.
- 5-minute memory per person in RAM: card → pane, pane → reopen draw complete in the first frame. Live presence,
  status and Right now update in place (a Right now row may appear; it is the user's world changing, not loading).
- Network unreachable: the banner "Discord is offline · showing what's on this Mac" sits as the heads-up (it outranks
  the others); profile sections are absent, Shared shows cache; Call disabled with tooltip "Discord is offline".
- Request failed (network up): one 12 `text_soft` line where the sections go, "Couldn't load the rest" + small ghost Retry.

## New tokens
| Name | Value | Use |
|---|---|---|
| `band(c)` | `over(c @ 40%, sidebar)`; on the card `over(c @ 40%, overlay)` | person / Space accent band |
| `BAND` | 48 | band height; head offsets fixed with or without it |
| `AVATAR_PROFILE` | 64, dot 14 ring 2 (overrules `05`'s 80) | pane and card |
| `TAG` | 16 high, radius 4, padding 5, 11/500 | Bot, App, Guest, External, Scam, Fake, Private |
| `CHIP_ROLE` | 20 high, radius 4, `hover` fill, 8 dot | role chips |
| `TILE` | 88, gap 2, 3 columns at 300 (recomputed from width: floor((w−32−4)/3)) | media grid |
| `media_label` | black 60% | duration chip on a tile |
| `CARD` | 300, max H − 16 | profile card (was 320) |
Glyphs: `badge-check`, `shield-alert`, `ban`, `plane`, `volume-2`, `gamepad-2`, `music`, `headphones`, `calendar`,
`clock`, `mail`, `phone`, `link`, `file`, `panel-right`, `bell-off`, `log-out`, `external-link`.

## GPUI notes
- The person body is one `div().overflow_y_scroll()` with a tracked `ScrollHandle`; Shared lists past 60 items move into
  a `list` below the fold. Head geometry is absolute (`.absolute().top(px(16.))`) inside a relative 176 box so the band's
  presence or absence changes nothing below it.
- Line clamp: `.line_clamp(4)` where this GPUI has it; else measure with `text_system().shape_line` once per width.
- Mask: format digits outside the last two as `•` in the hub, so the full number lives only in the `Profile` value
  held by the pane; unmasked state is a per-pane bool reset in `show_info`.
- Card placement: `anchored().anchor(Corner::TopLeft).snap_to_window_with_margin(px(8.))` with `AnchoredFitMode::SwitchAnchor`;
  its height is capped with `max_h(window_h − 16)` so growth scrolls inside instead of moving the top.
- Copy via `ClipboardItem::new_string`, then the shell's toast. Shared tabs are the 01 §3 tab bar, not the segmented switch built today.
- One `Profile` request per open (pacing per `06`); `space: Some(key)` only when opened from that Space's tree or member list.

## Deliberately absent
Banner images (P2), badges (Nitro, boosts), connected accounts, stories, birthday, mutual friends on Discord, manager and
org chart, working hours, the user ID as text, a "Discord" chip in the pane head (the strip and foot say it), "Friend" as a
line, Message beside its own DM, email masking (rulings mask phone only), role-coloured names, invite / settings / leave
for a Space, a spinner or shimmer, "N/A", "No status shared", relative times ("2 h ago"), a profile tab bar.
