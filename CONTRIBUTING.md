# Contributing to tinyclient

Thank you for looking. Questions and ideas are welcome on [Discord](https://discord.gg/kJHK7BNvJP).
This file covers how to build and test, how the code is written, and how a network adapter is
put together. The map of the system is [`docs/architecture.md`](docs/architecture.md);
read its first two sections before changing anything across a process boundary.

A security problem is not an issue: report it privately as [SECURITY.md](SECURITY.md) says.

## Build

You need current stable Rust and `protoc` (Signal's libraries compile protobufs at build time:
`brew install protobuf`, or `apt install protobuf-compiler`). On Linux, install the libraries
listed under "Build from source" in the [README](README.md#build-from-source). With Nix,
`nix develop` (or `direnv allow`, for `.envrc`) provides all of it.

The Nix package (`packaging/nix/package.nix`) keeps a hash for each git dependency in
`Cargo.lock`. When a change moves one to a new rev, run `packaging/nix/update-hashes.sh` (it
needs Nix) to rewrite them.

```sh
cargo build -p chatd -p mediad -p login    # the daemon and its helpers
cargo run -p tinyclient --features gui     # the window; it starts the chatd beside it
cargo run -p tinyclient                    # status and inbox as text, no GPU stack
packaging/bundle.sh                        # macOS: target/release/tinyclient.app
```

Feature flags:

| Crate | Feature | Effect |
| --- | --- | --- |
| `chatd` | `signal` (default) | Links presage and libsignal (AGPL-3.0). `--no-default-features` leaves Signal `Unsupported`. |
| `tinyclient` | `gui` | The GPUI window. Without it the binary prints status and the inbox as text. |
| `tinyclient` | `gui-shot` | Adds `--shot out.png`: render the window to a file and quit, for design review and driven tests. Such a shell never marks anything read. |

Telegram needs a registered app id at build time (`TINYCLIENT_TELEGRAM_API_ID`,
`TINYCLIENT_TELEGRAM_API_HASH`); see the README. Without it, everything else works and the tests
still run Telegram against its scripted wire. With direnv, export both in `.envrc.local`, which
`.envrc` loads and git ignores.

## Run without accounts

Never run `chatd` against your real data directory while developing. Point it at a scratch one:

```sh
TINYCLIENT_DIR=/tmp/tinyclient-demo cargo run -p chatd -- --seed-demo=live
TINYCLIENT_DIR=/tmp/tinyclient-demo cargo run -p tinyclient --features gui
```

With `TINYCLIENT_DIR` set, chatd never reads or writes the credential store, and sign-in runs
only a helper a test names. `--seed-demo` fills the directory with invented data served by an
adapter with no network behind it (`crates/chatd/src/demo/adapter.rs`). Variants and the timing
knobs (`TINYCLIENT_DEMO_ANSWER_MS`, `TINYCLIENT_DEMO_UPLOAD_MS`, ...) are listed under "The demo"
in [`docs/architecture.md`](docs/architecture.md).

## Test

```sh
cargo test --workspace
```

This is what CI runs on macOS, Linux and Windows for every push and pull request. It never
builds the GPU stack. Most of chatd's tests start a real `chatd` in a scratch directory and talk
to it through its socket, with the network replaced by a local fake:

- HTTP and websocket networks (Discord, Slack, Teams) read their addresses through
  `adapters::net::address`, which honours variables such as `TINYCLIENT_DISCORD_API` and
  `TINYCLIENT_SLACK_SOCKET` only in a scratch directory. The test serves recorded traffic from
  `crates/chatd/tests/fixtures/<network>/` and hands over a session in
  `TINYCLIENT_SESSION_<NETWORK>`.
- Telegram and Signal have no HTTP to fake. Each adapter talks to its library through a `Wire`
  trait (`adapters/<network>/wire.rs`); `TINYCLIENT_TELEGRAM_FAKE` and `TINYCLIENT_SIGNAL_FAKE`
  put a scripted wire (`fake.rs`) under the adapter instead of the real one.
- The call tests build `mediad` and run it with a generated tone and a fake voice server, so they
  need no microphone and no Discord.

Run one network's tests with `cargo test -p chatd --test discord`. If you change the window,
check the GUI still builds: `cargo check -p tinyclient --features gui`.

## How the code is written

Read a few files near the one you are changing and match them. In short:

- **Short, plain sentences**, in comments, docs, error messages and commit messages. No jargon
  where a word will do.
- **Comments say why.** What the code does should be clear from the code; a comment records the
  reason, the constraint, or the evidence (a measurement, a protocol document, another client's
  source) behind a choice. Large decisions go at the top of the module (see the headers of
  `adapters/telegram.rs` and `adapters/signal.rs`).
- **Errors the user sees are words**, specific and actionable: "trail.mov is too big for Discord
  (limit 10 MB)", not an error code.
- **Everything is bounded.** Pages, caches, lists and strings from a network have caps. An
  adapter holds no tree, member list or history in memory; it writes to the store as data
  arrives.
- **Idle means idle.** No loop of short sleeps and no polling timers. Block on the socket or the
  inbox; `Context::pause` is the one interruptible wait.
- **Nothing on the shell's main thread** that reads a socket, a file or decodes an image.
- **No new async runtime and no second TLS stack.** chatd uses the system TLS through `ureq` and
  `tungstenite`. Telegram and Signal each own one current-thread tokio runtime on their adapter's
  thread, built only when that network is in use. Say why in `Cargo.toml` beside any new
  dependency, as the existing lines do; size and portability decide.
- **The protocol only grows.** A field added to `crates/protocol` has `#[serde(default)]` and is
  left off the wire when empty, so an older frame still reads.
- **Never log a credential.** Anything that might carry an address or a header goes through
  `protocol::redact` (chatd: `adapters::net::redact`) before it is printed. Add a pattern there
  if your network has a new kind of secret, with a test.

There is no rustfmt configuration and the code is not formatted with `cargo fmt`'s defaults.
Do not reformat files you are not otherwise changing.

## Adding a network adapter

There is no adapter trait. An adapter is a module under `crates/chatd/src/adapters/` that
exports three functions, and talks to the rest of chatd through two types in
[`adapters/mod.rs`](crates/chatd/src/adapters/mod.rs):

- `Command`: everything chatd asks of an adapter (send, react, fetch history, sign out, ...).
  Serve the ones you can; end your `match` with `other => other.unsupported(ctx)`, which answers
  the rest so nothing waits, and keeps working when new commands are added.
- `Context`: everything an adapter tells chatd. Each call writes the store and announces the
  change (`ctx.tree`, `ctx.message`, `ctx.history`, `ctx.presence`, `ctx.sent`, ...). An adapter
  never touches the store's tables or the change list itself.

The three functions:

```rust
/// The accounts inside one stored session (one Slack sign-in holds a workspace each).
/// Account ids name directories: letters, digits, `.`, `_`, `-`.
pub fn accounts(session: &str) -> Result<Vec<Signed>, String>;

/// The session with one account taken out, or `None` when nothing is left.
pub fn without(session: &str, account: &str) -> Option<String>;

/// Start this account's threads and return. Typically one thread keeps the connection up with
/// `keep_connected` and one serves commands with `serve_commands`.
pub fn start(ctx: Context, session: &str, inbox: Receiver<Command>) -> Result<(), String>;
```

To add one:

1. Add the variant to `protocol::Network` and `Network::ALL` (`crates/protocol/src/lib.rs`).
   The compiler then lists every `match` that needs it: `adapters/mod.rs` (`supported`,
   `accounts`, `without`, `start`), the media host allow-lists in `blobs/fetch.rs` and
   `upload.rs`, the server's sign-in, the demo, and the shell.
2. Write `adapters/<network>.rs`, with its parts in `adapters/<network>/`. Put the decision (which
   library or protocol, and why) at the top of the module.
3. Reach the network through `net::address(VAR, default)` so a test can point it at a fake, or,
   for a library with its own transport, behind a `Wire` trait with a scripted `fake.rs`.
4. Sign-in: a web page goes in `crates/login` (see `common.rs` and `teams.rs`); a native flow
   (QR code, phone code) is a `signin.rs` step function, as Telegram and Signal do.
5. Add the network's icon to `crates/tinyclient/assets/networks/` and its place in the shell's
   `NETWORK_ORDER` (`gui/layout.rs`).
6. Add `crates/chatd/tests/<network>.rs` driving the adapter through chatd's socket.
7. Add the network to the README's status table and its terms to "Read this before you sign in".

The rules every adapter follows are in "What an adapter must do" in
[`docs/architecture.md`](docs/architecture.md), and the account-safety rules in section 5 of
[`docs/product/07-budgets-and-risk.md`](docs/product/07-budgets-and-risk.md). The two that are
never bent: nothing is done that the user did not ask for, and nothing is enumerated or crawled.

## Pull requests

- One change per pull request, with a description that says what changed and why. Link the
  issue if there is one.
- `cargo test --workspace` passes, and `cargo check -p tinyclient --features gui` if you touched
  the window. New behaviour comes with a test; a bug fix with a test that failed before it.
- Commit messages are one plain sentence in the imperative, ending with a period, saying what the
  change does for the user or the code: "Find mediad and OpenSSL on Windows so the call tests can
  run there."
- A visible change to the window: attach a screenshot (`--features gui-shot` and `--shot out.png`
  make one from the demo without screen recording permission).
- New dependencies: say what they cost in size, and why nothing already in the tree will do.
- Keep the docs true. If your change makes a sentence in the README or `docs/` wrong, fix the
  sentence in the same pull request.

By contributing you agree that your contribution is licensed under the MIT license of this
repository.
