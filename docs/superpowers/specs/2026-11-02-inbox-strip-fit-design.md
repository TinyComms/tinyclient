# Inbox title strip fit

**Intent:** The Inbox strip (title · All/Unread/Mentions · mark-all) must never truncate “Inbox” or clip the filter labels when the sidebar is dragged to its minimum (260).

**Goals:** Fixed short title never ellipsizes; filters stay usable; mark-all remains reachable when there is unread; same arithmetic-on-width style as the rest of the shell.

**Non-goals:** Changing `SIDEBAR_RANGE`; redesigning the segment control; applying the stack ladder to Discord/Slack (those strips are shorter).

**Anti-goals:** Letting `pane_title`’s `flex_1` + truncate steal space from a fixed label; measuring text every frame.

## Approaches

1. **Squeeze ladder by width (chosen):** fixed title → drop counts → hide mark-all → stack segments under the title.
2. Always stack below a single width — wastes vertical space when one line fits.
3. Raise sidebar minimum — fights narrow-list preference.

## Behavior

| Sidebar width | Layout |
|---|---|
| ≥ 360 | One line: Inbox + segments with counts + mark-all (when unread) |
| 300–359 | One line: Inbox + segments, no counts; mark-all when unread |
| 280–299 | One line: Inbox + segments, no counts; no mark-all |
| < 280 | Two rows: Inbox (+ mark-all when unread) on top; segments below; counts when width ≥ 270 |

Phone: keep today’s rules — no mark-all in the strip; stack when text scale > 1.25; one-line counts still at ≥310.

**Title:** “Inbox” is `flex_none` (not `pane_title`). “People” and long names keep `pane_title`.

**Builder call:** Thresholds live in a pure `inbox_strip_fit(width, phone, text_scale, has_unread)` tested with unit asserts. Discord/Slack keep showing segment counts at ≥ 300.

## Left to the builder

Exact constant names, wiring `segments` to take an explicit `show_counts` flag, and a one-line update in `docs/design/polish/04-unified.md`.
