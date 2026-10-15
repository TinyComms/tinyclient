# 04 · Composer and message actions

Everything I do to write a message and to act on one. Mockups: `mockups/04-composer-actions.html` (frame numbers below refer to it). Requirements: `product/03` stories 9, 10, 14, 17–21, 26, 29, 30 and `product/04` S1–S4. Row, chip and quote *looks at rest* belong to the conversation view; the popover, menu and modal shells belong to the visual system. Sizes are px at 1x. Body text is 14/20.

## Decisions

1. **One editor.** Editing a sent message happens in the composer, not in the row. One text entity, no row that changes height inside a virtual list, same behavior in a thread pane.
2. **Plain text with visible markup.** No WYSIWYG. `**bold**`, `_italic_`, `~~strike~~`, `` `code` ``, fenced blocks, `> quote`, `- list` stay as typed, with the markup characters in `text_faint`. One dialect on every network; the adapter writes each network's own.
3. **Text sends optimistically; attachments hold the composer.** Text-only: the row appears at once and the field empties (S1). With attachments: the composer locks and shows progress until the upload ends, so a failure leaves text and chips exactly where they were (story 21).
4. **Delete asks once; no undo toast.** No network can restore a message, and an undo toast means secretly delaying the delete while others still read it.
5. **Offline does not disable the composer.** Return still works (S3). Only signed-out, read-only and no-permission replace it.
6. **Escape never clears typed text.** It closes, then cancels, in the order of the key table.

## Composer anatomy (frames 1–6)

Dock: `px 20`, `pb 16`. Above it the 20 px typing slot (live signals), always reserved, so the message list never moves when someone starts typing. Order inside the card, top to bottom: mode bar, attachment tray, input row. One caption line may sit under the card (8 px gap, 12/16).

| Part | Spec |
| --- | --- |
| Card | `raised` fill, 1 px `hairline`, radius 14, min height 46. Focused: border `focus_ring`, no glow. |
| Input row | Left pad 8, right pad 7. Children bottom-aligned, 7 px from the bottom edge. |
| Attach | 30 x 30, radius 8, paperclip 16 `text_soft`; hover `hover` fill. Absent where the session cannot upload. |
| Text | 14/20, `text`; placeholder "Message #platform" / "Message Mara" / "Reply in thread" in `text_faint`. 13 px top and bottom inside a 46 px card (12 in the mockup plus borders). |
| Emoji | 30 x 30, radius 8, smile 16. `selected` fill while the picker is open. |
| Send | 32 x 32 circle. Empty: `raised`, glyph `text_faint`. Sendable: `accent`, glyph `on_accent`. Edit mode: check glyph. Uploading: `selected` fill, stop glyph. |
| Growth | One line = 20 px. Grows to 8 lines (card 186), then scrolls inside; 4 px thumb in `selected`, 3 px from the right, shown only while overflowing. The caret line is always scrolled into view. |
| Mode bar | 32 px, `px 12`, bottom hairline, 12 px text. Glyph 14 `accent`, label, snippet in `text_faint` (one line, ellipsis), 24 px close button. |
| Tray | Pad `10 10 0`, gap 8, one row, scrolls sideways past the card width. Max files from the session (10 if unknown). |
| Image chip | 64 x 64, radius 8, cover-cropped, decoded at 128 px. Remove: 18 px circle, `rail` at 85%, top right, inset 3. |
| File chip | 296 x 64 max (min 160), radius 8, `raised`: 36 px type tile, name 13/18 (middle ellipsis), size 12/16 `text_faint`. |
| Refused chip | Border `danger`, fill `danger_soft`, second line "Too big for Discord (limit 20 MB)". Checked at staging, no request made. Send disabled; caption "Remove *name* to send. It is 84 MB." |
| Uploading chip | 45% dark overlay, percent 11 px centered low, 3 px `accent` bar on the bottom edge. Determinate, driven by bytes; nothing moves unless bytes move. |
| Length | Counter appears right of the caption at 90% of the session limit: "1,912 / 2,000" `text_faint`; over the limit `danger` and Send disabled. |

**States.**

