# Tiny direct call signaling and audio

This headless scenario runs two isolated instances of the current chatd live adapter and call/media core against a real disposable tinyserver with direct_calls enabled. It opens no window or audio device. It deliberately starts neither text device, and exercises first direct creation, ring, accept, encrypted call media, mute/unmute, leave, decline and cancel. Each process records and measures actual received PCM from the other process's generated tone. Both clients must report EndToEnd protection, name the other member in their secure roster, and hold the same nonempty group authenticator.

Set `TINY_DIRECT_CALL_FIXTURE` to a private JSON file containing `base_url`, `owner` and `peer`; each member has `user_id`, `token` and `display_name`. Use only synthetic sessions on an owned disposable server. The pair must have no direct yet. The server's API and its advertised ICE-TCP candidate must both be reachable. The test leaves the created direct in that disposable database.

Set `TINYCLIENT_TEST_MEDIAD` to an absolute path to the matching mediad binary, or let the existing test helper build it. Run `python e2e/scenarios/tiny-direct-calls/drive.py`. The driver requires one actual test plus its proof marker, so an older build or an empty filter fails. The PCM recordings and synthetic identity data live in a temporary directory removed when the probe exits. No sessions are printed or saved in the repository.

This proves native signaling and received audio on the platform where it runs. It does not exercise GUI interaction or certify another platform. The separate constructor regression verifies text sending remains blocked without enrollment.
