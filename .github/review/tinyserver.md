# tinyserver

An axum 0.8 API over Postgres (sqlx 0.8, migrations in `crates/db/migrations`), with an SFU for
calls (`crates/sfu`), WebAuthn passkeys (webauthn-rs) and OpenID Connect sign-in. A community
runs its own. The privacy guarantees and what each is tested by are `docs/privacy/guarantees.md`;
the threat model is `docs/privacy/threat-model.md`. A diff that weakens a guarantee marked
"Implemented, tested" is a blocker even if no test is named in the diff.

## The server never holds what it must not

- It stores ciphertext and no key that opens it. Text and attachments of an encrypted
  conversation are opaque: no sniffing, scanning, thumbnailing or logging of their contents, name
  or type. The server's external sender key is a file (mode 0600), never a database row, and it
  signs proposals and opens nothing.
- A plaintext body for an encrypted conversation is refused in the Postgres definer functions.
  A change that lets one through, or adds a downgrade in either direction, is a blocker.
- A canary test (`a_canary_said_only_through_encrypted_paths_is_nowhere_on_the_server`) scans
  tables, logs and files for plaintext. A new column, log line, error string, temp file or event
  that carries message text, an attachment name, a key or a token is a blocker.
- Logging: `docs/logging.md`. No telemetry or exporter. A client address stays off the operator
  sink. `reqwest`, `hyper`, `h2`, `sqlx`, `tower` and `str0m` never log above `warn`.

## Access

- Every route checks who is asking and that they belong to the thing they name. A new handler with
  no membership, tenant or ownership check, a `404` that becomes a `403` and so confirms an id
  exists, or a query that reads a row by id alone, is a blocker.
- Sessions, invite codes, claim links, device-link codes and tokens: compared in constant time,
  single use where they are meant to be, expiring, and never in a URL that gets logged.
- Passkey and OIDC flows: state, nonce and redirect URI are checked; a failed check ends the
  flow. A step moved, skipped or made optional is a blocker.
- Anything that turns user input into a path, a URL the server fetches (SSRF), a SQL string (not a
  bind parameter), or a shell command is a blocker.

## Database and migrations

- A migration that has shipped is never edited; a new numbered file is added. The next number is
  the highest plus one with no gap or duplicate.
- A migration must work against the data already in a community's database and say its rollback
  in the pull request. Long locks on a large table (`ALTER ... NOT NULL` with a default rewrite,
  an index not built `CONCURRENTLY`, a full-table update) are a major, a blocker when the table
  is messages or events.
- Retention and deletion are real: a new table that holds a person's data is removed with the
  account and swept by retention (`docs/privacy/retention-and-deletion.md`).
- A transaction that spans a network call, or a query inside a loop that a client can make long,
  is a major.

## Compatibility and limits

- Older clients keep working (`docs/server.md`; the Compatibility table in `docs/calls.md`;
  `docs/privacy/migration.md`). `GET /v1/capabilities` answers 404 on servers from before it, so
  clients ask first. Removing or retyping a response field, changing a status code a client
  matches on, or requiring a new request field with no default is a blocker.
- Every body, upload, websocket message, list and rate that a client controls has a limit, and
  the limit is checked before the allocation. A new route without one is a blocker.
- The SFU relays packets; a change that forwards before it authorises the sender, or that sizes a
  buffer from a packet header, is a blocker.

## Deploy

CI only tests. Nothing deploys from a pull request. A workflow, script or compose change that
deploys, tags or pushes an image on its own is a blocker. Provider names, hostnames, IPs and plan
sizes of the community host stay out of the repository.
