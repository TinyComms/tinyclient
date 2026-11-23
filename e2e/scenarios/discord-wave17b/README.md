# discord-wave17b

Proves this build joins a Discord stage, names an Activity by its application, scales soundboard gain, keeps a raised hand suppressed, and lists a guild sound.

The lab starts `chatd` with `TINYCLIENT_PROVE=discord-wave17b`. The process exits 0 when those checks pass. A build from before this wave ignores the variable and stays up, so the driver fails there.

`drive.py` is the check. `drive.windows.ps1`, `drive.linux.sh`, `drive.macos.sh`, `drive.ios.sh`, and `drive.android.py` start it on each lab host.