| State | What it looks like and says |
| --- | --- |
| Rest / focused | Frames 1, 2. Opening a conversation focuses the composer; typing anywhere with no field focused goes to it. |
| Reply (frame 4) | Bar: reply glyph, "Replying to **Mara Okafor**", snippet. Snippet click jumps to the message. Absent on Slack (no inline reply; use the thread). Telegram and Discord: `R` on another message swaps the target. |
| Edit (frame 5) | Bar: pencil, "**Editing message**", hint "Return to save · Esc to cancel". Row tinted `accent_soft` with a 2 px `accent` left bar and kept on screen. No paperclip (no network adds files on edit). The unsent draft and its chips are stashed and restored on save or cancel. Saving unchanged text is a cancel. Saving empty text opens the delete confirmation. Refusal ("Too old to edit") shows as the caption in `danger`; the row is unchanged. |
| Uploading (frame 3b) | Text and buttons at 40–60%, field read-only, caption "Uploading 2 of 2 · 3.1 MB of 5.4 MB · Esc cancels and keeps the draft". |
| Offline (frame 6) | Composer fully usable. Caption in `warning` with wifi-off 12: "Slack is offline. A message sent now waits up to 60 seconds for it to come back." Attach is disabled while offline with tooltip "Files need a connection". |
| Signed out | Card replaced by a 46 px outlined bar (no fill): lock 14, "Discord is signed out.", primary "Sign in" button 32 px. |
| Read-only / no permission | Same bar, one sentence from the network's reason: "Only admins can post in #announcements." / "You left this group." / "Mara has blocked messages." No button. |
| Slow mode / timeout | Same bar with clock 14: "Slow mode. You can send again in 8 s." The number changes once a second and only while this state shows. The draft is kept and returns with the composer. |
| Refused send | Caption in `danger` with the network's reason; draft intact. |
| Drop target | Dragging files over the conversation pane: 2 px `accent` inset border, `accent_soft` wash, centered label "Drop to attach to #platform". Dropping stages; it never sends. |

## Keys

| Key | Where | Does |
| --- | --- | --- |
| Return | Composer | Send, or save the edit. Completes the selected row if a completion list is open. During IME composition it only commits the composition. |
| Shift-Return, Opt-Return | Composer | New line. Inside a fenced block plain Return is also a new line; Cmd-Return sends. |
| Cmd-Return | Composer | Always send. |
| Up | Empty composer | Edit my last message in this conversation. In text: move the caret; on the first line, to the start. |
| Up / Down, Ctrl-P / Ctrl-N | Completion list | Move selection (wraps). |
| Tab, Return | Completion list | Complete. Tab with no list inserts nothing and moves focus. |
| Escape | Composer | First that applies: close completion list or picker; cancel upload; cancel edit; cancel reply; move focus to the message list. Never clears text. |
| Cmd-V | Composer | Text: paste as plain text, newlines kept. Image: stage "Pasted image 9.41.12.png". Files copied in Finder: stage them. |
| Cmd-U | Composer | System file panel, multiple selection. |
| Cmd-E | Composer | Open or close the emoji picker. Ctrl-Cmd-Space opens the macOS picker. |
| Cmd-B / Cmd-I / Cmd-Shift-X / Cmd-Shift-C | Composer | Wrap the selection in `**`, `_`, `~~`, `` ` `` (toggle; no selection inserts the pair around the caret). |
| Cmd-Z / Cmd-Shift-Z | Composer | Undo and redo typing, coalesced by word. |
| Cmd-A, Cmd-C, Cmd-X, Opt and Cmd arrows, Ctrl-A/E/K | Composer | Standard macOS text behavior. All are required. |
| R / T / E / U / Backspace / Cmd-C | Focused message | Reply, thread, edit, mark unread from here, delete, copy text. |
| `+` or `:` | Focused message | Add reaction (opens the picker). |
| Return / Esc | Delete dialog | Delete / cancel. |

## What the input must do (GPUI: custom element)

Soft wrap at the field width; caret and selection across wrapped and hard lines; click, drag, double-click word, triple-click line; arrow, word and line motion; IME marked text and the system emoji picker through the same input handler `input.rs` already has; undo stack; internal scroll. **Atomic tokens:** a completed mention or channel is stored as a range with an id and drawn as `accent` text on an `accent_soft` 4 px-radius pill; the caret cannot enter it, one Backspace removes it whole, copy yields its label. Custom emoji are the text `:name:` tinted `text_soft` (no inline images in the field). Unicode emoji are ordinary characters. It does not: render bold or italic, show images inline, spell-check with underlines (P2 through `NSSpellChecker`), auto-correct, auto-pair brackets, convert `:)` to an emoji, or turn a long paste into a file.

## Autocomplete (frame 7)

One popover component for `@`, `#` and `:`. Anchored 6 px above the card, left edges aligned, 340 wide, `popover` fill, radius 12, pad 4, `shadow_pop`. Rows 40 px, radius 8, selected row `selected`; section labels 26 px, 11 px caps `text_faint`. At most 8 rows, then it scrolls. The first row is selected on open. No open or close animation.

