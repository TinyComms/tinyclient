Hand-written Slack payloads for `tests/slack.rs`, shaped like the documented responses of the
methods they are named after. No real workspace, person, token or message is in here.

`T01ALPHA/` and `T02BETA/` are two workspaces of one sign-in. A file is the answer to the method
in its name; `<method>.<channel>.json` answers for one channel, `.older` for the page before.
`users.json` is not a response: the fake serves `users.info` and the edge cache lookups from it.
`https://files.slack.com` is rewritten to the fake's own address when served, so no test can
reach Slack. `.after` answers a request with `oldest`, `.around` one with `inclusive`.
The same address percent-encoded (inside a gravatar `d=` fallback) is rewritten too.
