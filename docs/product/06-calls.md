# 06 — Calls

What tinyclient carries in a call, and what it still hands to the official app. Code on main is the source. A call button shows an in-call state only after the network confirms the join. A network or call type this file does not list as native is a hand-off: the control names the official app and does not start mediad.

## Sound

Discord calls and Tiny room calls use the same microphone processing. Echo cancellation, noise suppression, and automatic gain are on unless Settings › Voice turns one off. Noise suppression defaults to Enhanced, which turns down other voices and cries as well as fans. Standard and High leave other voices in. The canceller hears the mix that is playing, cues included. The Tiny channel bar says to wear headphones only while echo cancellation is off.

Discord voice is Opus at 48 kHz stereo: voice gateway v8, UDP, and DAVE. DAVE seals Opus, and on a picture it seals the H.264 access unit. Version 1 is required on every Discord call this client joins. A stage is not joined; it opens in Discord. A voice channel joins from the server list: the hover Join pill, a double-click, or Join voice on the row menu. A single click opens the channel and does not join. The Discord home lists friends who are in a voice channel or a DM call under In voice, and that row joins the same way. A DM can be called and can ring. A camera rides that same voice connection. A screen is a separate Go Live connection, described under Pictures on a Discord call.

A Tiny room joins with sound when the server's audio is up: one send-only Opus microphone (mono), one receive slot per other seat, UDP preferred, and ICE-TCP where UDP is blocked. Mute, deafen, speaking, and join and leave cues are part of that call. If the server has no audio, joining only takes a seat.

## Direct calls on a Tiny server

When a Tiny server advertises `direct_calls`, members can place 1:1 audio and video calls from a direct message conversation header or from a person's menu (bots cannot be called).

- **Ringing:** outgoing calls ring until answered, declined, cancelled, or timed out (60 s); incoming calls show in the call dock and as a system notification with Accept and Decline. Accepting starts `mediad` and joins the 1:1 media session; declining or cancelling ends it and records a call row in the conversation.
- **Media and auto-watch:** direct calls carry the same microphone processing, camera, screen share, and optional MLS `e2ee_calls` sealing as Tiny rooms. As soon as the peer turns on their camera or screen share, tinyclient automatically begins decoding and displaying it above the conversation (with a corner self-view PiP when you send your own camera or screen, and a compact preview tile above the dock if you navigate to another chat). Choosing **Stop watching** stops decoding until the peer stops and starts again.
- **Ending:** either side leaving ends the call for them; when the peer hangs up, the dock briefly shows "Call ended" before clearing.

## Pictures, on a Tiny room or direct call

A Tiny call can send a camera and a screen, and it receives one H.264 picture. On a desktop, what it sends is encoded first by the machine's own hardware: on Windows, Media Foundation lists the hardware H.264 encoders (NVIDIA, Intel, AMD), and the first one that takes the job runs it at the size, frame rate and bitrate the share chose, at constrained baseline, with no download and hardly any CPU. A machine that lists no such encoder, or one that refuses its setup, falls back to OpenH264 constrained baseline, downloaded from Cisco the first time a picture is encoded, and tried three times with a growing pause before it gives up. A failed download is said in the call dock with the reason (a VPN that blocks Cisco's host is the usual one) and how to try again: turn the share or the camera on once more. Until then the call stays voice. The downloaded file is kept across starts (in `%LOCALAPPDATA%\tinyclient` on Windows, `~/.cache/tinyclient` elsewhere), so the network is asked once and later calls need none. A file on disk that turns out not to be Cisco's is taken away and fetched again. macOS and Linux send through OpenH264 as they always have; whatever arrives is decoded through OpenH264 on every desktop.

The room or direct call forwards one picture: a share while someone else is sharing, otherwise one layer of another person's camera. The listener does not get their own picture back from the server; local self-view renders directly from the outgoing capture. The client offers three video lines, the first two inactive until asked: a camera with adaptive layers, one screen share at the quality chosen in the share picker (`ShareQuality`: **Best** up to 2560×1440 at up to 60 fps, **Balanced** 1080p at 30 fps, or **Low** 720p at 15 fps), and one receive line for the picture forwarded by the server. In a Tiny room, nothing decodes until someone chooses Watch; in a 1:1 Tiny direct call, the peer's camera or screen share is auto-watched as soon as it starts.

The camera is the system's camera: Media Foundation on Windows, AVFoundation on macOS, and V4L2 on Linux. A Linux camera that V4L2 cannot open is asked for through the desktop portal, the same request on GNOME, KDE, Sway, Hyprland, and the other desktops that ship one. Android and iOS use the phone's own camera. A machine with no camera says so and the call stays voice.

Screen share on Windows can be a screen or a window. On macOS it is the primary screen. On an X11 Linux session (XFCE, MATE, Cinnamon, i3, and the rest) it is the primary screen. A Wayland session asks the desktop portal, so the system's picker chooses a monitor or a window on every compositor that implements that portal. PipeWire is loaded when that share starts. A machine without it still launches, and says so if a Wayland share is asked for. Cancelling the picker does not fall through to a black picture. Android shares through MediaProjection. An iPhone shares the whole screen through a broadcast extension. The simulator, which has no system broadcast, shares this app's screen.

When the server offers `e2ee_calls`, the same MLS group seals Opus and the H.264 picture. Silence is not sealed. The picture's one-byte NAL header stays clear so the server can still see a keyframe. It cannot open the picture or the voice. Until the group has keys, the microphone and the picture are held back rather than sent in the clear. A server that does not offer `e2ee_calls` carries the call as transport encryption only. The lock and the call code in the call header are this Tiny protection. Discord DAVE does not use that lock.

## Pictures, on a Discord call

A camera is one H.264 layer on the voice connection (rid `100`). Go Live is a second voice connection. It sends the screen at the quality that was picked: 720p at 15 frames a second, 1080p at 30, or 1440p at 60. It does not open a microphone. Watching someone else's share is a third connection that only receives. The voice connection stays up the whole time. The stream's MLS group is the stream server id minus one, not the channel id. On desktop, received Discord video (the active speaker's camera, debounced by 800 ms, or a watched Go Live share) renders in the call picture area above the conversation or in the compact dock preview, one decoded picture at a time.

DAVE seals the picture as well as the voice. The transport encryption is the same AES-GCM or XChaCha the voice already uses. The client offers H.264 and does not offer another video codec. There is no simulcast, no AV1, VP8 or VP9, no system audio on the share, and no clips. A stage is still not joined.

## Hand-off

Which call control is native, a hand-off, or missing is [the generated table](../parity.md). A control that table does not mark native names the official app and does not start mediad.

A multi-stream video gallery (more than one simultaneous remote video feed), recording, captions, a soundboard, stage channels, activities, a lobby, and breakout rooms are not built on any network.

## Still open

A native Slack huddle, a native Telegram call, and a native Teams call are not in this tree. Each official path is large, undocumented or unofficial, and none of it is started here. Whether a Slack huddle is worth a spike is still a product choice.

Discord voice from a user account on an unofficial client can lose the account. DAVE makes the client distinguishable, and a picture makes it more so.
