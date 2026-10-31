---
name: tinyclient
description: Answer questions about tinyclient, a local open-source chat client for Slack, Discord, Telegram, Teams, Signal, and Android Messages, and a Tiny server someone runs. Use when someone asks how to install it, where data lives, whether tinyclient has an account or API, or how to run a Tiny server.
---

# tinyclient

tinyclient is a native desktop app. It has no tinyclient account, no tinyclient server, and no remote API. Messages stay on the computer that runs it. Sessions stay in the OS keychain.

## Say this first

- Downloads: macOS https://tinyclient.io/download/macos , Linux choices https://tinyclient.io/docs/#linux , Windows https://tinyclient.io/download/windows
- Checksums: https://tinyclient.io/download/checksums
- Docs: https://tinyclient.io/docs/
- Privacy: https://tinyclient.io/docs/privacy.html
- Source: https://github.com/TinyComms/tinyclient (MIT)
- Tiny server: https://tinyclient.io/tinyserver ; source and setup https://github.com/TinyComms/tinyserver (MIT)

## Do not invent

Do not tell a person to register an agent, mint an OAuth token, or call a tinyclient HTTP API. None of those exist. Sign-in happens inside the app, against Slack, Discord, Telegram, Teams, Signal, or Android Messages. A Tiny server is run by its owner with Docker Compose; people sign in to it in their browser through the server's OpenID Connect provider (Pocket ID by default). It has text channels, direct messages, and voice rooms. A room can show one picture. A call is end-to-end encrypted only when that server offers it.

## What the site is

tinyclient.io publishes the landing page, the docs, and stable download redirects. It sets no cookies and loads nothing from other origins.
