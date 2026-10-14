# 07 · Panes, selection, focus and memory over time

Scope: how the window behaves as the user moves between conversations. What the profile pane
contains belongs to the info area; lists belong to the per-network and Inbox areas. Evidence:
**[read]** from the code, **[seen]** in an offscreen render, **[U]** from memory of the official
clients and not re-checked. "Official clients" means the Discord, Slack, Telegram and Teams apps.

## The principle

The right pane holds a **kind** (Details, Thread, Search) that the user picked, and a **subject**.
- **Details** is sticky. Its subject is always derived from the open conversation: the member list
  for a channel or group, the other person for a DM. It is remembered as **two switches**, one for
  channels and groups and one for DMs. Discord keeps the member list and the DM profile panel as
  separate toggles [U], and a profile beside every DM would cost 300 pt of reading width that is
  rarely used. Telegram Desktop's info column follows the chat you open [U]; we do the same
  within each shape.
- **Thread** belongs to one conversation. It never sits next to a different one: a reply typed
  there would go somewhere the screen does not show. On a switch it is put away for that
  conversation and comes back when you return. Discord closes its thread view on a channel
  switch [U]. Slack keeps its thread pane open [U], and that is the confusion we avoid.
- **Search** is how you move around, so it never follows. Picking a result changes the
  conversation, and the results must stay put.
- **Pushed layer:** a person opened from inside the pane (a member row, a name in a thread's
  header) is drawn on top of the base with `‹ Members` / `‹ Thread`, so it is at most 2 deep. On a
  switch the pushed layer is dropped and the base follows its own rule.

## 1. Right pane across a change of conversation

| Open now ↓ / switch to → | Another DM | Group DM / channel | Conversation on another network | Nothing (closed, deleted, removed) |
|---|---|---|---|---|
| Details: DM partner's profile | **Follows:** the new partner | DM switch stays on; the channel switch decides (members if on, else closed) | Same as the left two columns: the kind is global, not per network | Closes; switches unchanged |
| Pushed profile of someone (over members or a thread) | Pushed layer dropped; the base follows | Same | Same | Closes |
| Details: member list | Channel switch stays on; the DM switch decides (profile if on, else closed) | **Follows:** the new channel's members, from cache, list scrolled to top | Follows | Closes |
| Details: channel info (when it exists) | As the member list row | Follows: the new channel's info | Follows | Closes |
| Thread | Put away for the old conversation; the pane shows what the new one's switch says (Details or nothing) | Same; if the new conversation has a remembered thread, that thread returns | Same | Closes; the remembered thread is forgotten |
| Search results | **Stays**: the results and the selected hit stay as they were | Stays | Stays | Stays |

**Remembered:** the two Details switches and the one pane width belong to the window and survive
a restart. One remembered thread per conversation (the root id), kept in memory for the 50 most
recent conversations, lost on quit. Search keeps its query and scope until the pane closes.
**Restart restores:** the sidebar mode, the open conversation (at its scroll rule, section 5), the
Details switches and the widths. Telegram, Slack and Discord all reopen the last chat [U]. It does
not restore threads or search.

## 2. Rules, ranked by how much they change the feel

