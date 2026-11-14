Run against the real release-profile app with the standard invented Discord demo data, in an
isolated data directory. Before starting the driver, open Casimir Thorne's profile through
People search (or a server member list). Keep the entire profile visible. Desktop uses
`chatd --seed-demo`; phones use the lab's demo app, as the smoke scenario does. Reset the
fixture between base and branch. No real Discord account is used.

The lab records the run and supplies E2E_OUT and E2E_T0. Each platform wrapper runs drive.py.
Install Python, Pillow and Tesseract on the driver host; desktop also needs PyAutoGUI and
screen/input permission. Android uses adb; iOS uses E2E_UDID and the lab's existing
E2E_LAB/ios-ui test runner. These are screen input and OCR checks, with no store or RPC reads.

The driver verifies the invented stranger's name and Add friend, clicks it, verifies Request
sent and Cancel request while Add friend is absent, cancels, then verifies Add friend returns.
On the base it fails at the first Add friend assertion. Repeat with the floating card on
desktop. API request shape, refusal, and duplicate-request coverage live in the Discord
adapter integration test; this UI scenario exercises the demo adapter and does not certify
Discord's live API. Lab provisioning and recorded runs are still required before claiming proof.
