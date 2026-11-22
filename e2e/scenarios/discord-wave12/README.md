# Discord Wave 12

The driver launches the desktop app against its loopback Discord fixture with a fresh local profile. The account uses Nitro Basic and READY names its authentication session hash. The fixture serves these account lists on request.

| List | Fixture |
| --- | --- |
| Devices | **Wave Browser** with the current hash and **Wave Desktop** with a different hash, both with an OS, location and last-use time |
| Authorized apps | **Wave Calendar** |
| Connections | **Wave Music**, a Spotify connection |

The fixture removes the named entry after a successful logout or DELETE. Each run starts with clean lists. Use the lab's default browser with a clean browser profile for the external link step.

The drivers open Settings, sign out the second device and check that the current device is still listed. They revoke the app, disconnect the linked account, and check the displayed plan and handoff rows. The Family Centre handoff is checked in the browser address bar. The base fails when looking for the account handoff.
