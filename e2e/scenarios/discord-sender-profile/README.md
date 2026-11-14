# Discord sender nickname opens a profile

Start the real release-profile app in an isolated test account with invented identities.
Open a Discord server channel containing a visible message from Casimir Thorne (@casimir),
a non-friend. The visible author header must use that nickname. Keep the member list and
profile closed, and keep this person's name out of the sidebar and message body so the
sender header is the only matching screen label. The lab starts its recorder before the driver.

The driver locates only the sender name's text bounds (excluding its adjacent timestamp),
clicks it using native input, and requires @casimir and Add friend in the opened profile.
Desktop shows the floating profile card; phones show the Profile page. On desktop the base
fails at the profile assertion, while the branch must pass. Phones already supported author
taps: they are regression runs and are not expected to fail on the base. The lab must report
this distinction rather than claim a base-failing phone run.

The screen/input adapters come from ../discord-profile-add-friend/drive.py; its README lists
Tesseract, Pillow, PyAutoGUI and platform input requirements. E2E_OUT/E2E_T0 come from the lab.
Provision the fictional Discord account/channel fixture before each run. Recorded platform
proof is still pending; adding this scenario does not certify a UI run.
