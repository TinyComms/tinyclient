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
