# Polish round: design rulings

Seven design files sit beside this one. `01-system.md` is the authority; where another file
departs from it, this list says which wins.

| Point | Ruling |
|---|---|
| Mention badge | A filled accent pill with the number alone. No "@" (01 over the navigation build). |
| Unread count | Outlined, never filled (04 over 01's grey fill). The filled pill means a mention and nothing else. |
| Inbox and People row height | Mixed heights with day headers: GPUI `list`, not `uniform_list` (04). The Discord home likewise (02). |
| Slack DMs | One-line 28 rows inside the user's sections, as Slack draws them (02 over 01). Discord DMs are 48. |
| Network marks | Monochrome in mixed lists, only with two or more networks signed in; none on pinned Spaces on the rail (04). New `*-mono.svg` assets. |
| Pane width | One width for every kind, 300, draggable 260–420 (07). |
| Composer focus | No accent border: caret plus a stronger neutral border (07 over 01). |
| Jumped-to message | Keeps its highlight until left or Escape; nothing fades (07). |
| Floating card | 300 wide, 64 avatar (05). |
| Email in profiles | Shown. Only phone numbers are masked until clicked (product rulings). |
| Busy | A solid `danger` disc, distinct from do-not-disturb's bar (03). |
| Red | Also for Teams' urgent mark and Telegram's Scam and Fake tags (03). |
| Draft label | Plain text weight, not accent (03): accent stays "for you" and "will act". |
| GIF playback | Only the hovered or keyboard-selected GIF plays, one at a time, never under Reduce Motion (06). |
| Upload cancel | Escape cancels only on a focused pending row (06): Escape never clears or discards typed work. |
| Option-Return | Silent send on Discord and Telegram; a new line on Slack and Teams (06). |
