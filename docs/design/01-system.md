# 01. Visual system
The tokens and base components every screen is built from. Sheet: `mockups/01-system.html`. All sizes are points (1 pt = 2 device px on the target display). Token names are the functions in `gui/theme.rs`; existing names are kept, values change, new names are marked **new**.

## 2. Tokens
### Color. Dark ships first.
Ratios are against `canvas` (dark). Light is in scope after increment 2 and follows the system appearance; it costs nothing later if components take color only from these functions, state fills stay tokens, and each theme carries its own shadow recipe. Light values not marked with a ratio are starting points to verify when it is built.

| Token | Dark | Light | Use |
|---|---|---|---|
| `sidebar`, `rail` | #121315 | #F4F4F6 | Navigation panes. One value: the rail is not a third stripe |
| `canvas` | #18191C | #FFFFFF | Conversation and content |
| `raised` | #1F2023 (opaque) | #F4F4F6 | Cards, code, banners, image placeholders, neutral avatar plate |
| `overlay` **new** | #25262A | #FFFFFF | Anything that floats: menu, popover, modal, toast |
| `hairline` | white 8% | black 8% | Pane and section dividers |
| `border` **new** | white 12% | black 12% | Control and overlay edges |
| `border_strong` **new** | white 20% | black 24% | Field hover, checkbox edge |
| `hover` | white 6% | black 4% | Pointer over a row or control; field and secondary-button fill |
| `selected` | white 12% | black 8% | Current row, menu highlight, toggled icon button |
| `pressed` **new** | white 16% | black 12% | Mouse down |
| `text` | #EDEEF0 · 15.1:1 | #1B1C1F · 17.0:1 | Names, bodies, unread titles |
| `text_soft` | #A8ABB3 · 7.7:1 | #565962 · 7.0:1 | Previews, read channel names, icons at rest |
| `text_faint` | #858993 · 5.0:1 | #666973 · 5.5:1 | Timestamps, placeholders, section labels. Lowest level that may carry meaning |
| `text_disabled` **new** | #53565E · 2.4:1 | #A9ACB4 | Disabled only |
| `accent` | #2F6FEB (white on it 4.6:1) | #2563EB | Primary button, mention badge, switch on |
| `accent_hover` / `accent_pressed` **new** | #3A77EE / #2861D3 | #1F5AD6 / #1A4FC0 | |
| `accent_text`, `focus` **new** | #8AB4FF · 8.4:1 | #1F5AD6 · 6.0:1 | Links, @mentions, "New" label, focus ring, caret |
| `accent_soft` | accent 18% | accent 12% | Mention chip; a message that mentions me uses accent 10% |
| `on_accent` | #FFFFFF (was dark) | #FFFFFF | |
| `positive`, `online` | #3FBF7A | #1F8F54 | Online dot, success icon |
| `warning`, `idle` | #E0A63A | #9A6A00 | Idle dot, warning icon. Never body text |
| `danger` | #F0655D · 5.6:1 | #C9302C · 5.3:1 | Destructive text and icon |
| `danger_fill`, `dnd` **new** | #D13F3F (white on it 4.7:1) | #C9302C | Destructive button on hover, do-not-disturb dot |
| `danger_soft` | danger 15% | danger 10% | Destructive button and menu item at rest or hover |
| `scrim` **new** | black 50% | black 30% | Behind a modal |

Semantic aliases, so they can diverge later: `mention` = `accent`; `unread` = `text`; `offline` = `text_faint`.

### Type. SF Pro, six sizes, weights 400 / 500 / 600 only.
| Size / line | Weight | Use |
|---|---|---|
| 20 / 26 | 600 | First run and sign-in headline |
| 16 / 22 | 600 | Pane title, modal title, empty-state title |
| 14 / 20 | 400, name 600 | Message body and sender name. Nothing else |
| 13 / 18 | 400, 500 controls, 600 unread | The interface size: rows, buttons, fields, menus |
| 12 / 16 | 400 | Row preview, helper text, tooltip, shortcut |
| 11 / 14 | 500, badge 600 | Timestamp, section label, badge numeral |
| code 13 / 20 | 400 | Inline code and code blocks |

No fractional sizes, no 700, no all-caps, no letter-spacing. Times and counts use tabular numerals.