| Trigger | Opens | Row | Ranking |
| --- | --- | --- | --- |
| `@` at a word start | At once, with recents | 26 px avatar with a 10 px presence dot, name 13 semibold, secondary 12 `text_faint` ("handle · spoke 9:36 AM" or team/role) | People who spoke here, newest first; then members by display-name prefix, handle prefix, word prefix, substring. Then `@here`, `@channel`, `@everyone`, roles and groups where the network has them, with "notifies 14 people". |
| `#` at a word start | At once | Channel avatar 26, name, server or workspace as secondary | Channels of this server or workspace only: recently opened, then prefix, then substring. Absent in Telegram and in DMs outside a workspace. |
| `:` plus 2 letters | After the 2nd letter | Glyph 22 (or the custom image), `:name:` | My recents, then prefix, then keyword match; this workspace's custom emoji ranked with the rest. `:name:` typed in full with the closing colon converts without the list. |

**Lookup in flight.** Local matches draw in the same frame as the keystroke. If the network is also being asked (Slack directory, Discord member search if the owner allows it: product 03 question 3), the last line of the popover is static 12 px text, "Searching Discord for “dev”…". Results append under a "More people" label; rows already shown never move or reorder, so Return always completes what I was looking at. No results: "No one else matches". No spinner; a keystroke-driven lookup is not idle, but text is enough and nothing needs a frame clock. Queries are debounced 150 ms and cancelled by the next keystroke. Space after no match closes the list.

## Emoji picker (frame 8)

352 x 420 popover, anchored 6 px above the card, right edges aligned (from a message: beside the toolbar, flipped to stay in the window). Top to bottom: search field 32 (focused on open, pad 10); category tabs 32 (Recent, this workspace, then eight Unicode groups; 2 px `accent` underline on the current one); grid; footer 40 with the hovered emoji at 22 px, its `:name:`, a "⌃⌘Space" link that closes this picker and opens the macOS one, and the skin-tone button 26.

- **Grid.** 9 columns of 36 px cells, glyph 23 px, hover and keyboard cell `selected`, radius 8. Section labels are 28 px rows. Arrow keys move, Return inserts, Shift-Return inserts and keeps the picker open, Esc closes. Search replaces the grid with one flat ranked result list.
- **Recent.** 18 (two rows), per account, most recent first; also feeds the toolbar's three slots.
- **Custom.** Only the open conversation's workspace or server, under its name. Other Discord servers' emoji appear only if the session says the account may use them. Animated ones and ones limited to roles the account lacks are left out without Nitro.
- **Reacting.** The picker shows only what the conversation's rules (`Reactions`) take: each section keeps only its allowed emoji and drops out when none are left, a listed emoji the table lacks goes under More, and the footer says "Only the reactions allowed here" where the network has a list of its own. Where a listed emoji takes no skin tone (Telegram, Teams) the tone button is absent and no tone is applied.
- **Skin tone.** One global choice: the footer button opens a row of six swatches. No per-emoji long-press.
- **How it stays cheap.** Glyphs come from Apple Color Emoji through the text system: no sprite sheet, no image set. The catalog is a static table in the binary (about 1,900 entries, names and keywords, under 100 KB). The grid is one `uniform_list` of rows, so about 10 rows exist at a time. Custom emoji decode at 48 px only while their row is visible and are dropped when it scrolls off. The picker entity is created on open and dropped on close. Budget: under 4 MB while open, zero when closed. Animated custom emoji play as GIFs do (at most 2 MB of frames each), and their frames go when their row scrolls off.
- **Zero-cost path.** Ctrl-Cmd-Space works from the first release because the field is a real text input client; the in-app picker is release 3.

## Hover toolbar (frame 9)

