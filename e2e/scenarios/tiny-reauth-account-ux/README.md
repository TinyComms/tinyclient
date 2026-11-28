# tiny-reauth-account-ux

Proves that re-authenticating an expired Tiny server account resolves its saved server origin, skips the invite and host prompts, launches browser sign-in directly for that exact server, completes the PKCE callback into `SignInStep::Done`, preserves fresh `Invite` sign-in when no account/server is provided, and keeps `Request::SignInBegin` wire serialization backwards-compatible.
