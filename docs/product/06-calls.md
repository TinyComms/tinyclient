# 06 — Calls

What tinyclient carries in a call, and what it still hands to the official app. Code on main is the source. A call button shows an in-call state only after the network confirms the join. A network or call type this file does not list as native is a hand-off: the control names the official app and does not start mediad.

## Sound

Discord calls and Tiny room calls use the same microphone processing. Echo cancellation, noise suppression, and automatic gain are on unless Settings › Voice turns one off. Noise suppression defaults to Enhanced, which turns down other voices and cries as well as fans. Standard and High leave other voices in. The canceller hears the mix that is playing, cues included. The Tiny channel bar says to wear headphones only while echo cancellation is off.

Discord voice is Opus at 48 kHz stereo: voice gateway v8, UDP, and DAVE. DAVE seals Opus, and on a picture it seals the H.264 access unit. Version 1 is required on every Discord call this client joins. A stage is not joined; it opens in Discord. A voice channel joins from the server list: the hover Join pill, a double-click, or Join voice on the row menu. A single click opens the channel and does not join. The Discord home lists friends who are in a voice channel or a DM call under In voice, and that row joins the same way. A DM can be called and can ring. A camera rides that same voice connection. A screen is a separate Go Live connection, described under Pictures on a Discord call.

A Tiny room joins with sound when the server's audio is up: one send-only Opus microphone (mono), one receive slot per other seat, UDP preferred, and ICE-TCP where UDP is blocked. Mute, deafen, speaking, and join and leave cues are part of that call. If the server has no audio, joining only takes a seat.

## Pictures, on a Tiny room

A Tiny call can send a camera and a screen, and it receives one H.264 picture. On a desktop, what it sends is encoded first by the machine's own hardware: on Windows, Media Foundation lists the hardware H.264 encoders (NVIDIA, Intel, AMD), and the first one that takes the job runs it at the size, frame rate and bitrate the share chose, at constrained baseline, with no download and hardly any CPU. A machine that lists no such encoder, or one that refuses its setup, falls back to OpenH264 constrained baseline, downloaded from Cisco the first time a picture is encoded, and tried three times with a growing pause before it gives up. A failed download is said in the call dock with the reason (a VPN that blocks Cisco's host is the usual one) and how to try again: turn the share or the camera on once more. Until then the call stays voice. The downloaded file is kept across starts (in `%LOCALAPPDATA%\tinyclient` on Windows, `~/.cache/tinyclient` elsewhere), so the network is asked once and later calls need none. A file on disk that turns out not to be Cisco's is taken away and fetched again. macOS and Linux send through OpenH264 as they always have; whatever arrives is decoded through OpenH264 on every desktop.

The room forwards one picture: a share while someone else is sharing, otherwise one layer of another person's camera. The listener does not get their own picture. The client offers three video lines, the first two inactive until asked: a camera as two layers of one picture (640×360 and 320×180), one share fitted inside 1280×720, and one receive line for the picture the room forwards. Both the camera and the share run at 15 frames a second. Nothing plays until someone chooses Watch.

The camera is the system's camera: Media Foundation on Windows, AVFoundation on macOS, and V4L2 on Linux. A Linux camera that V4L2 cannot open is asked for through the desktop portal, the same request on GNOME, KDE, Sway, Hyprland, and the other desktops that ship one. Android and iOS use the phone's own camera. A machine with no camera says so and the call stays voice.

Screen share on Windows can be a screen or a window. On macOS it is the primary screen. On an X11 Linux session (XFCE, MATE, Cinnamon, i3, and the rest) it is the primary screen. A Wayland session asks the desktop portal, so the system's picker chooses a monitor or a window on every compositor that implements that portal. PipeWire is loaded when that share starts. A machine without it still launches, and says so if a Wayland share is asked for. Cancelling the picker does not fall through to a black picture. Android shares through MediaProjection. An iPhone shares the whole screen through a broadcast extension. The simulator, which has no system broadcast, shares this app's screen.

When the server offers `e2ee_calls`, the same MLS group seals Opus and the H.264 picture. Silence is not sealed. The picture's one-byte NAL header stays clear so the server can still see a keyframe. It cannot open the picture or the voice. Until the group has keys, the microphone and the picture are held back rather than sent in the clear. A server that does not offer `e2ee_calls` carries the call as transport encryption only. The lock and the call code in the call header are this Tiny protection. Discord DAVE does not use that lock.

## Pictures, on a Discord call

A camera is one H.264 layer on the voice connection: rid `100`, 640×360, 15 frames a second. Go Live is a second voice connection. It sends the screen, fitted inside 1280×720, and it does not open a microphone. Watching someone else's share is a third connection that only receives. The voice connection stays up the whole time. The stream's MLS group is the stream server id minus one, not the channel id.

DAVE seals the picture as well as the voice. The transport encryption is the same AES-GCM or XChaCha the voice already uses. The client offers H.264 and does not offer another video codec. There is no simulcast, no AV1, VP8 or VP9, no system audio on the share, and no clips. A stage is still not joined.

## Hand-off

| Network | What the control does |
| --- | --- |
| Slack | Huddles open in Slack. |
| Teams | Calls and meetings open in Teams. |
| Telegram | Calls open in Telegram. |
| Android Messages | Opens in Messages. |
| Signal | No call control. |
| Discord stage | Opens in Discord. |

Gallery, recording, captions, a soundboard, stage channels, activities, a lobby, and breakout rooms are not built on any network.

## Still open

A native Slack huddle, a native Telegram call, and a native Teams call are not in this tree. Each official path is large, undocumented or unofficial, and none of it is started here. Whether a Slack huddle is worth a spike is still a product choice.

Discord voice from a user account on an unofficial client can lose the account. DAVE makes the client distinguishable, and a picture makes it more so.
