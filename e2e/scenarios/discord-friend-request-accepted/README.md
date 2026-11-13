# Discord friend request acceptance

Provision isolated Discord test accounts with invented identities, including Casimir Thorne.
With the real release-profile app connected to the requester account, have Casimir accept its
request. Start the recording with the inbox visible and Casimir Thorne's DM in the sidebar;
the acceptance must be in the newest visible page, without other unsupported messages.
Run the platform driver, which opens that DM using screen input and checks the rendered
acceptance text while rejecting the unsupported-message fallback. Reset app data and refetch
history for each base/branch run, so cached decoded messages do not mask the change.

The base fails the acceptance assertion. The branch must display a system line with the name
and “accepted your friend request.” The decoder integration test separately covers history
and live gateway delivery, including the saved friend nickname.

The driver reuses the screen/OCR adapters and dependencies documented in
../discord-profile-add-friend/README.md (Tesseract, Pillow, PyAutoGUI on desktop, adb on Android,
the existing lab UI runner on iOS). E2E_OUT and E2E_T0 come from the lab. No live credentials
or test accounts are provisioned by this scenario: that setup and the full recorded platform
runs are still required. No lab proof is claimed by adding the driver.
