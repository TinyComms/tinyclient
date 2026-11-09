# Encrypted membership after reconnect

Required recorded real-app scenario, using isolated profiles and invented accounts:

1. Sign in Ada and create an encrypted direct conversation with Bea before Bea
   has registered an encryption device. Ada's device is the sole group member.
2. Close Ada's app. Sign in Bea and open the direct conversation. Verify that
   Bea cannot send yet. The server should have a signed, uncommitted Add for Bea.
3. Start Ada's release-profile app again. Leave it at the conversation list;
   do not open the conversation, send a message, or press Update encryption.
4. Restart Bea's app, then send a unique message from Bea. It must send and
   Ada must be able to read it. Reply from Ada and verify Bea reads it too.
5. Repeat with Ada's process left running but its network disconnected while
   Bea's Add is created. Restore the network and repeat step 4. This checks
   socket resume as well as cold startup. Allow 90 seconds for bounded retries
   on older servers that batch membership changes.

Run the same scenario on the base and branch on every platform. The base must
fail at step 4 because connection/list refresh does not process the pending Add.
The lab must record and publish these runs; no full GUI proof is claimed here.
Platform UI automation and lab recordings are still pending, so this scenario
does not currently certify the change and is not in the release suite.

The focused source regression uses real MLS devices and the actual conversation
list publication path with an in-process server. It checks that the creator
commits the missed Add, that the recipient takes its Welcome without a live
hint, and that the recipient's first message opens on the creator:

```sh
cargo test --locked -p chatd --lib publishing_conversations_settles_an_add_missed_while_offline -- --nocapture
```

For a behavioral base comparison, transplant the test and its fake server list
routes, leaving production code unchanged. A missing test is not a base failure.
This source regression supplements the required real-app scenario; it is not
`e2e-proof` or a substitute for recordings.
