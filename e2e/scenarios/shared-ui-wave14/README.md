# Shared UI, Wave 14

The desktop drivers use the real accessibility tree to open the demo Telegram conversation
with Ines Marchetti, read the Spanish message with its sender and time, activate Translate,
and press Show original. The Android driver shares an invented picture from MediaStore into
Nadia's conversation, checks that it is staged, opens the native photo picker and exercises
translation. On the iOS simulator the driver opens the native attachment source menu.

The macOS driver needs Accessibility permission and Linux needs the system Python AT-SPI
bindings and an active accessibility bus. The phone checks use the lab's demo build. Demo
accounts cannot receive a real message.

For the signed mobile device pass, start with two test accounts. In Photos or the gallery,
share a photo to Tiny. On iOS, save it in the extension, then open Tiny. Choose the other
account's conversation, verify the filename in the attachment tray, send it, and open the
received photo on the other account. Repeat with a web link and two files. Verify that choosing
a different conversation during a native picker keeps the file with the conversation that
opened the picker.
