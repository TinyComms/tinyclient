# Voice recording state

Run `drive.linux.sh` with the lab's standard `E2E_APP_DIR`, `E2E_OUT`,
`E2E_T0`, `DISPLAY` and `XAUTHORITY`. Requires tesseract and ffmpeg.
Set `E2E_MIC_X` and `E2E_MIC_Y` to the microphone button's screen coordinates
and `E2E_PREVIEW_X` / `E2E_PREVIEW_Y` to the staged note's play button coordinates
in the maximized 1280x800 window on the lab host. These must be calibrated
for the host's font scaling before requesting proof.

The driver seeds invented demo data, opens the first conversation, and
clicks the real composer microphone twice. It reads pixels to assert the
recording caption and then the staged voice note. It plays, pauses and resumes
the note through its own play button, asserting the visible playback status
without sending. On the base before preview support, the playback assertion
fails because the staged note has no play control.
The real recorder/encoder runs with a generated microphone tone so the lab
does not record anybody's speech. This checks the recording state lifecycle,
not Linux device selection or Telegram's upload service.

The recording button must display a stop square while the caption is active.
Inspect this in the lab's recording; a caption alone does not prove the icon rendered.
The `recorder_upload` integration test additionally records real Ogg Opus through
mediad, plays/pauses/resumes/replays that exact file, then opens it with the upload
helper, checks its MIME type and cleanup, and verifies that unrelated outbox and
private cache files remain blocked. Removing the note stops its preview. A Telegram lab
send from an invented account is still needed to prove delivery in the real app.

The shared fix also reaches Windows, macOS, Android and iOS. Drivers and
recorded base/branch runs for those platforms are still needed before this
scenario can provide full `e2e-proof`. No lab proof is claimed here.
