# 03. The conversation view
How a message looks, on every network. Mockup: `mockups/03-conversation.html` (frame 1 the pane at 720 px, 2 the thread pane, 3 the image viewer, 4
states that cannot share a screen). Requirements: `product/03-message-content.md`, and R3, R5, S1 to S3 of `product/04-live-signals.md`. All sizes are
px at 1x. Token names are from `theme.rs`; new ones are listed at the end.

## Decisions
- **No bubbles, for anyone.** Every message, mine included, is a left-aligned row: avatar, name, time, body. Three of the four networks are
  channel-first, and a 2,000-character post, a code block, a 400 px image grid and a bot card all fit a full-width column and fight a bubble. A bubble
  layout would also need a second layout for channels, which breaks "one visual system". My own messages are marked by the name "You" and nothing
  else.
- **One density.** No compact mode. Rows are measured by a virtual list; a second set of metrics doubles every state here for a setting few people
  touch. The thread pane uses one fixed dense variant because it is 380 px wide, not because the user chose it.
- **Names are `text`.** No Discord role colors: the only brand or per-network color is the icon.
- **Media max is 400 x 300**, never upscaled, and every box size is computed from metadata.

## 1. The row
| Part | Spec |
| --- | --- |
| Row | full pane width, padding 3 / 20 (group start), 1 / 20 (continuation). 10 px transparent gap above a group start. |
| Avatar | 36 round, 2 px down from the row top. Apps and bots: radius 9 (squared), so an app never reads as a person. |
| Gutter | avatar + 12 gap. Body starts at x = 68. Continuation rows keep an empty 36 px gutter. |
| Header | height 20. Name 14 semibold `text`; 8 gap; time 11.5 `text_faint`, tabular ("9:07 AM", tooltip: full date). Then flags: `APP` tag, "Pinned". |
| Body | 14 / 21 `text`, max measure 620 (about 90 characters). Media and cards align to the body's left edge. |
| Hover | whole row `row_hover`. Continuation rows show their time in the gutter, 10.5 `text_faint`, right-aligned. The hover toolbar (composer area) floats over the top-right corner; the row reserves nothing for it. |
| One-line group start | 3 + 20 + 21 + 3 = 47 tall. |

- **A group breaks** when any of these is true: different sender; different day; more than 5 minutes since the previous message; a divider or system
  line sits between; the message carries a reply quote or forwarded card header; the previous one is a tombstone; the app identity differs (same bot
  user, different app name). Pending and failed messages group normally.
