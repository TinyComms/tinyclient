# auth.md

tinyclient does not run an account service. There is no agent registration, no client id, and no credential to claim or revoke.

The desktop app signs in to Slack, Discord, Telegram, Microsoft Teams, Signal, and Android Messages on the computer where it is installed. Those sessions stay in the operating system keychain. tinyclient.io does not issue access tokens, does not accept credentials, and does not proxy messages.

## Audience

People and agents reading the public website.

## Registration

There is no registration endpoint. Do not POST credentials anywhere on tinyclient.io.

## Protected resources

None. The pages, docs, and download redirects are public. `/.well-known/oauth-protected-resource` and `/.well-known/oauth-authorization-server` are intentionally absent: publishing an issuer or a token endpoint would point agents at a server that does not exist.

## How to use the site

Read https://tinyclient.io/llms.txt and https://tinyclient.io/docs/. Downloads are linked from the getting-started page. No bearer token is required.