| # | P | Rule | Why |
|---|---|---|---|
| R1 | P0 | Clicking a conversation row draws the new title strip and the cached messages in the next frame. Rows open on **mouse-down**, as Finder and Mail select do. Never block a click for longer than one frame; data from the network merges in later without shifting what is on screen. | Mouse-down saves about 80–120 ms per switch, and the switch is the most frequent action in the app. |
| R2 | P0 | Apply the section 1 table on every switch. A thread never stays next to another conversation. | Today a thread from #a stays open beside #b, and its composer posts to #a [read]. |
| R3 | P0 | A conversation opens **at the first unread** when it has unread messages that do not fit on screen: the "New" divider sits about a third of the way down. Otherwise it opens at the bottom. Exceptions: opened from a search hit, a mention or a jump link, it is centred on that message; reopened in this session after the user left it scrolled up with nothing new since, it returns to where they were. | Slack lands on unread and Discord restores the position [U]; landing at the bottom loses what was missed (N10). |
| R4 | P0 | After an open the composer has focus, so typing just works. Exceptions: a conversation with no composer (read-only, Join bar) focuses the messages; walking the sidebar list with plain arrows keeps focus in the list. | Slack and Discord do this. Losing the first keystroke feels broken. |
| R5 | P0 | Nothing the user did not cause moves focus or the scroll position: an incoming message, a pane loading, a banner, a toast, a reorder of the list. | Stolen keystrokes and a list that jumps under the pointer are the commonest "cheap app" tell. |
| R6 | P0 | Details uses **one width** for members and for profile (default 300, drag 260–420, remembered). Opening or following the pane never reflows the conversation beyond that pane's own width. | Today members is fixed at 240 and the others at 340 [read], so moving from a channel to a DM with both open would jump the text. |
| R7 | P1 | Back and forward (`cmd-[`/`]`, 50 deep) record **conversation changes**, with each one's sidebar mode, scroll anchor and pane state, and restore them. A sidebar mode change on its own is not an entry. | A Back that only swaps the sidebar looks like nothing happened. Slack's history steps through conversations [U]. |
| R8 | P1 | The selected fill shows on the open conversation's row, wherever that row appears. The keyboard cursor is a separate ring. If the open conversation is not in the list on show, no row is selected and the list does not scroll. The sidebar scrolls to reveal the row only when it was opened **from that list by keyboard**, or through "Reveal in sidebar" (click the subtitle). | It agrees with the ruling that the sidebar does not follow, and the list never jumps on its own. |
| R9 | P1 | Plain up and down in a focused list move the cursor and open nothing; Enter opens and gives focus to the composer. `alt-up`/`down` open at once, from anywhere. | Opening on every arrow marks things read and subscribes to a Discord guild per row (account risk). The alt keys are the fast path, as in Slack. |
| R10 | P1 | Drafts, reply mode and the attachment tray are kept **per conversation**. The text survives a quit through chatd (as today). Reply mode survives switching away, as part of the draft, the way Telegram keeps a reply in a draft [U]. An unsaved edit survives switching away in this session. | Losing a half-typed reply on a glance elsewhere is data loss. |
| R11 | P1 | Escape follows the ladder below and never clears text. The sticky Details pane does **not** close on Escape: the header button or `cmd-.` closes it. | Otherwise, with members on, Escape could never reach "mark read, jump to newest". |
| R12 | P1 | Closing a layer (pane, menu, switcher, viewer, card) returns focus to whatever had it before, else to the composer. | Focus left on nothing makes the next keystroke vanish. |
| R13 | P1 | Narrow window: see section 4. Details never replaces the conversation. | Otherwise every switch would show a member list instead of the messages. |
| R14 | P1 | In the open conversation: at the bottom, new messages scroll into view. Scrolled up, or the window in the background, they do not; the pill counts them, and in the background the "New" divider is placed before the first one so it is there when you return. | This is what Slack does [U]; read marking already waits for a key window. |
| R15 | P1 | Live changes in the pane: the member list updates rows in place and keeps its scroll anchor. A profile shown updates in place. A thread that gets a reply appends it (at the bottom it follows, otherwise a pill). If I am removed or the conversation is deleted, the conversation region shows "You're no longer in #x" (or "This conversation was deleted") with its cached messages greyed out, the composer is replaced by that line, and the pane closes. Never jump elsewhere by yourself. | Silently swapping the screen loses the user's place. |
| R16 | P1 | The Inbox order is held while the pointer or the keyboard is in it (as built [read]) and is applied 1 s after the pointer leaves. | It stops misclicks on a row that moved. |
| R17 | P2 | Clicking a pinned Space on the rail shows its tree; the tree remembers its scroll and collapsed Groups per Space for the session. Clicking the lit rail button scrolls to the top (as built [read]). | Coming back to the same place keeps your bearings. |
| R18 | P2 | Hover fill appears on pointer enter and pressed fill on mouse-down, with no delay and no fade (ruling: nothing animates). | Instant feedback is what makes it feel native. |
| R19 | P2 | One window in v1: `cmd-N` is a new message (the switcher with `@`), not a window. Double-click opens nothing more. | A second window doubles every piece of state above, for a feature nobody asked for. |

## 3. Pane stacking, closing, width

- One slot, at most 2 deep. Clicking a member pushes a profile with `‹ Members`. Opening a thread
  while Details is open pushes the thread with `‹ Members`. Clicking an avatar inside a thread opens
  the **floating card** (as built [read]), so the thread is not given up.
- Closing: the pane's ✕ or `cmd-.` closes everything in it. The lit header button toggles
  Details. Escape pops the pushed layer, then closes a Thread or Search base, never Details.
- The pane's own back chevron is not part of window history (R7 records the pane state, not
  each push inside it).

## 4. Narrow windows (order regions give way)

1. The conversation shrinks to 392. 2. **Details hides** on its own: its switch stays on and it
returns when there is room again; its header button still opens it, as a one-off that replaces the
conversation with `‹ #channel`. 3. Thread, search and pushed profiles replace the conversation with
`‹ #channel` (as built [read]). 4. The sidebar leaves the row and floats on demand (as built). 5. In
replace mode, opening a conversation shows the conversation; the thread is put away (R2).