- **Row flags** (drawn as a 2 px inset bar on the row's left edge, plus tint): mentions me: bar `accent`, background `mention_row`. Failed send: bar
  `danger`, no tint. Jump target: background `accent_soft` fading to none over 1.2 s. Failed wins over mention.

## 2. Rich text
| Kind | Spec |
| --- | --- |
| Bold / italic / strike | weight 600 / italic / strikethrough with color `text_soft`. Underline is drawn where the network has it (Telegram, Teams). |
| Inline code | `MONO` 13, background `code_inline`, radius 4, 4 px side padding. |
| Code block | box `code_bg`, 1 px `hairline`, radius 8, max width 620, margin 5 top / 3 bottom. Header strip 28 tall, always present: language label 11 `text_faint` at left (empty when unknown), 24 px copy button at right (`text_faint`, `text` on hover; shows a check for 1.5 s after copying). Body `MONO` 12.5 / 19, padding 9 / 12, no wrapping, horizontal scroll. Over 20 lines: clipped at 20 with a 28 px footer "Show 64 more lines" (click expands; the user caused it, so the height change is fine). No syntax colors. |
| Quote | 3 px left bar `hairline_strong`, 12 px inset, text `text_soft`, margin 3 / 0. Nested quotes flatten to one bar. |
| Lists | marker column 22 wide in `text_faint` ("•" or "1."), text hangs. 22 px per level, three levels drawn, deeper levels flatten to the third. |
| Link | `link` color, underline on hover only, tooltip shows the full URL. Opens in the default browser. |
| Mention | user, channel, role, `@everyone` / `@here`: background `accent_soft`, text `link`, weight 500, radius 4, 3 px side padding. A mention of me: background accent at 32%, text white, and the row flag above. Click on a user opens the profile card (people area); on a channel, opens it. |
| Spoiler | text: a `spoiler` block with text in the same color, radius 4; click reveals (background `code_inline`) for as long as the row stays loaded. Spoilered media: the reserved box filled `media_fill` with "Spoiler · click to show" 12 `text_faint`; nothing is decoded until the click. |
| Emoji | system emoji font at text size. A message that is only 1 to 3 emoji draws them at 40 / 48 (jumbo). 4 or more: normal size. |
| Custom emoji | 18 x 18 image, radius 0, centered on the 21 px line. Jumbo rule applies (40 x 40). Until the image is on disk, and always in P0: the literal `:name:` in `text_soft`. |
| Edited | "(edited)" 11.5 `text_faint`, appended to the last text run so it never adds a line; on its own 16 px line only when the message ends in a block. Tooltip: edit time. |

## 3. Attachments
Stack under the body: 5 px above the first, 4 px between. `W` = min(400, body width).

- **Single image.** Scale = min(W / w, 300 / h, 1). Radius 8, 1 px inset `hairline` so dark pictures keep an edge. Clamps: if the scaled width is under
  120 (very tall), the box is 120 x 300, cover crop from the top; if the scaled height is under 64 (very wide), the box is W x 64, cover crop centered;
  if either natural side is under 48, natural size centered in a 48 x 48 `media_fill` box. Width or height missing from the network: a fixed 240 x 180
  box, contain fit, never resized later.
- **Grids.** Width W, 4 px gaps, every tile cover-cropped, radius 8 each. Sizes at W = 400:

| Count | Layout |
| --- | --- |
| 2 | two 198 x 198, side by side |
| 3 | one 264 x 264 left; two 132 x 130 stacked right |
| 4 | 2 x 2 of 198 x 148 (total 400 x 300) |
| 5+ | as 4; the fourth tile is covered by black at 60% with "+N", 22 semibold white, N = count - 4 |

- **States, all in the same box.** Loading: the network's tiny placeholder (Discord `placeholder`, Slack `thumb_tiny`, Telegram `minithumbnail`)
  stretched to the box, else flat `media_fill` with a 20 px image glyph in `text_faint`. No shimmer, no spinner. Loaded: the bitmap replaces it with a
  120 ms opacity fade. Failed: `media_fill`, image-off glyph, "Couldn't load image · Retry" 12. Withheld by the network (Slack plan limit): the
  unsupported line, "Image hidden by Slack's plan".
- **GIF.** Still frame; "GIF" badge bottom-left (height 20, radius 5, `badge` fill, 10.5 bold white); 44 px `badge` disc with an 18 px white play glyph
  in the center. A GIF that is an MP4 (Discord `gifv` from Tenor and Giphy, Telegram animations) is a video here: built today, a click plays it in
  the system's player, as a video. Planned: it plays in place in the same box, muted and looping, and stops when off screen or the window loses
  focus.
- **Video.** Poster (the network's still), the same disc, duration badge bottom-right ("1:24"); a round video message (Telegram) is a circle. Click
  fetches the file (the badge reads "Loading…") and plays it in the system's player (QuickTime on macOS, the default app on Linux and Windows), which
  decodes with the system's codecs and has its own sound, scrubbing and full screen. It is opened through a link under the attachment's own name in
  `open/` in the support directory, because the cache's file names have no extension. Every moving tile goes through one function,
  `Pane::moving` (`gui/conversation/video.rs`), the one place an inline player would go.
- **File row.** min(360, body width) x 56, radius 10, `raised`, 1 px `hairline`. 36 px icon tile (radius 8, `raised`, 18 px Lucide glyph in `text_soft`
  chosen by type: text, archive, sheet, audio, video, code, generic). Name 13.5 medium, one line, truncated; meta 11.5 `text_faint` "PDF · 2.4 MB" (the
  type is repeated here because truncation eats the extension). 28 px button at right. Idle: download glyph. Downloading: meta "7.1 of 18.6 MB", 2 px
  `accent` bar on the bottom edge, button is cancel. Downloaded: meta adds "Downloaded", button is Quick Look; click or Space opens Quick Look,
  double-click opens the default app. Failed: meta "Couldn't download · Retry" in `danger`.
- **Voice message** (an audio attachment with the network's waveform: Discord, Telegram, Slack clips). min(360, body width) x 48, radius 10,
  `raised`. 32 px round play / pause button (`accent_soft`, glyph `accent_text`); waveform of 40 bars, 2 wide, spread across the space left, 3 to 20 tall,
  `text_faint`, played part `accent`; press or drag on it to seek. Time 12 `text_soft`: the length at rest ("0:47"), "0:14 / 0:47" once started,
  "Loading…" while the file is fetched, "Couldn't play · Retry" in `danger` with the reason on hover. Then a speed chip (20 high, radius 4, `hover`
  fill, 11 semibold): 1×, 1.5×, 2×, pitch kept; the last speed picked is where the next message starts. First click fetches, then plays; one file
  plays at a time, across every conversation.
- **Audio file** (an audio attachment with no waveform: music, Signal's and most phones' recordings). min(360, body width) x 56, the same box and
  button; the name 13 medium over a 4 pt bar (`hover` track, `accent` played part, press or drag to seek) with the time beside it; the speed chip.
- **Now playing.** Whatever plays keeps playing while the user scrolls or goes elsewhere. Above the call dock, in every sidebar mode: a 40 pt
  `raised` bar with play / pause (28), the sender of a voice message or the file's name, "0:14 / 0:47" (or why it failed, in `danger`), and a ✕ that
  stops it. Clicking the words opens the conversation. It goes when the file ends or is stopped.
- **Sticker.** 128 x 128, no box, no border. Animated stickers show their static thumbnail.

## 4. Link cards
Only from the network's own preview data. At most **three** cards per message; further links stay as text. Card: min(440, body width) wide, radius 10,
`raised`, 1 px `hairline`, padding 10 / 12. Site 11.5 `text_faint`; title 14 semibold, 2 lines; description 13 / 18 `text_soft`, 2 lines.
- **Compact** (default): text left, 72 x 72 thumbnail right, radius 6, cover.
- **Large**: only the first card, only when the preview image is at least 400 wide with aspect 1.3 or wider. Image under the text at 400 x 210 max,
  cover, radius 8. Video previews add the play disc.

All cards: no proxied image means a text-only card. No favicon. Clicking a card opens the link.

## 5. Around the message
- **Reactions.** Row under the content, margin 6 / 3, wraps. Chip: height 26, radius 13, padding 0 / 8, emoji 15 (custom emoji 18 x 18), count 12
  medium tabular `text_soft`, fill `raised`, 6 px gaps. Mine: fill `accent_soft`, 1 px `accent` border, count `link`. Hover: 1 px `hairline_strong`
  border; tooltip lists who (fetched then). At most 12 chips, then a "+7" chip that expands the row. A final add chip (smiley) is always laid out and
  is invisible until the row is hovered, so hovering never changes the height; where nothing more may be added (04, Reactions) it never
  shows. A chip the conversation will not let me join is not clickable and its tooltip says why.
- **Reply quote.** One 20 px line above the header, starting at x = 68: 16 px avatar, name 12.5 semibold `text_soft`, snippet 12.5 `text_faint`,
  truncated. An elbow (2 px `hairline_strong`, top-left radius 6) joins it to the avatar column. Media-only target: glyph and "Photo" / "File".
  Deleted target: italic "Original message was deleted". Hover: snippet `text`. Click scrolls to the target and flashes it (row flag above), loading
  history around it if needed.
- **Thread summary.** Under the content: height 30, radius 8. Up to three 20 px avatars overlapped by 5 with a 2 px `canvas` ring, "4 replies" 13
  semibold `link`, "Last reply 2h ago" 12 `text_faint`. Unread replies add a 6 px `accent` dot and "2 new". Hover: `raised` and `hairline`.
- **Forwarded.** Header line 18 tall: forward glyph 13, "Forwarded from Nadia Brandt in #support" 12 `text_faint`. The forwarded content sits under it
  behind a 2 px `hairline_strong` bar, 12 inset, at normal sizes. The forwarder's own comment follows as ordinary body.
- **Deleted.** The row is removed. Exception: a root that still has replies becomes a tombstone, italic "Message deleted" in `text_faint` with a
  dimmed avatar, keeping its thread summary.
- **Pinned.** 12 px pin glyph and "Pinned" 11.5 `text_faint` in the header. No tint.
- **System line.** Centered, 28 tall, 12.5 `text_faint`, names in `text_soft` medium, optional 14 px glyph. Runs of the same kind merge: "Tomas, Nadia
  and 2 others joined". Calls: "Mara started a huddle · 14 min" (the join control belongs to the calls area).
- **Bot / app.** The normal row with the squared avatar and an `APP` tag (height 16, radius 4, `raised`, 10 semibold `text_soft`). Structured content
  is one card: min(440, body) wide, radius 10, `raised`, with a 3 px left bar whose color is the app's color snapped to `positive`, `danger`,
  `warning` or `hairline_strong`. Title 14 semibold; text 13.5 / 19; fields two per row (label 12 `text_faint`, value 13.5); image at attachment
  rules; footer 12. Controls are not drawn; the card ends with "2 actions · Open in Slack".
- **Unsupported.** One line, never blank: height 36, radius 8, `raised`, `hairline`; 16 px glyph by kind, "Poll: Lunch on Thursday?" 13.5 `text_soft`,
  then "Open in Telegram" in `link`.

## 6. Dividers and scroll
- **Day.** 36 tall: hairline, centered label 11.5 medium `text_faint` ("Today", "Yesterday", "Monday, October 12"), hairline. Not sticky.
- **New.** 24 tall: 1 px `accent` line ending in a "NEW" tab (height 16, `accent` fill, 10.5 bold `on_accent`) at the right edge. Placed at the first
  unread when the conversation opens and it stays there until I leave. On a day boundary it sits directly above the day divider.
- **Jump to bottom.** Floats 12 above the composer, 20 from the right, once I am more than 200 px from the bottom. Idle: 32 px circle, `float`,
  `hairline`, chevron. With new messages: `accent` pill "3 new" 12.5 semibold `on_accent`. Shadow 0 6 20 black 40%. Click goes to the New divider if
  it is below the viewport, otherwise to the bottom.
- **Older messages.** Fetch starts 600 px from the top. A 44 px row "Loading earlier messages…" 12 `text_faint` with a 14 px loader glyph that rotates
  only while the row is on screen. Failure: "Couldn't load earlier messages · Retry". Offline: "Earlier messages need Slack to be online."
- **Start of conversation.** When history is exhausted: 56 px avatar, title 20 bold, one line 13 `text_soft`: "This is the start of #launch-week.
  Created by Ines Vauquelin on March 4, 2026." For a DM: "This is the start of your conversation with Mara Okafor."

## 7. Image viewer
Opens over the whole window on a flat `scrim` at 94%. Top bar 52: 24 px avatar, name 13 semibold, "Monday 5:02 PM · #launch-week · hero-final.png ·
2400 x 1600" 11.5 `text_faint`; at right, 32 px buttons: zoom out, zoom label ("Fit", "100%"), zoom in, copy, save, Quick Look, close. The image is
fitted inside the window less 52 top, 72 bottom, 64 each side, never above 100% at fit.
- **Zoom:** click toggles fit and 100% at the cursor; pinch and Cmd-scroll from 10% to 800%; Cmd-0 fit, Cmd-1 actual; drag pans when zoomed.
- **Previous / next:** 40 px round `float` buttons at the edges, and the arrow keys, through every image and GIF loaded in this conversation in order
  (grid tiles included, videos skipped). Footer: "3 of 12 · caption" 12.
- **Keys:** Cmd-C copy, Cmd-S save to Downloads (a pill "Saved to Downloads · Show in Finder" for 3 s), Space or Cmd-Y Quick Look, Esc or a click on
  the scrim closes.
- **Loading:** the thumbnail is shown stretched at once; the full file replaces it; a 2 px `accent` bar under the top bar shows progress. Failure:
  "Couldn't load the full image · Retry".

## 8. Thread pane contents
One list, top-aligned: the root message in full as item 0 (it scrolls away; a pinned root would eat a 380 px pane), a 32 px count line ("4 replies"
11.5 `text_faint` plus a hairline), the replies with the same grouping, dividers and New rule, then the composer slot. Dense variant: inset 16, avatar
28, gap 10, so the body is 310 wide and W = 310. Thread summaries are not drawn inside a thread.

## 9. Send states at rest
- **Pending:** body and attachments at 60% opacity; header and time unchanged; no spinner. An uploading attachment has the 2 px `accent` bar on its
  bottom edge.
- **Waiting (offline, first 60 s):** as pending, plus an 18 px status line under the body: clock glyph 13 and "Waiting for Slack…" 12 in `warning`.
- **Failed:** body back to full opacity, `danger` row bar, status line under the body: alert glyph, "Not sent · Slow mode: 8 s" 12 `danger`, then
  "Retry" (semibold `link`) and "Delete" (`text_soft`).
- **Sent / read (Telegram and Teams direct chats):** on my newest message only, a 14 px check (`text_faint`, sent) or double check (`accent`, read) at
  the row's right edge, on the first line.

## New tokens
`row_hover` ffffff08 · `mention_row` 7f8cff14 · `hairline_strong` ffffff24 · `code_bg` 0e0f14 · `code_inline` ffffff14 · `media_fill` 1d1f27 (opaque)
· `float` 22242d (opaque floating surface) · `scrim` 07080b at 94% · `badge` 000000a6 (labels over media) · `spoiler` 34364a · `link` 9ea8ff (accent
lightened for body-text contrast) · `MONO` = "SF Mono" falling back to Menlo.

## GPUI notes
- **Row height before media.** One pure function `media_box(w, h, count, W)` returns every box in sections 3 and 4 from the metadata in `open`; rows
  are built with fixed-size divs and the bitmap is a child that arrives later. Nothing in a row is sized by decoded pixels.
- **Late content.** Link cards, edits and reactions arrive after the row is on screen and do change its height. At the bottom the list stays pinned to
  the bottom, as for a new message; scrolled up, the list anchors on the top visible item, so growth above the viewport moves nothing.
- **Rich text** is one custom element around `StyledText`: per-range weight, italic, color, strike and underline are text runs; rounded backgrounds
  (inline code, mentions, spoilers) are quads painted behind the range bounds from the text layout; a custom emoji is a placeholder run of fixed width
  with the image painted over it. Range clicks and hovers go through the same layout. A plain square run background is the fallback if the paint pass
  slips; heights are identical.
- **Blocks** (code, quote, list, cards, grids) are flex divs. The code block body is a horizontally scrollable div; its header strip is always laid
  out, so hover changes color only.
- **Images:** `img` from the path in `media/`, decoded at box size x scale factor, `ObjectFit::Cover` or `Contain`, corner radius on the element.
  Bitmaps are released when the list drops the row; the viewer holds one full image, decoded to at most 2560 px on the long edge (about 15 MB), 4096
  only past 100% zoom, and conversation thumbnails are released while it is open.
- **Hover-only things** (gutter time, add-reaction chip) are always laid out and toggle color or opacity. Avatar overlap is negative margin with a
  `canvas` ring. The reply elbow is a div with left and top borders. Viewer, jump pill and toast are absolutely positioned layers with shadows.
- **Animations:** image fade 120 ms, jump flash 1.2 s, the loader glyph. Nothing animates at idle.

## Deliberately absent
Bubbles; compact density; role colors; syntax highlighting; inline video (for now: the system's player); autoplaying GIFs; animated stickers; shimmer placeholders; backdrop blur;
favicons and any image not from the network's proxy; sticky day headers; a filmstrip in the viewer; per-message read receipts and "seen by" lists;
edit history; interactive bot controls; polls, tables and headings as real elements (they are the unsupported line until P2); network badges on
individual messages.