Shown for the row under the pointer and for the keyboard-focused row, with no delay and no fade. Height 30, pad 2, radius 8, `popover` fill, 1 px `hairline`, shadow `0 2 8 rgba(0,0,0,.35)`. Right edge 16 px from the pane's right edge; top edge 14 px above the row's top, so it straddles the boundary and never covers the hovered text. Buttons 26 x 24, radius 6, glyph 16 `text_soft`, hover `selected` and `text`. Tooltips after 500 ms with the key: "Reply  R".

Slots, left to right: three quick reactions (my three most used, then 👍 ❤️ 😂 🎉 🙏 👀, then what the conversation lists and what is on the message, keeping only what would go through on it; fewer, or none, where less is allowed), 1 px divider, Add reaction (absent where nothing more may be added), Reply (absent on Slack), Thread (only where the conversation has threads), Edit (my messages only), More. Hidden while a text selection is being dragged, while the row is pending or failed, and on system lines.

## Context menu (frame 10)

Right-click a message, or More. 248 wide, pad 4, rows 30 px with a 14 px glyph, 13 px label and the key right-aligned in `text_faint`. A row that does not apply is absent.

| # | Row | Present when |
| --- | --- | --- |
| 0 | Reaction strip: six quick reactions (32 px), as the toolbar picks them; React is absent where nothing more may be added | Reactions allowed here |
| 1 | Reply | Discord, Telegram, Teams chats |
| 2 | Reply in thread / Open thread | Slack, Teams channels; Discord when the message has a thread |
| 3 | Edit | My message, inside the network's edit window |
| 4 | Copy text (Copy selection when text is selected) | Always |
| 5 | Copy link | Slack, Discord, Teams channels, Telegram public chats |
| 6 | Copy image, Save image… | Right-click landed on an image |
| 7 | Pin / Unpin | The session may pin here (release P2) |
| 8 | Mark unread from here | Slack, Discord, Teams. Telegram: "Mark as unread" (whole chat) |
| 9 | Open in Discord | Always; the escape hatch for everything cut |
| 10 | Delete… in `danger` | My message; last, after a separator |

Separators after 0, 3, 6, 8 and 9. On a failed row the menu is only Retry, Copy text, Delete.

## Reactions

Add: toolbar slot, menu strip, the `+` chip at the end of a message's chips, or `+` on a focused message. Choosing closes the picker. A chip click toggles mine: count and outline change at once; if the network refuses, the chip reverts and a toast says why ("Couldn't react: That reaction isn't allowed here."), and the rules are asked again. Where the network allows one reaction per person, choosing another replaces mine. Hovering a chip for 500 ms asks who reacted (P2) and shows names in a tooltip.

What may be added follows the conversation's rules, asked when it opens (`Reactions`, kept five minutes by chatd), and chatd refuses anything they refuse before the network hears of it:

| Network | Emoji | Mine on one message | Different on one message | Off or narrowed when |
| --- | --- | --- | --- | --- |
| Telegram | Telegram's own list, or the chat's part of it; Premium-only ones and custom emoji with Premium | 1, 3 with Premium; a new one replaces the oldest | The chat's `reactions_limit`, else 11 | The chat turned reactions off |
| Discord | Any; the server's own custom emoji, every server's with Nitro, animated only with Nitro | No limit | 20 | No Read Message History: off. No Add Reactions: only reactions already there can be added to |
| Slack | The standard emoji chatd can name, in any tone; the workspace's custom emoji | 23 | 50 | |
| Teams | The emoji Teams has a key for here | No limit | No limit | |
| Signal | Any one emoji | 1; a new one replaces mine | No limit | |

Where nothing may be added (reactions off, only joining allowed, a message at its cap) the toolbar's Add reaction, the menu's React and the `+` chip are absent; the `+` chip stays laid out so the row keeps its height. A chip that cannot be joined is not clickable and its tooltip says why. A chatd that does not answer `Reactions` leaves everything offered, as before.

## Delete (frame 12)

Modal 400 wide, radius 12, pad 20, over `scrim`: "Delete this message?", one line naming the consequence ("It is removed for everyone in #platform. Discord cannot bring it back."), the message quoted (sender, time, two lines), Cancel and a `danger` Delete. Focus starts on Delete. Telegram DMs add a checked box "Also delete for Mara". Shift-click Delete in the menu skips the dialog. The row disappears when the network confirms; a refusal leaves it and shows a toast. Deleting a failed or pending row is local and asks nothing.