### Space, radius, elevation
- **Spacing:** 2, 4, 6, 8, 12, 16, 20, 24, 32. Pane padding 16. Lists are inset 8 from pane edges.
- **Density:** controls and one-line rows 28; two-line rows 48; message lines 20 with 2 between messages of one sender and 12 between senders. A 720 pt window shows 13 conversations or 22 channels.
- **Radius:** 4 chips, inline code, checkbox, segment · 6 every control and row · 8 cards and media · 10 floating containers and the composer · 12 modal · full for people, badges, dots. Nested: inner = outer minus the padding between them (menu 10, pad 4, item 6; segmented 6, pad 2, segment 4). No control is a pill.
- **Elevation.** Every floating surface is `overlay` + 1 pt white 10% border + a shadow. Panes never cast one.

| Level | Shadows (x y blur spread, black) |
|---|---|
| tooltip | 0 2 8 0 @40% |
| popover, menu, toast | 0 0 0 1 @40%; 0 2 6 0 @30%; 0 12 32 0 @45% |
| modal | 0 0 0 1 @40%; 0 4 12 0 @35%; 0 24 64 0 @55%, over `scrim` |

### Icons
- Lucide, at 14 (inside 12 pt text), 16 (default) and 20 (rail, empty state). Ship one file per size with stroke-width 2.5 / 2.25 / 1.75 so the drawn stroke is 1.5 pt at every size.
- A glyph takes the color of the text beside it. It is `accent` only inside an accent control.
- Network icons are the real marks in full color at 14 / 16 / 20 / 32. Never tinted, recolored or put on a plate. Signed out or offline: 40% opacity. Shown once per context: account switcher, conversation header, "Open in Slack". A row shows one (14, leading the title) only in a list that mixes networks. It never sits on the avatar corner.

### Motion
Easing is ease-out for entering, linear for leaving. Reduce Motion sets every duration to 0.

| Animates | How |
|---|---|
| Menu, popover | opacity 0 to 1 and 4 pt toward its anchor, 120 ms. Leaves in 80 ms |
| Modal and scrim | opacity and 8 pt rise, 160 ms. Leaves in 100 ms |
| Toast | opacity and 8 pt rise, 160 ms. Leaves in 100 ms |
| Scrollbar thumb | fades out over 150 ms, 800 ms after scrolling stops |

Never animates: hover, press, selection, focus, tab and segment changes, tooltips (appear whole after 500 ms), a message arriving, badges, presence, typing ("Wren is typing…" is static text), skeletons, progress (the bar moves only when the value does), connection state. No spinner exists. Reason: the idle budget, and instant state change is what reads as fast. The one thing that breathes is a server page's loading mark (§3, Loading), because a screen with nothing on it yet has no other way to say it is working.

### Focus, targets, scrollbars
- **Focus ring:** 2 pt `focus`, drawn outside the edge, on keyboard focus only. On a filled button, a 2 pt gap of the surface color comes first. A focused text field shows it always, as a `focus` border plus a 3 pt halo of accent 35%.
- **Hit target:** 28 × 28 minimum. Anything drawn smaller (14 pt clear ×, 16 pt badge, reaction chip) pads its clickable box to 28, or to 24 inside a toolbar where targets touch.
- **Scrollbar:** overlay, no track, takes no layout width. Thumb 6 wide, radius 3, 3 from the edge, white 20%, minimum length 32; 8 wide and white 36% under the pointer.

## 3. Components
States are rest / hover / pressed / focus / disabled unless listed. Every hover is instant.

