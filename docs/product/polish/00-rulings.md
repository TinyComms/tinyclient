# Polish round: rulings

Nine product files sit beside this one. Where they ask the owner a question or disagree, this
file answers. The owner asked that open choices be decided rather than sent back to him.

## Shared rules

1. **Inside a network's own place, its structure, order and words win. Where networks mix, the
   shared row wins** (`05`). Network homes follow `01`–`04`; Inbox, People, rail and switcher
   follow `05`.
2. **The right pane follows the conversation when it is about the conversation** (`07`): members
   and the DM partner's profile switch with it; a thread closes on leaving and comes back on
   return; search results stay. Two Details switches: one for channels and groups, one for DMs.
3. **A collapsed group never hides what needs me** (`01`): unread rows, the open row and voice
   channels with people stay visible.
4. **Bots and apps are never people.** They leave People, and in the Inbox they fold into one
   Apps row that raises no count and shows only while something in it is unread (`02`, `05`).
5. **One number on the rail**, on Inbox. Network and pinned buttons get a dot (`05`).
6. **Presence is honest**: unknown draws nothing; "last seen recently" is words, not a dot; a
   **Busy** state joins online, idle, do-not-disturb and offline on every network, for in a
   meeting, in a call or in a huddle (`03`, `04`).
7. **Profiles use one fixed section order** and show a section only when it has content (`06`).
   The info button shows info for whatever is open; server and workspace info opens from the
   Space name.
8. **Lists that come from the local cache say so**, and searching the network is a button, never
   automatic (`06`).
9. **The composer has attach, GIF, emoji and send**; stickers are a tab of the one picker; a
   control a network lacks is absent (`09`).

## Answers to the open questions

| From | Question | Answer |
|---|---|---|
| 01 | Friends' game and music activity | Shown. It is free and tells you who is around. |
| 01 | Folders start collapsed | Yes, safe under rule 3. |
| 01 | DMs before "Show more" | 10. |
| 01 | Close DM | In. |
| 02 | App DMs in the Inbox | Rule 4. |
| 02 | Hide channels quiet for 30 days | Yes. |
| 03 | Pin a Telegram folder to the rail | Yes, P2. |
| 03 | Channels in the Inbox | Only when starred. |
| 03 | Telegram pins versus Inbox pins | Separate. Telegram's order the Telegram list only. |
| 03 | Read every chat at sign-in | The newest 500, then more as the list scrolls. |
| 04 | Meeting chats after 14 quiet days | Into a collapsed Meetings group. |
| 04 | Busy presence | Rule 6. |
| 04 | Hidden chats come back on a new message | Yes, as Teams does. |
| 04 | Read Teams' Activity stream | Yes, once at connect, so mentions missed while closed are not lost. |
| 05 | Fetch previews | Slack, Teams and Telegram: yes, for rows on screen, paced. Discord: no fetch; a row fills in as messages arrive or are opened. A fetch per DM is the burst that flags accounts. |
| 05 | 30-day cut-off | Kept. |
| 05 | Drop People's everyone A–Z section | Yes. Full lists live in each network's home. |
| 05 | Own avatar | The first signed-in account's picture. |
| 06 | Mask phone numbers | Until clicked. |
| 06 | Discord activity line | Shown (as 01). |
| 06 | Leave a group from the pane | In, behind one confirm. Leaving servers stays cut. |
| 07 | One Details switch or two | Two (rule 2). |
| 07 | Pinned Space opens its last channel | Yes. |
| 09 | Scheduled send | Allowed where the network schedules it on its servers (Slack, Telegram). The user picks the time; tinyclient automates nothing. P2. |
| 09 | Second Teams token for OneDrive | Yes; Teams cannot send files without it. |
| 09 | Voice messages | P2. |
| 09 | GIFs on Slack and Teams | Hidden until the call is captured on a live session. |

## First in development

1. A thread composer that posts to the conversation the user left (`07`). Wrong-place sends.
2. The shell must start `chatd` itself, survive it going away, and say when it waits on a
   keychain prompt (`08`).
3. Crash protection: a panic hook in both processes (`08`).
4. Inbox previews and the rail's own picture (`05`, `08`).