## Send lifecycle (frame 11)

| State | Row | Then |
| --- | --- | --- |
| Pending | Body at 55%, a 12 px clock where the time goes. No toolbar. | Solid with its time when the echo arrives; the echo replaces the row, never adds one. |
| Waiting (offline) | Pending plus a 12 px `warning` line: wifi-off, "Waiting for Slack…" | Sent if the link returns within 60 s; otherwise Failed. Never sent later on its own. |
| Failed | Body `text_soft` at full opacity, 12 px `danger` line: alert glyph, "Not sent. You can't post in this channel.", then Retry and Delete (22 px, `raised`). | Retry returns to Pending. Delete removes the local row. Survives restart. |
| Upload failed | Not a row: composer unlocks with text and chips intact, caption in `danger` with the reason, failed chip outlined `danger`. | Send again retries only what did not upload. |

Sending scrolls the list to the bottom. A conversation with a failed row shows a `danger` alert glyph in place of its unread pill in the sidebar until resolved.

## Drafts (frame 13)

Per conversation and per thread: text, token ranges, reply target and staged file paths. Saved to SQLite 500 ms after the last keystroke and on switch; restored with the caret at the end. A conversation with a draft and nothing unread shows, in its preview line, a 12 px pencil and "Draft:" in `text_soft` followed by the draft in `text_faint`. Unread wins: the incoming preview and pill show, and the draft is still there on open. A staged file missing on restore is dropped with the caption "report.pdf is no longer on disk". Edit mode is not a draft; it ends on switch.

## Text selection and copy in messages

Required, and tested: drag selects by character inside a message body; double-click a word, triple-click the message; Shift-click extends; Cmd-C copies. Selection is stored as (message id, offset) pairs, not as element state, so it survives scrolling and rows being recycled, and a drag continues across messages while the list auto-scrolls. Copy gives plain text: mentions as `@Name`, custom emoji as `:name:`, links as their URL when the label differs, code blocks with their newlines; several messages copy as "Name  9:07 AM" then the text. Selection color `accent_soft`. A click without a drag still opens links and images. Code blocks keep a copy button; names, times and system lines are not selectable.

## New tokens

| Token | Value | Use |
| --- | --- | --- |
| `popover` | `#1d1e26` opaque | Toolbar, completion list, picker, menu, modal (use the visual system's if it defines one) |
| `focus_ring` | `accent` at 55% | Focused card border; outline of my reaction chip |
| `scrim` | black at 50% | Behind the delete modal |
| `shadow_pop` | `0 8 24`, black at 45% | Floating surfaces |
| `pending_alpha` | 0.55 | Body of an unsent row |
| `FONT_MONO` | `Menlo` | Inline code in the composer |

New Lucide glyphs: paperclip, smile, smile-plus, check, square, x, corner-up-left, message-square, more-horizontal, pencil, trash-2, copy, link, pin, mail, external-link, circle-alert, file, clock, lock.

## GPUI notes

- The multi-line input is a new element built on `input.rs`: shape with `shape_text` and a wrap width, cache line layouts per paragraph, repaint only on edit or caret blink. Height is `min(lines, 8) * 20 + 26` and is reported to the pane so the message list stays bottom-pinned.
- Floating surfaces are `anchored` + `deferred` elements with an opaque fill; no blur exists and none is drawn. They close on outside mouse-down and on window blur.
- The toolbar is drawn by the hovered row only (one instance), positioned absolutely with a negative top; the row must not clip its overflow.
- Paste reads `ClipboardEntry::Image`; pasted bytes go to a temp file and the chip holds a path, so the shell never keeps full-size pixels. Drop uses `on_drop::<ExternalPaths>`; the file panel is `prompt_for_paths`.
- The only timers: caret blink while focused, the slow-mode second while that bar shows, and upload progress events. With the window idle and unfocused, none run.

## Deliberately absent

Live formatted (WYSIWYG) editing and a formatting button bar. Escape-to-clear. An undo toast for delete. Disabled-but-visible controls for features a network lacks. A GIF picker, stickers, voice recording, scheduled send, slash commands and bot buttons (cut in `stories.md`). Inline custom-emoji images in the composer. Uploading at staging time (bytes leave only on Send). Background resend of old failed messages. Edit history. Per-emoji skin-tone long-press. A send sound. A character counter below 90% of the limit. Any spinner.