- **Button.** Height 28 (24 small, 32 for a dialog's main action), radius 6, 13/500, padding 12, icon 16 with a 6 gap and 10 on the icon side. Disabled: `hover` fill, `text_disabled` label. Busy: the label becomes words ("Sending…") at the same width.
  - *Primary:* `accent` / `accent_hover` / `accent_pressed`, white label. One per view.
  - *Secondary:* `hover` fill + `border`; hover `selected` + `border_strong`; pressed white 3%.
  - *Ghost:* no fill, `text_soft`; hover `hover` fill and `text`; pressed `selected`.
  - *Danger:* `danger_soft` fill, `danger` label; hover and pressed `danger_fill` with white.
  - *Icon-only:* 28 × 28 (24 in toolbars), 16 glyph `text_soft`; hover `hover` and `text`; pressed `pressed`; toggled on `selected`. Always has a tooltip.
- **Input.** Height 28, radius 6, white 4% fill, `border`, padding 8, 13/18. Placeholder `text_faint`. Hover `border_strong`. Focus as above; caret 2 pt `accent_text`; selection accent 45%. Error: `danger` border and a 12 pt `danger` line 4 below. Disabled: 50% opacity.
- **Search field.** The input with a 14 search glyph at 8 and text at 28. Empty and unfocused it shows a `⌘K` keycap (18 high, radius 4, `border`); with a query, a clear × in a 20 box.
- **List row.** Radius 6, padding 8. *One line:* 28, glyph 16, gap 8. *Two lines:* 48, avatar 32, gap 10, title 13/18 over preview 12/16; time 11 on the title line, badge on the preview line.

  | State | One line | Two lines |
  |---|---|---|
  | rest | title `text_soft` 400 | title `text` 400, preview `text_soft`, time `text_faint` |
  | hover | `hover` fill | same |
  | selected | `selected` fill, title `text` | same, time `text_soft` |
  | unread | title `text` 600, count badge | title 600, preview `text`, time `text_soft`, badge |
  | mention | as unread, mention badge | same |
  | muted | all `text_faint`; unread is a 6 pt faint dot, a mention still shows its badge | same |

- **Avatar.** Sizes 16, 20, 24, 32 (rows), 36 (messages), 64 (profile). A circle is a person; a rounded square (radius 25% of size) is a place. A 1 pt white 6% edge keeps dark images off the dark surface.
  - *Person:* image, else initials (one letter to 24, two from 32) at 0.4 × size, 600, on a flat tint from eight fixed hues (8, 32, 95, 150, 185, 215, 265, 325): fill hsl(h 30% 22%), letters hsl(h 70% 82%), about 7.5:1.
  - *Group:* its picture, else two member avatars at 66% size on the diagonal, the front one with a 2 pt ring of the row color.
  - *Channel:* `#` glyph 16 in `text_soft` on a `raised` rounded square. One-line rows use the bare glyph.
  - *Server or workspace:* its icon in a rounded square, else initials on the tint.
  - *Presence dot:* bottom-right, overhanging 2: diameter 8 on 20 and 24, 10 on 32 and 36, 14 on 64, none on 16, ringed 2 pt in the color behind the avatar (it follows hover and selection). Shape-coded: online solid `online`; idle `idle` with a bite out of the top-left; do not disturb `dnd` with a bar; offline a hollow `text_faint` ring.
- **Badge.** Height 16, min width 16, padding 5, radius 8, 11/600, `99+` past 99. *Count:* white 16% fill, `text`. *Mention:* `accent` fill, white. *Dot:* 8 pt `text` (unread), `accent` (mention), 6 pt `text_faint` (muted). On an icon it overhangs the top-right with a 2 pt ring.
- **Tooltip.** #2C2D32, `border`, radius 6, padding 4 × 8, 12/16, max width 240, 6 from the anchor, no arrow. An optional shortcut follows in `text_faint`.
- **Popover.** Elevated surface, radius 10, padding 12, width 200 to 360, 6 from the anchor, no arrow.
- **Context menu.** Radius 10, padding 4, width 180 to 280. Item 28, radius 6, padding 8, glyph 16 `text_soft`, label 13, shortcut 12 `text_faint` right. Highlight (pointer or keys) is `selected`, not accent. Destructive: `danger` text, `danger_soft` highlight. Separator: hairline, 4 above and below. Section label 11/500, height 24.
- **Modal.** 440 wide (360, 560), radius 12, padding 20, title 16/600, body 13/18 `text_soft`, 20 above the buttons, which sit right with 8 between and the main action last. A sheet is the same panel hung from the title bar with square top corners.
- **Toast.** Bottom-right, 16 from the edges, 340 wide, min height 40, radius 10, padding 12, optional semantic glyph 16, text 13, optional small ghost action. 4 s; 8 s with an action; errors stay until dismissed. At most three, 8 apart.
- **Inline banner.** Height 32, radius 6, `raised` + `hairline`, glyph 16 in its semantic color, text in `text`. This is how a pane says "Slack is offline".
- **Segmented switch.** Height 28, padding 2, radius 6, `hover` fill; segment 24, radius 4, padding 10, 13/500 `text_soft`; selected white 14% fill and `text`. Two to four choices.
- **Tabs.** Height 32, 16 apart, 13/500 `text_soft`; selected `text` with a 2 pt `text` bar on the hairline.
- **Switch, checkbox.** 32 × 18 with a 14 knob; 16 square, radius 4. On is `accent`.
- **Divider.** 1 pt `hairline`. Day: 11/500 `text_faint` label, centered, 12 from the rules. New: 1 pt `accent` rule with "New" 11/600 `accent_text` at its right end.
- **Loading.** Skeleton blocks are static `hover`-filled rectangles (radius 4, 10 high for a title, 8 for a preview) in the row's final geometry, shown only after 150 ms. An image is its final box in `raised`. Known progress is a 4 pt bar (white 10% track, `accent` fill). Unknown progress is a sentence. A server's own page, while it is still being read, is the one splash: the server's icon in its own colors at 56 (`network_icon`), 16, its name 20/600, 6, one line 14 `text_soft` (max 320), centered in the pane, as the first launch says hello. While it waits the mark breathes: its opacity 0.62 to 1 and back over 2.4 s, full at the start of each breath, and nothing else moves. Reduce Motion holds it at full. The column beside it is that server's own list as skeletons, at its rows' own heights, so the page shapes up where its content will land.
- **Empty state.** Centered: glyph 20 `text_faint`, 12, title 16/600, 4, one line 13 `text_soft` (max 320), 16, at most one secondary button. No disc, no illustration. The one exception is where the window speaks as tinyclient itself, with nothing open and on the first launch: the app's mark in its own colors at 56 (`brand_mark`, `assets/brand/tinyclient.svg`) in place of the glyph.

## 4. Craft rules
1. **One baseline per line.** Sender 14/600, time 11 and body 14 all sit on 20 pt lines; name and time align on the baseline. The 36 avatar starts 2 below the name line's top so it centers on the first two lines. *Misaligned small text is the first thing that reads as amateur.*
2. **Whole points, even boxes.** Every size, offset and line height is a whole point, and a centered child differs from its box by an even number (16 in 28, 14 in 28, 10 in 14). *Odd differences land on half pixels and blur.*
3. **Hairlines are 1 pt elements at low alpha**, a border or a 1 pt div on whole coordinates, never a shadow or a 0.5 pt line. *Crisp at 2×, still present on a 1× external display.*
4. **Separate with a step or a line, not both.** Panes: a hairline. Fields: a border. Only floating surfaces get fill, border and shadow together.
5. **Hover is a fill; selection is a fill plus brighter text.** Hover never changes weight, size or layout and never reveals something that shifts a neighbor.
6. **Color is rationed.** On any screen the colored pixels are avatars and media, network icons, presence dots, and accent. Accent means "for you" (mention) or "this will act" (primary, focus, link). Unread is weight. Red is destructive or do-not-disturb.
7. **Three text levels per row at most**, and `text_faint` never sits on `selected` or `overlay` (3.8:1); it steps up to `text_soft` there. *This is what stops grey-on-grey mush.*
8. **Titles truncate with one ellipsis; times, badges and icons never shrink.**
9. **Optical padding:** 10 on a button's icon side, 12 on its text side; a badge's numeral is centered on its cap height, not its line box.
10. **Nothing reflows.** Images, avatars and skeletons hold their final box before content lands.

## 5. GPUI notes
- **State fills are alpha tokens;** the presence ring needs the opaque result, so `components` takes a `ring: Rgba` argument (as `network_avatar` does now) fed by a `theme::over(fill, surface)` helper. On `sidebar`: hover #202123, selected #2E2F31.
- **Focus ring and elevation** are `BoxShadow` lists with zero blur and positive spread (GPUI has no outline). Use `.focus_visible()` if this GPUI revision has it, else track the last input kind.
- **No inset shadows.** The offline dot is a `text_faint` border; the mention bar is a 2 pt child div.
- **Presence shapes:** the idle bite is a child circle in the ring color at 60% size, offset −1, −1; the do-not-disturb bar is a 60% × 2 div in the ring color.
- **Avatars:** `img()` at 2× the drawn size with `.rounded()`, inside a relative div that also holds the 1 pt edge and the dot. The tint is `hsla()` from the existing name hash mod 8.
- **Baseline:** `.items_baseline()` on the header row, every child with `line_height(px(20.))`.
- **Tabular numerals:** `FontFeatures` `tnum` on SF Pro. Verify it applies; if not, give times a fixed-width box.
- **Icon stroke:** GPUI cannot restyle an SVG's stroke, so export `name-14.svg`, `-16`, `-20`.
- **Motion:** `with_animation` on opacity and a top offset, only on elements that have just mounted. Nothing holds an `Animation` with `.repeat()`. Tooltip delay and toast expiry are one-shot timers.
- **Scrollbar:** an absolutely positioned div read from the `ScrollHandle` / `ListState` offset; no built-in.
- **Rows stay fixed height** (28 and 48) so both lists remain `uniform_list`.

## 6. Removed

Gradient avatars, the rail selection bar, 64 pt rows, the third rail surface, and the other pieces this section used to list are already gone from the window.
