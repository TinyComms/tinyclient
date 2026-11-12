# OpenH264 download retry

Run `sh e2e/scenarios/openh264-download-retry/run.sh`. It needs no devices and no network:
the scenario drives the tests of the download in `crates/mediad/src/picture/desktop.rs` —
three tries with pauses of 1, 4 and 16 seconds, the words each kind of failure says (the
host a VPN usually blocks, and "turn share or camera on again to retry"), the folder the
codec is kept in so a restart finds it, and a cached file that is not Cisco's being taken
away so it is fetched again.

On the base the scenario fails: the tests do not exist, so cargo reports no matching tests.
A download that is really blocked, on a real call, needs a network that drops
`ciscobinary.openh264.org`; that is not something the lab can arrange, and this scenario
does not claim it.
