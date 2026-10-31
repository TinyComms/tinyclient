# Task report: TypeScript call scenario

Status: DONE_WITH_CONCERNS

Implementation commit: `ae4fce1cb17435858b2b5a76c3bd0a11138ddb5e` on `feat/call-e2e` (not pushed).

## What landed

`e2e/` is a strict ESM TypeScript package. `npm test` compiles with `tsc` and runs the oracle, scenario, and CLI unit tests under `node:test`. `npm run call` runs `dist/cli.js`.

`e2e/src/scenario/call.ts` is the only scenario. It does not name an operating system or an ffmpeg device. It starts the server, launches two clients with distinct `TINYCLIENT_DIR` values and the same `TINYCLIENT_CALL_FIXTURE` (`crates/mediad/tests/fixtures/call`), copies `TINY_E2E_SESSION_A` and `TINY_E2E_SESSION_BEA` into each process's `TINYCLIENT_SESSION_TINY`, and sets `TINYCLIENT_MEDIAD_SINK` to that process's `played.pcm`. Empty or blank sessions throw before any process starts. It then waits for Join channel, joins both, selects the microphone named `fixture` when that name is not already present, turns camera and share on from the first client, waits until the second client can watch the share, records that window, and checks the pass rule.

The pass rule is in that file. The remote sink's PCM RMS must clear the speech floor (200). The PNG grabbed from the mp4 must be near the four-quadrant average `{127.5, 127.5, 127.5}` and far from the colour-bar average `{109.714, 109.714, 109.714}` (Euclidean distance greater than 20). The mp4 must exist and be longer than one second.

`e2e/src/server.ts` spawns `TINYSERVER_BIN`. It runs `migrate`, then `serve`, with `TINYSERVER_PUBLIC_IP` (`127.0.0.1`), `TINYSERVER_MEDIA_PORT` (`50000`), `DATABASE_URL`, and `DATABASE_URL_MIGRATE` from the tinyserver README's local example. An operator value overrides a default. The rest of the process environment is inherited. The scenario does not embed a server.

The CLI maps `win32`, `linux`, and `darwin` onto a host module and refuses any other platform, or a module that does not load. `ffmpeg` and `appium` are required in the host's `prepare`. A missing binary fails the run and names the command. It does not skip. Automation names and ffmpeg device names are only in `e2e/src/host/windows.ts` (`Wincore`, `gdigrab` by window title, muxing the PCM sink), `macos.ts` (`mac2`, `avfoundation` cropped to the driver's window bounds), and `linux.ts` (`atspi2`, `@itsmeaj/appium-linux-driver`, `x11grab` of the window rectangle). Clicks go through the accessibility name.

## Tests

`npm test` before the modules existed: `tsc` exited 1. It could not find `./cli.js`, `./oracle.js`, `../host.js`, or `./call.js`.

`npm test` after: 15 passed, 0 failed.

```
the cli maps the process platform onto a host
an unknown platform is refused because its host module is missing
a host module that does not load is refused
pcm rms is the root mean square of little-endian samples
silence is under the speech floor and a steady tone is over it
rgb average of the four quadrants is the midpoint
colour averages more than twenty apart tell bars from quadrants
the call joins both clients, sends the pictures, and records the remote window
a microphone that is not the fixture is selected before the camera
an operator database url is the one the server process gets
a missing session does not start a process or invent a sign-in
the remote sink must carry the speech clip
the recorded frame must be the share picture
the mp4 must exist and be longer than one second
a client that never shows Join is stopped
```

The microphone test's filter also matched the client launch line, because that line contains the directory `fixtures`. The assertion now skips lines that start with `client `, and still requires the find and click order. No GUI, Appium, or ffmpeg process was started. Rust was not changed.

## Concerns

1. `crates/chatd/src/session.rs` builds the scratch session name as `TINYCLIENT_SESSION_` plus `Network::Tiny.label().to_uppercase()`. That label is `Tiny server`, so chatd reads `TINYCLIENT_SESSION_TINY SERVER` (the space is in the name). Node does pass that name through to a child. The scenario sets it to the same JSON as `TINYCLIENT_SESSION_TINY`. It does not add a sign-in path. A desktop run still depends on that spaced name until chatd looks up `TINYCLIENT_SESSION_TINY`.

2. The scenario waits for the accessible name `Join channel`. It does not open a voice channel. A fresh `TINYCLIENT_DIR` has no saved room, so both windows can sit on the server list until the join timeout.

3. The colour check averages every pixel of the PNG taken from the mp4. The unit test feeds a pure quadrant buffer and a pure red buffer. A recording of the tinyclient window also contains the shell, so the average can miss `{127.5, 127.5, 127.5}` while the share is visible. On Windows, `gdigrab` selects `title=tinyclient`, and both clients use that title, so the grab can be the sender's window.

4. macOS capture uses avfoundation input `1:none` unless `TINY_E2E_AVFOUNDATION` is set, and the Mac2 session uses bundle id `dev.tinyclient.app`. A binary that is not that bundle fails the session. Linux needs the `atspi2` driver installed (`appium driver install --source=npm @itsmeaj/appium-linux-driver`) and an X11 `DISPLAY` for `x11grab`.

5. `serve` still needs the operator's sign-in and scanner environment (`TINYSERVER_PUBLIC_URL`, the OIDC variables, and a scanner). This task did not run `npm run call`. There is no `TINYSERVER_BIN`, session JSON, or desktop session in this worktree run.

`docs/superpowers/specs/task-rust-report.md` is still untracked. It was not part of this commit.