## 5. Per-conversation memory

| What | Where kept | Survives a switch | Survives closing the window | Survives quit |
|---|---|---|---|---|
| Scroll anchor (message id + offset), "left at bottom" bit | shell, last 50 | yes (R3) | yes (chatd keeps running) | no: reopens by R3 |
| "New" divider | shell, set at open from `last_read` | removed on leave; recomputed on return | n/a | recomputed |
| Draft text + tokens | chatd `SetDraft` + shell map | yes | yes | yes |
| Reply target | with the draft | yes (R10) | yes | P2 |
| Unsaved edit | shell | yes, in this session | yes | no |
| Composer height | follows its text | n/a | n/a | n/a |
| Remembered thread | shell, root id | yes, restored on return | yes | no |
| Details switches, pane width | prefs | n/a (per window) | yes | yes |
| Sidebar scroll per mode/Space | shell | yes | yes | no |
| Last open conversation + sidebar mode | prefs | n/a | yes | yes |

## 6. Escape ladder (first that applies, one step per press)

1 autocomplete list / IME candidate → 2 menu, popover, profile card → 3 image viewer →
4 switcher → 5 floating sidebar → 6 edit mode (restores the original) → 7 reply mode →
8 pushed pane layer → 9 Thread or Search pane (the search field leaves focus, keeping its query) →
10 nothing open: mark read, jump to newest, focus the composer. It never clears typed text and
never closes Details.

## 7. Stories

| # | P | Story | Observable check |
|---|---|---|---|
| S1 | P0 | I switch from one DM to another with the profile open and see the new person. | DM A + profile on, open DM B: pane subject == B's peer in the same frame; width unchanged. |
| S2 | P0 | I open a thread in #a, then click #b: no #a thread beside #b; back in #a it is there. | Thread kind gone after the switch; return to #a: thread with the same root reopens. |
| S3 | P0 | I click a row and it opens before I let go. | Mouse-down to new title drawn ≤ 1 frame with the cache warm; no blank message region. |
| S4 | P0 | I open a chat with 40 unread and land on the first one. | The divider row is in the top third of the viewport after open. |
| S5 | P0 | I type right after opening anything. | Open from row / cmd-K / Back, type "hi": the composer holds "hi". |
| S6 | P1 | With members on in channels, I open a DM and get no profile unless I turned it on for DMs. | The channel switch stays on; the pane is closed in the DM; back in a channel the members return. |
| S7 | P1 | I go Back and get my scroll position and pane. | Scroll up in #a, open #b, `cmd-[`: the same first-visible id, and the pane kind as it was. |
| S8 | P1 | I relaunch and land where I was. | Quit in #a with members on: on launch #a is open, members on, same mode. |
| S9 | P1 | A half-written reply survives a detour. | Reply to m1 in #a, type, open #b, return: the reply bar and the text are intact. |
| S10 | P1 | Escape never eats my text or my member list. | Repeated Escape: the composer text is unchanged and Details stays open. |

## 8. Current behavior where it differs

- A thread stays open beside the next conversation; its composer still targets the old one
  [read: `conversation.rs` drops the thread only when the pane kind changes].
- DM profile → another DM: the pane keeps the **old** person [read: `toggle_info` and `show_info`
  run only on a click]. The member list does follow a switch, and into a DM it shows that DM's
  members [read: `members.rs` `see`].
- The member list is fixed at 240 and other kinds at 340: the conversation reflows when the kind changes [read].
- Opening lands at the bottom; the divider is drawn but not shown [read: `Purpose::Open` keeps the last message].
- No scroll memory: every switch restarts at the bottom [read: `Pane::start` resets the list].
- History entries carry the mode and the conversation only, without scroll or pane; mode-only changes are entries [read: `sidebar.rs` `go`/`here`].
- Reply mode and an edit are dropped on a switch [read: `composer.rs` `aim` → `leave`].
- Escape closes Details like any pane [read: `close_top`].
- A restart restores only widths and pins: it opens on the Inbox with "Pick a conversation" [read: `save_prefs`; seen].
- Rows open on click (mouse-up) [read].

## 9. Open questions for the owner

1. **Details: one switch or two?** Recommended: two (channels / DMs), as Discord does. With one
   (Telegram's way), a member list left on means a profile beside every DM.
2. **Clicking a pinned Space opens its last channel** (Discord's and Slack's habit) instead of
   leaving the open conversation alone (ruling 6)? Recommended: yes, for pinned Spaces only.
