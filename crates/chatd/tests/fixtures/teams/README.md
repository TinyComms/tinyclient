# Teams fixtures

Hand-written. No payload here was captured from Teams; nobody on this project has run the
adapter against a real account yet. Each file follows the shape a working third-party client
reads or a real fixture shows:

| File | Shaped after |
| --- | --- |
| `authz.json` | fossteams/teams-api `pkg/auth_test.go` (a real answer, trimmed) |
| `teams.users.me.json` | fossteams' CSA fixture for teams and channels; `chats[]` after the fields Squads' `Chat` struct reads |
| `messages.*.json` | fossteams `resources/chatsvcagg/messages/messages-1.json`, with `properties` as purple-teams, Squads and mautrix-teams parse them (`emotions` an array, `mentions` / `files` / `cards` JSON held as text) |
| `thread.*.json` | `GET /v1/threads/{id}` as mautrix-teams reads it (`members[]`, `properties`) |
| `users.json` | `fetchShortProfile` values as purple-teams and Squads read them |
| `presence.json` | `getpresence` items as purple-teams reads them |

The test's fake (`tests/teams.rs`) replaces Microsoft's hosts in these files with its own
address, and builds the token, trouter and registrar answers itself.
