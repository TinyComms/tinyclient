# Design

One visual system for Slack, Discord, Telegram, Teams, Signal, Messages, and a Tiny server you
already run. A network is identified by its real icon and nothing else changes. More than one
Tiny server can be signed in. A Tiny server has text channels and direct messages; its screen
is `10-tiny-server.md`.

The project is <https://github.com/TinyComms/tinyclient>. The website is <https://tinyclient.io>.

The detail is in `docs/design/`:

| File | Covers |
|---|---|
| `01-system.md` | tokens, type, spacing, radii, motion, every base component |
| `02-layout-navigation.md` | window regions, rail, sidebar modes, trees, quick switcher, search, keys |
| `03-conversation.md` | message rows, rich text, media, cards, reactions, replies, thread pane, viewer |
| `04-composer-actions.md` | composer, attachments, autocomplete, emoji, hover toolbar, context menu |
| `05-people-presence.md` | avatars, presence, People view, profile card, member list, own status |
| `06-live-signals.md` | unread, mention and mute rules, typing, connection states, notifications |
| `07-calls.md` | call dock, ringing, voice channels, Tiny picture, hand-off |
| `08-first-run-settings.md` | first launch and sign-in per network |
| `09-settings.md` | the Settings window (replaces 08 §5) |
| `10-tiny-server.md` | a Tiny server's column, channel, member list, and server settings |

`09` and `10` have no mockup yet. The earlier files have one in `docs/design/mockups/`.

## Rulings

These files disagree in places. Where they do, this section wins.

1. **`01-system.md` is the authority** for tokens, type sizes, radii, shadows and motion. Tokens the
   other files invent map onto its tokens; a file's own "New tokens" survive only where the system
   has no equivalent (`warning_soft`, `quiet_fill`, `positive_soft`, the opaque row fills).
2. **Accent is blue `#2F6FEB`.** It means "for you" or "this will act". Plain unread is weight plus
   a neutral count. The "New" divider and the mention-of-me tint are accent.
3. **Row heights.** Two-line rows (Inbox, People) are 48 with a 32 avatar. One-line rows (channel
   trees, members in a voice channel, menus) are 28. Message avatars are 36. `07` says 64,
   32 and 30; it is overruled.
4. **Presence owns the avatar's bottom-right corner**, coded by shape as in `05`. Unknown shows
   nothing. The network icon is never on an avatar: it is a 14 px icon leading the title, and only
   in a list that mixes networks.
5. **People is a rail destination** that puts the People list in the sidebar and leaves the open
   conversation alone. There is no Chats | People switch. Order is online first, then A to Z.
6. **Rail buttons are places.** Inbox, People, one per network, pinned Spaces, add, own avatar. No
   selection bar and no connection dot on rail icons; trouble shows as the inline banner of `06`.
7. **Typing** is the row of `06`: text plus three dots stepped by a 400 ms timer in their own
   entity, bounded to 30 s. Nothing else on an unchanged screen moves.
8. **Red** means: destructive action, do-not-disturb, failed send, expired session, missed call,
   and a muted or deafened glyph. It is never a fill behind a count.
9. **Composer** follows `01` for shape (radius 10, 28 px controls) and `04` for behavior, except
   that an upload does not lock it: the message with attachments becomes a pending row with its
   own progress, and the composer is free for the next one. Escape never clears typed text.
10. **Calls** use the dock of `07` at the bottom of the sidebar. A voice-only call takes no room in
    the conversation. Shortcuts yield to `02` where they collide.
11. **Quit.** On macOS, closing the window hides it and leaves `chatd` receiving while
    Settings › General › "Keep receiving when the window is closed" is on (the default).
    On Linux and Windows that setting is absent and closing quits. ⌘Q stops an
    application-managed `chatd`. A systemd user unit keeps the daemon across quit.
    There is no menu-bar status item.
12. **Dark only** until the light values in `01` have been checked on a screen.
13. **Settings** follow `09-settings.md`. `08` §5 is withdrawn.
14. **A Tiny server** follows `10-tiny-server.md` for its column and stage. Text channels use
    the ordinary conversation. Direct messages are People conversations.

## Rules

- Only rows on screen are drawn. Lists with one row height use `uniform_list`.
- Media boxes are sized from metadata before bytes arrive, so rows do not jump.
- No blur, no filters, no spinner, no shimmer. Menus, popovers, modals, toasts and the scrollbar
  fade over 80 to 160 ms; nothing else animates.
- Warnings about using an unofficial client are in the README, never in the client.

## Icons

Network icons are from Dashboard Icons and interface glyphs from Lucide; both are compiled in.
Sources and licenses are in `crates/tinyclient/assets/README.md`.
