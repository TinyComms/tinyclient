# Desktop capture Wave 13

Run this scenario on an X11 desktop with a compositor, on macOS with Screen Recording granted to tinyclient, and separately on a Wayland desktop with a Global Shortcuts portal. The Linux lab adapter currently provisions an X11 session. Its default run covers capture. Run the same driver in the Wayland seat with a desktop recorder to cover portal shortcuts.

Each driver uses the lab's `E2E_APP_DIR`, `E2E_OUT` and `E2E_T0`. Set `E2E_DESKTOP_CAPTURE_FIXTURE` to an isolated lab fixture JSON on the driver host or put it at `~/.config/tiny-lab/desktop-capture-wave13.json`. It contains:

- `sender_seed` and `viewer_seed` directories for distinct invented users on a disposable Tiny server. Both accounts start on the server page with the voice room named by `room` visible. The sender uses push-to-talk on the Wayland seat. Bind talk, priority talk, mute and deafen to F8 through F11 respectively and approve them in the portal. The user must have permission to use priority speech.
- `window_geometry` and `resized_geometry` in Tk geometry syntax, initially placing the source underneath the tinyclient window in the first display. `second_geometry` places the green source on the second display. `tiny_window_title` is the tinyclient window title, used to check exclusion.
- `share_button` and `picker_share_button` coordinates for the sender's share control and the picker's confirmation button. `viewer_video_bounds` gives the left, top, right and bottom of the viewer's video area, excluding the source windows.
- For Wayland, `focus_geometry` positions the unrelated window so the call's status stays visible. `sender_focus` and `deafen_button` are the coordinates used to confirm deafen through its tooltip.

The desktop driver uses Tk for the colored fixture windows, Pillow and Tesseract for pixel assertions, and PyAutoGUI for desktop input. X11 also needs xdotool for window activation. The Wayland run uses Spectacle screenshots and the lab seat's ydotool for input. Use a theme and language that match the driver's English labels.

The base build fails the two-display inventory on X11 and macOS. On Wayland it fails to show Talking while another app has focus. The branch must show the second display and then the selected covered window in the viewer, follow a resize, end that share when the source closes, and handle the four portal shortcuts in the Wayland run.
