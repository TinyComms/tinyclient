# Reauthentication history

Runs the packaged daemon with synthetic sessions and temporary encrypted files. It renews the token through the real Tiny session merge, then reopens the same device identity and cached plaintext and verifies both encrypted files stayed byte-identical. The driver requires the dedicated completion marker and a successful exit, and kills its own process on timeout. It never reads a real keychain or account profile.

Component proof, release:false. No native GUI, actual sign-in provider, or mobile certification is claimed. Unit regressions additionally cover multiple servers, changed account/address, explicitly supplied keys, and null/missing fresh keys.
