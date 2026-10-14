# Assets

- `networks/`: brand icons from [Dashboard Icons](https://dashboardicons.com)
  (homarr-labs/dashboard-icons, Apache-2.0). The logos are trademarks of their owners.
  `signal.svg` puts that set's Signal mark in white on a `#3a76f0` circle, the way Signal's app icon
  shows it, so it sits with the other filled icons.
- `icons/`: interface glyphs from [Lucide](https://lucide.dev) 0.469.0 (ISC), unmodified. They are drawn
  with a 1.5 point stroke at every size: `assets.rs` sets the stroke width from the drawn size.
  arrow-down, arrow-left, arrow-right, arrow-up-right, at-sign, bell-off, bell, building-2, check-check, check, chevron-down, chevron-left, chevron-right, chevron-up, circle-alert, circle-plus, clock, copy, corner-down-right, corner-up-left, download, ellipsis, external-link, eye-off, file, grip-vertical, hash, headphone-off, headphones, image, inbox, info, link, list, lock, log-in, log-out, mail, megaphone, message-circle, message-square, messages-square, mic-off, mic, monitor-up, panel-left, paperclip, pencil, phone-incoming, phone-missed, phone-off, phone-outgoing, phone, picture-in-picture-2, pin, plus, refresh-cw, reply, search, send-horizontal, settings, smile-plus, smile, square-pen, square, star, trash-2, triangle-alert, user, users, video, volume-2, volume-x, wifi-off, x.

All of them are compiled into the binary by `src/gui/assets.rs`.
