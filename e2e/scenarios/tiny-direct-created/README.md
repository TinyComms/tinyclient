# Newly received direct message

Run with `E2E_APP_DIR` pointing to a directory containing the built `chatd` and
`E2E_OUT` pointing to an evidence directory, then run `python drive.py`.

This is a **headless daemon regression**, not native window or phone UI proof.
It starts a real daemon core with fresh temporary storage and invented loopback
sessions, receives production-shaped `direct.created` and `message.created`
events over a WebSocket, and queries the conversation and inbox through normal
IPC. It also opens history through IPC and verifies the adapter's HTTP backfill.
The fixture's direct list stays empty, so a refresh cannot conceal a broken
event decoder. A replayed creation must preserve unread state.

With the scenario hook present and the decoder from base `82fefc20`, this fails
at `named direct published before first message`. With the fix it prints three
`direct-created:` evidence lines and exits zero. The lab still needs native
app input and recordings to certify discovery, clicking Message, and navigation;
this scenario alone does not certify those flows or post `e2e-proof`.
