# Discord Wave 11

Start the desktop app against a provisioned Discord fixture with invented accounts. Open the fixture server's text channel with Details enabled. Keep the poll, image, and bot embeds visible at the bottom of the conversation. Use an initially unvoted single-choice poll named **Where next?** whose **Studio** answer has a vote from **Wave Voter**. Include an image with the description **A lamp over a drafting table** and one bot message with the **Build 812** and **Coverage** embed cards.

The same server lists two joinable voice channels. **Wave Lounge** has the status **Studying** and contains **Wave Guest**. The signed-in account can manage that channel, set its status, and moderate voice members. The test service emits the usual status, vote, and voice-state events after accepted writes. Keep **Wave DM** accessible in navigation with a single unpinned message containing **Wave pin this message**.

The desktop drivers use the same OCR and input helper as the Discord profile scenario. Supply the fixture and start the recorder before running the driver. Reset the fixture between the base and branch runs. On a base without Wave 11 the driver fails when it tries to vote from the message. The branch run checks the on-screen result, voters, image description, status editor, voice settings, moderation menu, and pin action.
