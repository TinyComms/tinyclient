# tinychat

Native client for Telegram, Discord, Slack, and Teams. One process stays up for chat. A second process starts for a call or screen share and exits when it ends.

| Crate | Role |
| --- | --- |
| `chatd` | Always on. Unix socket at `~/Library/Application Support/tinychat/chat.sock`, SQLite next to it. Runs the network adapters. Spawns `mediad` for a call and reaps it. |
| `mediad` | Voice, video, and screen share. Exits immediately when no call is active, and when chatd closes its stdin. |
| `tinychat-login` | The sign-in window for Slack and Discord. Runs only while you sign in, then exits. |
| `tinychat` | Shell. Draws what `chatd` reports. GPUI is behind the `gui` feature so the daemons and tests never build a GPU stack. |
| `protocol` | Length-prefixed JSON frames shared by the binaries. Local IPC only. |

```
cargo test
cargo run -p chatd
cargo run -p tinychat                    # status and conversations as text
cargo run -p tinychat --features gui     # the window
cargo run -p mediad
```

What is and is not in the product: [docs/stories.md](docs/stories.md). How it looks:
[docs/design.md](docs/design.md). How the processes fit together: [docs/architecture.md](docs/architecture.md).

## Builds

GitHub Actions builds on every push and pull request (`.github/workflows/ci.yml`). Each run
uploads three artifacts: **tinychat-macos** (`tinychat.app`), **tinychat-linux-x86_64** (four
binaries in a `.tar.gz`), and **tinychat-windows-x86_64** (four `.exe` files in a `.zip`).
Tag pushes matching `v*` also publish the same three packages on the release
(`.github/workflows/release.yml`).

| OS | Data directory |
| --- | --- |
| macOS | `~/Library/Application Support/tinychat` |
| Linux | `~/.local/share/tinychat` |
| Windows | `%APPDATA%\tinychat` |

The macOS CI bundle is ad hoc signed. macOS Gatekeeper will warn the first time you open it;
allow it in System Settings if you trust the build.

## Read this before you sign in

tinychat is an unofficial client. It is not made, endorsed, or supported by Slack, Discord,
Telegram, or Microsoft.

- **Slack.** tinychat signs in the way Slack's web app does and uses that session. Slack does not
  offer this to third-party clients, so it can stop working without notice, and it is outside
  Slack's terms for API use. If this is a work Slack, your employer's security policy may not
  allow unofficial clients. Ask first.
- **Discord.** Discord's terms prohibit third-party clients that use your user account. Discord
  can lock or ban an account for it, and has. tinychat only does what you do by hand (it does not
  automate anything, and it asks for members and presence only for what is on your screen, as the
  official app does), which lowers the risk and does not remove it. To Discord and Slack it
  presents itself the way the sign-in window does, as the web client in the Safari you have
  installed, because one consistent identity draws less attention than an unknown one.
  Use it on an account you can afford to lose access to, or not at all.
- **Teams.** tinychat signs in as Microsoft's own Teams client and uses that client's chat
  service, which Microsoft does not offer to others. Your organization's policy applies, and its
  administrators can see the sign-in.
- **Telegram.** Telegram permits third-party clients, so this one is within its terms.

Your session for each network is stored in the macOS login keychain under the service `tinychat`.
Messages are cached in `~/Library/Application Support/tinychat/chat.sqlite`, and avatars and
attachments you have looked at in `media/` beside it, both unencrypted. Signing out of an account
in tinychat deletes its session, its cached messages and its media.

You use tinychat at your own risk.

## State

Discord and Slack have run against real accounts. Teams, Telegram and calls have only run against
local fakes written from documentation and other clients' source, so expect the first real use of
each to find bugs.

| Network | Sign-in | Reading | Writing | Calls |
| --- | --- | --- | --- | --- |
| Discord | works | Servers, channels, threads, DMs, friends with presence, avatars, images, files, link cards, reactions, replies. Seen on a real account. | Text, replies, into a thread; react, edit, delete, friend requests and own status are built and not yet tried on a real account. | Voice is built, with Discord's end-to-end encryption, and has never joined a real call. It needs the app bundle (below) for the microphone. |
| Slack | works, every workspace of one sign-in | Sections, channels, threads, DMs, avatars, files, reactions. Seen on a real workspace. | As Discord, less friend requests. | Huddles open in Slack. |
| Teams | built, never tried. Work and school accounts only; an organization can refuse it, and then nothing tinychat does helps. | Teams, channels, chats, reply chains as threads. | Text, react, edit, delete. | Calls and meetings open in Teams. |
| Telegram | built, never tried. Needs an app id (below). | Chats, folders, topics, photos as thumbnails. | Text, react, edit, delete. | Calls open in Telegram. |

Not built: sending files, search, notifications and a dock badge, settings, screen share.

To sign in, run `chatd` and the shell without `TINYCHAT_DIR` and press Sign in beside a network.
Discord, Slack and Teams show their own sign-in page in a separate window that closes when you
are in. Telegram shows a code to scan with your phone.

### Telegram needs your own app id

Telegram asks every client to register. The id is not in this repository: register an app at
my.telegram.org (the name must not contain "Telegram") and build with the two values in the
environment.

```
TINYCHAT_TELEGRAM_API_ID=... TINYCHAT_TELEGRAM_API_HASH=... cargo build -p chatd
```

A binary built this way carries your id, and Telegram holds its owner responsible for what the
app does. Give such a binary to other people only if you accept that. Telegram's terms also mean
tinychat cannot hide that you read a message or are typing there.

### Calls need the app bundle

macOS gives microphone access to an app, not to a bare program. `packaging/bundle.sh` builds
`target/release/tinychat.app` with all four programs inside; open that to make a call.

## Looking at the shell without accounts

`TINYCHAT_DIR` moves the socket and database and turns sign-in off, so a scratch directory never
touches the keychain. `--seed-demo` fills it with invented servers, friends and conversations, one
of which (`#showcase`) holds every kind of message. It refuses to run without `TINYCHAT_DIR`.

```
TINYCHAT_DIR=/tmp/tinychat-demo cargo run -p chatd -- --seed-demo
TINYCHAT_DIR=/tmp/tinychat-demo cargo run -p tinychat --features gui
```

`--seed-demo=live` also keeps one conversation busy: someone types, a message lands, a reaction, an
edit. `--features gui-shot` adds `--shot out.png`, which renders the window to a file and quits.
