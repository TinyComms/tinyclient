# telegram-wave18a

Verifies Telegram Wave 18A calling features (`#458`, `#448`, `#450`):

- 1:1 video calls (`phone.requestCall`/`phone.discardCall` with `video: true`, H.264 video negotiation, camera state `active`/`inactive`/`paused`/`low_battery`, camera picker & mobile flip)
- 1:1 screen share with system/window sound (`screencast` track, `ShareSend.sound` loopback mixing with echo subtraction, multi-picture `Call::pictures`, `Participant::sharing`)
- Encrypted group conferences (`t.me/call/<slug>`, `phone.createConferenceCall`, `phone.inviteConferenceCallParticipant` upgrading 1:1 -> conference, `phone.declineConferenceCallInvite`, `phone.deleteConferenceCallParticipants`, `phone.getGroupCallChainBlocks` / `updateGroupCallChainBlocks`, 4 verification emoji updating on every membership change, and fail-closed refusal on tampered chain blocks)
