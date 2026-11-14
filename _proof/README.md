# Inbox row hover proof

## Bug

`list_row` applied `.hover()` on an anonymous inner face. GPUI only keeps
`hover_state` and calls `cx.notify` on enter/leave when the hovered element has
an id, so the fill updated only when something else happened to redraw — lag and
skipped rows while moving the pointer (R18 in `docs/product/polish/07-panes-selection.md`).

## Fix

Give the face `id("row-face")` and attach hover/active after that, matching the
phone press path and the switcher’s stateful hover rows.

## Automated proof

```text
cargo test -p tinyclient --features gui-shot list_row_hover_notifies
```

Fails before the fix (no notify on enter). Passes after: entering row A notifies,
moving to row B notifies again.

## Visual baselines

Demo `--shot` frames of the inbox after the fix (layout unchanged; hover is
pointer-driven and covered by the test above):

- `inbox.png` — All filter
- `inbox-unread.png` — Unread filter
- `switcher.png` — Jump switcher open

---

# Muted Telegram row’s bell-off (#257)

## Bug

The bell-off a muted Telegram row draws was chained on the row’s column, not on
line two: it laid out as a third line under the preview, outside the 16 pt line
the badge lives in, on a row fixed at 48 pt.

## Fix

`chat_row` assembles line two with the bell inside it, beside the count it
qualifies (`polish/03-telegram.md` §4): preview, then the bell, then the badge.

## Visual baseline

Demo `--shot --demo --mode telegram` frame of the real muted row (Nadia
Sokolova, muted, 3 unread):

- `telegram-mute-bell.png` — the bell-off sits on line two, immediately before
  the quiet outlined count.
