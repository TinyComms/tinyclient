# Discord share with sound, a preview, and the call's privacy report

The driver starts this build's chatd, then mediad, both with `TINYCLIENT_PROVE=discord-wave15-share`.

chatd checks the stream key, that a viewer Discord names is kept with no profile loaded, that a preview with no captured frame is still a JPEG, and that the DAVE identity post is standard base64 of the 65-byte point. mediad checks that share sound is a real frame with this process's own playback removed, and that a Go Live report is marked as the stream so it does not replace the voice call's report.

A build from before this ignores the variable. chatd then stays up as a server, and mediad opens the audio engine, so the driver fails there. The check that two members share one 30-digit privacy code lives in mediad's library tests: it needs the MLS harness, which the shipped binary does not link.
