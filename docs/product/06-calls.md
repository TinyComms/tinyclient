# 06 — Calls

What tinyclient carries in a call, and what it still hands to the official app. Code on main is the source. A call button shows an in-call state only after the network confirms the join. A network or call type this file does not list as native is a hand-off: the control names the official app and does not start mediad.

## Sound

Discord calls and Tiny room calls use the same microphone processing. Echo cancellation, noise suppression, and automatic gain are on unless Settings › Voice turns one off. The canceller hears the mix that is playing, cues included. The Tiny channel bar says to wear headphones only while echo cancellation is off.

Discord voice is Opus at 48 kHz stereo: voice gateway v8, UDP, and DAVE. DAVE here seals and opens Opus only. Version 1 is required on every Discord call this client joins. A stage is not joined; it opens in Discord. A voice channel joins from the server list: the hover Join pill, a double-click, or Join voice on the row menu. A single click opens the channel and does not join. The Discord home lists friends who are in a voice channel or a DM call under In voice, and that row joins the same way. A DM can be called and can ring. Screen share, camera, and Go Live are not built. Asking for a picture on a joined Discord call is refused in words, and the call stays voice.

A Tiny room joins with sound when the server's audio is up: one send-only Opus microphone (mono), one receive slot per other seat, UDP preferred, and ICE-TCP where UDP is blocked. Mute, deafen, speaking, and join and leave cues are part of that call. If the server has no audio, joining only takes a seat.

## Pictures, on a Tiny room only

A Tiny call can send a camera and a screen, and it receives one H.264 picture. The codec is OpenH264 constrained baseline, downloaded from Cisco the first time a picture is encoded. If that download fails, the call stays voice.

The room forwards one picture: a share while someone else is sharing, otherwise one layer of another person's camera. The listener does not get their own picture. The client offers three video lines, the first two inactive until asked: a camera as two layers of one picture (640×360 and 320×180), one share fitted inside 1280×720, and one receive line for the picture the room forwards. Both the camera and the share run at 15 frames a second. Nothing plays until someone chooses Watch.

The camera is the first camera, and only on Windows. On macOS and Linux the control says a camera is not available and that the screen can still be shared.

Screen share is the whole primary screen, not a window and not a system picker. It captures on Windows, on macOS, and on a Linux session that has an X11 display. A session with no X11 display cannot share, which is a Wayland session that is not also exposing X11. There is no Wayland capture.

When the server offers `e2ee_calls`, the same MLS group seals Opus and the H.264 picture. Silence is not sealed. The picture's one-byte NAL header stays clear so the server can still see a keyframe. It cannot open the picture or the voice. Until the group has keys, the microphone and the picture are held back rather than sent in the clear. A server that does not offer `e2ee_calls` carries the call as transport encryption only. The lock and the call code in the call header are this Tiny protection. Discord DAVE does not use that lock.

## Hand-off

| Network | What the control does |
| --- | --- |
| Slack | Huddles open in Slack. |
| Teams | Calls and meetings open in Teams. |
| Telegram | Calls open in Telegram. |
| Android Messages | Opens in Messages. |
| Signal | No call control. |
| Discord stage | Opens in Discord. |
| Discord voice picture, or any Go Live | Not built. The voice call is not a video call. |

Gallery, recording, captions, a soundboard, stage channels, activities, a lobby, and breakout rooms are not built on any network.

## Still open

A native Slack huddle, a native Telegram call, and a native Teams call are not in this tree. Each official path is large, undocumented or unofficial, and none of it is started here. Whether a Slack huddle is worth a spike is still a product choice. Discord video would be a second path from the one Tiny pictures use, and it is not started: DAVE on Discord seals audio only.

Discord voice from a user account on an unofficial client can lose the account. DAVE makes the client distinguishable. That risk is unchanged by this file.
