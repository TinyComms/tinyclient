# Call end-to-end proof

Branch `feat/call-e2e`, cut from `origin/main` (`f97935e`). Work only in this worktree: `C:\Users\gollum\Development\Personal\tiny\_wt-call-e2e`. Do not touch the dirty checkout at `tinyclient` or `tinyserver`.

## Intent

A TypeScript application drives the shipping tinyclient binary against a real tinyserver on Windows, Linux, and macOS. A run passes only when two live windows are in one room, speech and two distinguishable pictures went across the SFU, the remote window shows them, and each machine stored an mp4 of that window.

`cargo test` stays the unit suite. This application is a separate program.

## What main already does

Tiny calls already offer camera and share, encode with openh264, and decode a received Annex B frame to a JPEG the shell paints (`crates/mediad/src/tiny.rs`, `crates/mediad/src/picture.rs`). `NO_VIDEO` applies when a voice engine is up and the call is not Tiny. Camera capture is Windows-only (`nokhwa`). Screen capture reads the real display. `TINYCLIENT_MEDIAD_AUDIO` already replaces the microphone with `null`, `tone`, or `onset`, and opens no device. `TINYCLIENT_MEDIAD_SINK` writes played audio as raw 48 kHz stereo `i16`.

Call buttons have element ids (`tiny-join`, `tiny-bar-camera`, `tiny-bar-share`, `tiny-leave`) and no GPUI role. Settings rows already set `.role()`. AccessKit exposes a node only when it has both an id and a role.

## Fixture devices

`TINYCLIENT_CALL_FIXTURE` is a directory. When it is set, mediad uses it and opens no microphone, camera, or screen.

The directory contains:

- `speech.pcm` — 48 kHz stereo `i16`, little-endian, a spoken sentence. The microphone loops it.
- `camera-h.h264` and `camera-l.h264` — Annex B, constrained baseline. Copy the colour-bar files from `C:\Users\gollum\Development\Personal\tiny\tinyserver\crates\sfu\fixtures\` (those files are MIT, synthetic SMPTE bars).
- `share.h264` — Annex B of a picture that is not colour bars. A solid four-quadrant frame (red, green, blue, white), encoded with the openh264 encoder this crate already uses, one IDR, is enough. Commit the bytes. A unit test decodes both the camera high clip and the share clip and asserts their first-frame average colours differ.

`listing()` includes an input whose id and name are `fixture` when the variable is set, and that input is the default. `Audio::capture` reads `speech.pcm` the same way it reads the tone: one 20 ms frame per call, looping, no device. `probe_camera` and `probe_screen` succeed. `Pictures::camera` returns the next frames of the two camera clips, split on Annex B start codes, looping, high then low. `Pictures::share` returns the next frame of `share.h264`. The existing `write_video` path sends those bytes. The existing decode path paints them.

Unset, behaviour stays the hardware path. `TINYCLIENT_MEDIAD_AUDIO` still wins for audio when both are set, so current tests keep their tone.

## Call controls

These controls get `.role(gpui::Role::Button)` and a name a driver can read, using the label the person sees:

- Join (`tiny-join` and `tiny-join-empty`): "Join channel"
- Mute, deafen, share, camera, devices, health, leave: the tooltip words already chosen in `tiny_call_bar`

`button` and `button_with_icon` set `Role::Button` and the accessible name from the label. `bar_button` takes the name and sets the role. Disabled buttons stay unnamed-free of a click action.

## Catalog

`npm run all` walks every demo surface, then the call. `npm run doctor` names what this machine still lacks. The demo scenarios are `inbox`, `people`, `discord`, `slack`, `telegram`, `teams`, `signal`, and `settings`. Each one starts `chatd --seed-demo` and the shell, presses the accessible names in `e2e/src/catalog.ts`, and stores an mp4. The rail, each conversation row, each menu item, and Settings publish those names. A network button's name is the network label (`Discord`, `Slack`), so a second workspace does not rename it.

## The TypeScript application

Replace nothing that is not in this worktree. `desktop-e2e` is not on this branch. Add `e2e/` as a strict TypeScript package (`"type": "module"`, `strict` true). No CommonJS.

Layout:

- `e2e/src/cli.ts` — `node --experimental-strip-types` or `tsx` is not required if the package compiles with `tsc`. Script `npm test` runs the unit tests of the orchestrator. Script `npm run call` runs the call scenario.
- `e2e/src/scenario/call.ts` — one scenario, no OS names inside it. Steps: start the server, launch two clients with distinct `TINYCLIENT_DIR` values and the same `TINYCLIENT_CALL_FIXTURE` directory (`crates/mediad/tests/fixtures/call`). Sign-in uses the scratch-directory path `session.rs` already has: when `TINYCLIENT_DIR` is set, chatd reads `TINYCLIENT_SESSION_TINY`. The operator supplies `TINY_E2E_SESSION_A` and `TINY_E2E_SESSION_BEA` (the same JSON a saved Tiny session already is). The scenario puts each value into that process's `TINYCLIENT_SESSION_TINY`. It does not add a new sign-in mechanism. Each process also sets `TINYCLIENT_MEDIAD_SINK` to its own pcm path. Then join the same room, select the fixture microphone if it is not already the default, turn the camera on, turn share on, wait until the remote side is receiving the share, record the remote window, stop.
- `e2e/src/host.ts` — `Host` with `platform: "windows" | "linux" | "macos"`, `record(title, pcmPath, mp4Path)`, `click(name)`, `find(name)`.
- `e2e/src/host/windows.ts`, `linux.ts`, `macos.ts` — the only files that name ffmpeg devices or Appium automation names. Windows: WinCore (`automationName` `Wincore`) and ffmpeg `gdigrab` by window title, muxing the PCM sink. macOS: Mac2 (`mac2`) and `avfoundation` cropped to the window bounds the driver reports. Linux: `atspi2` (`@itsmeaj/appium-linux-driver`) and `x11grab` of the window rectangle. The CLI selects the host from `process.platform` and refuses a host module that is missing. A missing binary (ffmpeg, appium) fails the run with the command it needed. It does not skip.
- `e2e/src/oracle.ts` — pure functions: PCM RMS above a floor, and two RGB averages far enough apart to tell bars from quadrants. Unit tests cover them with synthetic buffers. The scenario calls them on the sink and on a PNG ffmpeg grabbed from the mp4.
- `e2e/src/server.ts` — spawn `TINYSERVER_BIN` with the env the tinyserver README names for a local call (`TINYSERVER_PUBLIC_IP`, `TINYSERVER_MEDIA_PORT`, database URLs). The scenario does not embed a server.

Appium is a dependency (`webdriverio`). The click path uses the accessibility name. Tests of the scenario use a fake `Host` and assert the step order and that the scenario passes the fixture directory and two data directories. They do not launch the GUI.

## Desktops

`npm run fleet` runs one command on each desktop. The Windows desktop is `ssh winbuild`, the Windows VM on Atomic Studio shared by Gunmetal and Tiny (console user `builder`, QXL, logged on). OpenSSH on that guest is session 0, so the Windows leg extracts the checkout over SSH and `e2e/scripts/desk.ps1` runs the command as an interactive scheduled task in the console session, where WinCore and `gdigrab` see the window. The Mac leg is one `ssh mbp`. The Linux leg is one `ssh tiny-linux`, the Ubuntu desktop VM on Atomic Studio (`tiny-linux`, bridged to the LAN, GNOME on Xorg, autologin). `atomicstudio` is the Unraid host that runs those guests, not a desktop itself. Each remote leg streams this checkout in. `e2e/scripts/desk.sh` installs the Mac or Linux Appium driver, and `desk.ps1` installs WinCore. Appium stays on `127.0.0.1` of that desktop. The artifact tar comes back on that same ssh stdout to `e2e/artifacts/<platform>/`.

## Pass rule

A desktop run is green only when both clients joined, the remote PCM RMS shows the speech clip, the recorded frame is the share picture and not the camera picture, and `mp4` exists and is longer than one second. The same scenario file is what Windows, Linux, and macOS run.

## Out of scope

Mobile. A cloud device farm. A paid voice API. Replacing `cargo test`. Driving Discord, Slack, Telegram, or Teams calls.
