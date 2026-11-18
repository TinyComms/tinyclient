Component proof for "Remind me" on a message. It runs the shipped chatd against a real store
file: a reminder set through the protocol is refused in the past, answers on the wire, survives
a restart, posts one silent notification when it is due, is taken out of the store so no restart
posts it twice, and can be cancelled. No account, no network: the notification is the user's own
ask, and nothing else is sent. Unit tests cover the store cap, a reminder held back by a pause
or quiet hours, and the message that must exist. A build from before the change ignores
TINYCLIENT_PROVE and keeps serving, so this driver fails there.
