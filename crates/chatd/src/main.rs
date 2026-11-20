use chatd::{spawn_core, Host};
use std::io::{self, Write};

fn main() -> io::Result<()> {
    // Before the support directory is touched: the packages' smoke tests run it on a bare system.
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("chatd {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("tiny-reauth-history") {
        if let Err(err) = chatd::adapters::tiny::prove_reauthenticated_history() {
            eprintln!("tiny-reauth-history: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-tiny-reauth-history:0");
        return Ok(());
    }
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("tiny-direct-created") {
        if let Err(err) = chatd::adapters::tiny::prove_tiny_direct_created() {
            eprintln!("tiny-direct-created: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-exit:0");
        return Ok(());
    }
    // The lab's tiny-send-identity driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("tiny-send-identity") {
        if let Err(err) = chatd::adapters::tiny::prove_tiny_send_identity() {
            eprintln!("tiny-send-identity: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-tiny-send-identity:0");
        return Ok(());
    }
    // The lab's leave-chime driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("leave-chime") {
        if let Err(err) = chatd::leave_prove::run() {
            eprintln!("leave-chime: {err}");
            std::process::exit(1);
        }
        return Ok(());
    }
    // The lab's join-order driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("join-before-room") {
        if let Err(err) = chatd::adapters::tiny::prove_join_before_room() {
            eprintln!("join-before-room: {err}");
            std::process::exit(1);
        }
        return Ok(());
    }
    // The lab's legacy-saved-speakers driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("legacy-saved-speakers") {
        if let Err(err) = chatd::adapters::tiny::prove_legacy_saved_speakers() {
            eprintln!("legacy-saved-speakers: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-legacy-saved-speakers:0");
        return Ok(());
    }
    // The lab's sealed-row driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("sealed-row") {
        if let Err(err) = chatd::adapters::tiny::prove_missing_seal() {
            eprintln!("sealed-row: {err}");
            std::process::exit(1);
        }
        return Ok(());
    }
    // The lab's sealed-preview driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("sealed-preview") {
        if let Err(err) = chatd::adapters::tiny::prove_sealed_preview() {
            eprintln!("sealed-preview: {err}");
            std::process::exit(1);
        }
        return Ok(());
    }
    // The lab's first-encrypted-message driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("first-encrypted-message") {
        if let Err(err) = chatd::adapters::tiny::prove_first_encrypted_message() {
            eprintln!("first-encrypted-message: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-exit:0");
        return Ok(());
    }
    // The lab's encrypted-reaction driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("tiny-e2ee-reaction") {
        if let Err(err) = chatd::adapters::tiny::prove_encrypted_reaction() {
            eprintln!("tiny-e2ee-reaction: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-exit:0");
        return Ok(());
    }
    // The lab's linkify driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("linkify") {
        if let Err(err) = chatd::adapters::tiny::prove_linkify() {
            eprintln!("linkify: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-exit:0");
        return Ok(());
    }
    // The lab's telegram-edited driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("telegram-edited") {
        if let Err(err) = chatd::adapters::telegram::prove_telegram_edited() {
            eprintln!("telegram-edited: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-exit:0");
        return Ok(());
    }
    // Exercise an open through the real Tiny adapter command loop.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("tiny-open-backfill") {
        if let Err(err) = chatd::adapters::tiny::prove_tiny_open_backfill() {
            eprintln!("tiny-open-backfill: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-exit:0");
        return Ok(());
    }
    // The lab's cache-before-enrol driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("cache-before-enrol") {
        if let Err(err) = chatd::adapters::tiny::prove_cache_before_enrol() {
            eprintln!("cache-before-enrol: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-exit:0");
        return Ok(());
    }
    // The lab's cache-open-without-device driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("cache-open-without-device") {
        if let Err(err) = chatd::adapters::tiny::prove_cache_open_without_device() {
            eprintln!("cache-open-without-device: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-exit:0");
        return Ok(());
    }
    // The lab's sealed-cache-preview driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("sealed-cache-preview") {
        if let Err(err) = chatd::adapters::tiny::prove_sealed_cache_preview() {
            eprintln!("sealed-cache-preview: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-exit:0");
        return Ok(());
    }
    // The lab's sealed-preview-attachments driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("sealed-preview-attachments") {
        if let Err(err) = chatd::adapters::tiny::prove_sealed_preview_attachments() {
            eprintln!("sealed-preview-attachments: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-exit:0");
        return Ok(());
    }
    // Long conversations must keep saving newly opened history across restarts.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("cache-history-limit") {
        if let Err(err) = chatd::adapters::tiny::prove_cache_history_limit() {
            eprintln!("cache-history-limit: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-cache-history-limit:0");
        return Ok(());
    }
    // A torn cache tail must not absorb the next complete encrypted record.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("cache-append-tail") {
        if let Err(err) = chatd::adapters::tiny::prove_cache_append_tail() {
            eprintln!("cache-append-tail: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-cache-append-tail:0");
        return Ok(());
    }
    // A failed sealed-cache append must not leave only a consumed receive ratchet.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("opened-history-cache-commit") {
        if let Err(err) = chatd::adapters::tiny::prove_opened_history_cache_commit() {
            eprintln!("opened-history-cache-commit: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-opened-history-cache-commit:0");
        return Ok(());
    }
    // The lab's keep-opened-sealed driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("keep-opened-sealed") {
        if let Err(err) = chatd::adapters::tiny::prove_keep_opened_sealed() {
            eprintln!("keep-opened-sealed: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-media-preserved:0");
        eprintln!("prove-exit:0");
        return Ok(());
    }
    // The lab's refused-list driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("refused-list") {
        if let Err(err) = chatd::adapters::tiny::prove_refused_list() {
            eprintln!("refused-list: {err}");
            std::process::exit(1);
        }
        return Ok(());
    }
    // The lab's reaction-reactors driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("reaction-reactors") {
        if let Err(err) = chatd::adapters::prove_reactors() {
            eprintln!("reaction-reactors: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-reaction-reactors:0");
        return Ok(());
    }
    // The lab's empty-list driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("empty-list") {
        if let Err(err) = chatd::adapters::tiny::prove_empty_list() {
            eprintln!("empty-list: {err}");
            std::process::exit(1);
        }
        return Ok(());
    }
    // The lab's before-blank driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("before-blank") {
        if let Err(err) = chatd::server::prove_before_blank() {
            eprintln!("before-blank: {err}");
            std::process::exit(1);
        }
        return Ok(());
    }
    // The lab's around-blank driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("around-blank") {
        if let Err(err) = chatd::server::prove_around_blank() {
            eprintln!("around-blank: {err}");
            std::process::exit(1);
        }
        return Ok(());
    }
    // The lab's kept-backfill driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("kept-backfill") {
        if let Err(err) = chatd::server::prove_kept_backfill() {
            eprintln!("kept-backfill: {err}");
            std::process::exit(1);
        }
        return Ok(());
    }
    // The lab's failed-start-offline driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("failed-start-offline") {
        if let Err(err) = chatd::adapters::prove_failed_start_offline() {
            eprintln!("failed-start-offline: {err}");
            std::process::exit(1);
        }
        return Ok(());
    }
    // The lab's signed-out-messages driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("signed-out-messages") {
        if let Err(err) = chatd::server::prove_signed_out_messages() {
            eprintln!("signed-out-messages: {err}");
            std::process::exit(1);
        }
        return Ok(());
    }
    // The lab's signed-out-threads driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("signed-out-threads") {
        if let Err(err) = chatd::server::prove_signed_out_threads() {
            eprintln!("signed-out-threads: {err}");
            std::process::exit(1);
        }
        return Ok(());
    }
    // The lab's connect-keeps-offline driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("connect-keeps-offline") {
        if let Err(err) = chatd::server::prove_connect_keeps_offline() {
            eprintln!("connect-keeps-offline: {err}");
            std::process::exit(1);
        }
        return Ok(());
    }
    // The lab's offline-queue driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("offline-queue") {
        if let Err(err) = chatd::server::prove_offline_queue() {
            eprintln!("offline-queue: {err}");
            std::process::exit(1);
        }
        return Ok(());
    }
    // The lab's pending-message-identity driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("pending-message-identity") {
        if let Err(err) = chatd::server::prove_pending_message_identity() {
            eprintln!("pending-message-identity: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-pending-message-identity:0");
        return Ok(());
    }
    // The lab's keycap-reaction driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("keycap-reaction") {
        if let Err(err) = chatd::adapters::tiny::prove_keycap_reaction() {
            eprintln!("keycap-reaction: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-exit:0");
        return Ok(());
    }
    // The lab's discord-voice-bitrate driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("discord-voice-bitrate") {
        if let Err(err) = chatd::adapters::prove_discord_voice_bitrate() {
            eprintln!("discord-voice-bitrate: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-discord-voice-bitrate:0");
        return Ok(());
    }
    // The lab's discord-voice-move driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("discord-voice-move") {
        if let Err(err) = chatd::adapters::prove_discord_voice_move() {
            eprintln!("discord-voice-move: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-discord-voice-move:0");
        return Ok(());
    }
    // The lab's discord-nitro-length driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("discord-nitro-length") {
        if let Err(err) = chatd::adapters::prove_discord_nitro_length() {
            eprintln!("discord-nitro-length: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-discord-nitro-length:0");
        return Ok(());
    }
    // The lab's discord-mark-unread driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("discord-mark-unread") {
        if let Err(err) = chatd::adapters::prove_discord_mark_unread() {
            eprintln!("discord-mark-unread: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-discord-mark-unread:0");
        return Ok(());
    }
    // The lab's discord-group-members driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("discord-group-members") {
        if let Err(err) = chatd::adapters::prove_discord_group_dm() {
            eprintln!("discord-group-members: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-discord-group-members:0");
        return Ok(());
    }
    // The lab's space-mark-all-read driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("space-mark-all-read") {
        if let Err(err) = chatd::server::prove_space_mark_all_read() {
            eprintln!("space-mark-all-read: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-space-mark-all-read:0");
        return Ok(());
    }
    // The lab's call-leave-clears driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("call-leave-clears") {
        if let Err(err) = chatd::media::prove_call_leave_clears() {
            eprintln!("call-leave-clears: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-call-leave-clears:0");
        return Ok(());
    }
    // The lab's messages-transport driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("messages-transport") {
        if let Err(err) = chatd::adapters::messages::prove_message_transport() {
            eprintln!("messages-transport: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-messages-transport:0");
        return Ok(());
    }
    // The lab's messages-phone-quiet driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("messages-phone-quiet") {
        if let Err(err) = chatd::adapters::messages::prove_phone_quiet() {
            eprintln!("messages-phone-quiet: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-messages-phone-quiet:0");
        return Ok(());
    }
    // The lab's messages-file-types driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("messages-file-types") {
        if let Err(err) = chatd::adapters::messages::prove_message_file_types() {
            eprintln!("messages-file-types: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-messages-file-types:0");
        return Ok(());
    }
    // The lab's messages-history driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("messages-history") {
        if let Err(err) = chatd::adapters::messages::prove_message_history() {
            eprintln!("messages-history: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-messages-history:0");
        return Ok(());
    }
    // The lab's messages-reply driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("messages-reply") {
        if let Err(err) = chatd::adapters::messages::prove_message_reply() {
            eprintln!("messages-reply: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-messages-reply:0");
        return Ok(());
    }
    // The lab's messages-failed-send driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("messages-failed-send") {
        if let Err(err) = chatd::adapters::messages::prove_messages_failed_send() {
            eprintln!("messages-failed-send: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-messages-failed-send:0");
        return Ok(());
    }
    // The lab's messages-media driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("messages-media") {
        if let Err(err) = chatd::adapters::messages::prove_message_media() {
            eprintln!("messages-media: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-messages-media:0");
        return Ok(());
    }
    // The lab's messages-folders driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("messages-folders") {
        if let Err(err) = chatd::adapters::messages::prove_message_folders() {
            eprintln!("messages-folders: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-messages-folders:0");
        return Ok(());
    }
    // The lab's messages-search driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("messages-search") {
        if let Err(err) = chatd::adapters::messages::prove_message_search() {
            eprintln!("messages-search: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-messages-search:0");
        return Ok(());
    }
    // The lab's teams-reactions driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("teams-reactions") {
        if let Err(err) = chatd::adapters::prove_teams_reactions() {
            eprintln!("teams-reactions: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-exit:0");
        return Ok(());
    }
    // The lab's teams-file-download driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("teams-file-download") {
        if let Err(err) = chatd::adapters::prove_teams_shared_file() {
            eprintln!("teams-file-download: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-exit:0");
        return Ok(());
    }
    // The lab's teams-loop-component driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("teams-loop-component") {
        if let Err(err) = chatd::adapters::prove_teams_loop_component() {
            eprintln!("teams-loop-component: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-exit:0");
        return Ok(());
    }
    // The lab's slack-custom-emoji driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("slack-custom-emoji") {
        if let Err(err) = chatd::adapters::prove_slack_custom_emoji() {
            eprintln!("slack-custom-emoji: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-slack-custom-emoji:0");
        return Ok(());
    }
    // The lab's teams-copy-link driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("teams-copy-link") {
        if let Err(err) = chatd::adapters::prove_share_links() {
            eprintln!("teams-copy-link: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-exit:0");
        return Ok(());
    }
    // The lab's teams-team-info driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("teams-team-info") {
        if let Err(err) = chatd::adapters::prove_teams_team_info() {
            eprintln!("teams-team-info: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-teams-team-info:0");
        return Ok(());
    }
    // The lab's messages-unread driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("messages-unread") {
        if let Err(err) = chatd::adapters::messages::prove_messages_unread() {
            eprintln!("messages-unread: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-messages-unread:0");
        return Ok(());
    }
    // The lab's signal-message-info driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("signal-message-info") {
        if let Err(err) = chatd::adapters::signal::prove_signal_message_info() {
            eprintln!("signal-message-info: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-signal-message-info:0");
        return Ok(());
    }
    // The lab's discord-forum-landing driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("discord-forum-landing") {
        if let Err(err) = chatd::demo::prove_discord_forum_landing() {
            eprintln!("discord-forum-landing: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-discord-forum-landing:0");
        return Ok(());
    }
    // The lab's teams-markdown driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("teams-markdown") {
        if let Err(err) = chatd::adapters::prove_teams_markdown() {
            eprintln!("teams-markdown: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-exit:0");
        return Ok(());
    }
    // The lab's telegram-block-contact driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("telegram-block-contact") {
        if let Err(err) = chatd::adapters::telegram::prove_telegram_block_contact() {
            eprintln!("telegram-block-contact: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-telegram-block-contact:0");
        return Ok(());
    }
    // The lab's telegram-markers driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("telegram-markers") {
        if let Err(err) = chatd::adapters::telegram::prove_telegram_markers() {
            eprintln!("telegram-markers: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-telegram-markers:0");
        return Ok(());
    }
    // The lab's telegram-custom-emoji driver. A build without this stays up and the driver fails.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("telegram-custom-emoji") {
        if let Err(err) = chatd::adapters::telegram::prove_telegram_custom_emoji() {
            eprintln!("telegram-custom-emoji: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-telegram-custom-emoji:0");
        return Ok(());
    }
    // The lab's telegram-quote driver. A build without this stays up and the driver fails.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("telegram-quote") {
        if let Err(err) = chatd::adapters::telegram::prove_telegram_quote() {
            eprintln!("telegram-quote: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-telegram-quote:0");
        return Ok(());
    }
    // The lab's telegram-album driver. A build without this stays up and the driver fails.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("telegram-album") {
        if let Err(err) = chatd::adapters::telegram::prove_telegram_album() {
            eprintln!("telegram-album: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-telegram-album:0");
        return Ok(());
    }
    // The lab's telegram-stickers driver. A build without this stays up and the driver fails.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("telegram-stickers") {
        if let Err(err) = chatd::adapters::telegram::prove_telegram_stickers() {
            eprintln!("telegram-stickers: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-telegram-stickers:0");
        return Ok(());
    }
    // The lab's telegram-mention-name driver. A build without this stays up and the driver fails.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("telegram-mention-name") {
        if let Err(err) = chatd::adapters::telegram::prove_telegram_mention_name() {
            eprintln!("telegram-mention-name: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-telegram-mention-name:0");
        return Ok(());
    }
    // The lab's telegram-large-files driver. A build without this stays up and the driver fails.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("telegram-large-files") {
        if let Err(err) = chatd::adapters::telegram::prove_telegram_large_files() {
            eprintln!("telegram-large-files: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-telegram-large-files:0");
        return Ok(());
    }
    // The lab's telegram-link-preview driver. A build without this stays up and the driver fails.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("telegram-link-preview") {
        if let Err(err) = chatd::adapters::telegram::prove_telegram_link_preview() {
            eprintln!("telegram-link-preview: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-telegram-link-preview:0");
        return Ok(());
    }
    // The lab's telegram-dice driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("telegram-dice") {
        if let Err(err) = chatd::adapters::telegram::prove_telegram_dice() {
            eprintln!("telegram-dice: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-telegram-dice:0");
        return Ok(());
    }
    // The lab's telegram-service-lines driver. A build without this stays up and the driver fails.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("telegram-service-lines") {
        if let Err(err) = chatd::adapters::telegram::prove_telegram_service_lines() {
            eprintln!("telegram-service-lines: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-telegram-service-lines:0");
        return Ok(());
    }
    // The lab's telegram-premium driver. A build without this stays up and the driver fails.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("telegram-premium") {
        if let Err(err) = chatd::adapters::telegram::prove_telegram_premium() {
            eprintln!("telegram-premium: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-telegram-premium:0");
        return Ok(());
    }
    // The lab's telegram-invoices driver. A build without this stays up and the driver fails.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("telegram-invoices") {
        if let Err(err) = chatd::adapters::telegram::prove_telegram_invoices() {
            eprintln!("telegram-invoices: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-telegram-invoices:0");
        return Ok(());
    }
    // The lab's telegram-location driver. A build without this stays up and the driver fails.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("telegram-location") {
        if let Err(err) = chatd::adapters::telegram::prove_telegram_location() {
            eprintln!("telegram-location: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-telegram-location:0");
        return Ok(());
    }
    // The lab's telegram-inline-bots driver. A build without this stays up and the driver fails.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("telegram-inline-bots") {
        if let Err(err) = chatd::adapters::telegram::prove_telegram_inline_bots() {
            eprintln!("telegram-inline-bots: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-telegram-inline-bots:0");
        return Ok(());
    }
    // The lab's telegram-live-location driver. A build without this stays up and the driver fails.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("telegram-live-location") {
        if let Err(err) = chatd::adapters::telegram::prove_telegram_live_location() {
            eprintln!("telegram-live-location: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-telegram-live-location:0");
        return Ok(());
    }
    // The lab's telegram-member-sections driver. A build without this stays up and the driver fails.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("telegram-member-sections") {
        if let Err(err) = chatd::adapters::telegram::prove_telegram_member_sections() {
            eprintln!("telegram-member-sections: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-telegram-member-sections:0");
        return Ok(());
    }
    // The lab's telegram-mute-until driver. A build without this stays up and the driver fails.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("telegram-mute-until") {
        if let Err(err) = chatd::adapters::telegram::prove_telegram_mute_until() {
            eprintln!("telegram-mute-until: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-telegram-mute-until:0");
        return Ok(());
    }
    // The lab's telegram-mark-unread driver. A build without this stays up and the driver fails.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("telegram-mark-unread") {
        if let Err(err) = chatd::adapters::telegram::prove_telegram_mark_unread() {
            eprintln!("telegram-mark-unread: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-telegram-mark-unread:0");
        return Ok(());
    }
    // The lab's message-remind driver. A build without this stays up and the driver fails.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("message-remind") {
        if let Err(err) = chatd::notify::prove_message_remind() {
            eprintln!("message-remind: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-message-remind:0");
        return Ok(());
    }
    // The lab's telegram-accounts driver. A build without this stays up and the driver fails.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("telegram-accounts") {
        if let Err(err) = chatd::adapters::telegram::prove_telegram_accounts() {
            eprintln!("telegram-accounts: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-telegram-accounts:0");
        return Ok(());
    }
    // The lab's telegram-secret-chat driver. A build without this stays up and the driver fails.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("telegram-secret-chat") {
        if let Err(err) = chatd::adapters::telegram::prove_telegram_secret_chat() {
            eprintln!("telegram-secret-chat: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-telegram-secret-chat:0");
        return Ok(());
    }
    // The lab's telegram-polls driver. A build without this stays up and the driver fails.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("telegram-polls") {
        if let Err(err) = chatd::adapters::telegram::prove_telegram_polls() {
            eprintln!("telegram-polls: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-telegram-polls:0");
        return Ok(());
    }
    // The lab's telegram-profile driver. A build without this stays up and the driver fails.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("telegram-profile") {
        if let Err(err) = chatd::adapters::telegram::prove_telegram_profile() {
            eprintln!("telegram-profile: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-telegram-profile:0");
        return Ok(());
    }
    // The lab's slack-connect-org driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("slack-connect-org") {
        if let Err(err) = chatd::adapters::prove_slack_connect_org() {
            eprintln!("slack-connect-org: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-slack-connect-org:0");
        return Ok(());
    }
    // The lab's signal-own-profile driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("signal-own-profile") {
        if let Err(err) = chatd::adapters::signal::prove_signal_own_profile() {
            eprintln!("signal-own-profile: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-signal-own-profile:0");
        return Ok(());
    }
    // The lab's signal-payment-labels driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("signal-payment-labels") {
        if let Err(err) = chatd::adapters::signal::prove_signal_payment_labels() {
            eprintln!("signal-payment-labels: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-signal-payment-labels:0");
        return Ok(());
    }
    // The lab's signal-view-once driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("signal-view-once") {
        if let Err(err) = chatd::adapters::signal::prove_signal_view_once() {
            eprintln!("signal-view-once: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-signal-view-once:0");
        return Ok(());
    }
    // The lab's signal-long-text driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("signal-long-text") {
        if let Err(err) = chatd::adapters::signal::prove_signal_long_text() {
            eprintln!("signal-long-text: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-signal-long-text:0");
        return Ok(());
    }
    // The lab's signal-privacy-local driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("signal-privacy-local") {
        if let Err(err) = chatd::adapters::signal::prove_signal_privacy_local() {
            eprintln!("signal-privacy-local: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-signal-privacy-local:0");
        return Ok(());
    }
    // The lab's signal-poll driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("signal-poll") {
        if let Err(err) = chatd::adapters::signal::prove_signal_poll() {
            eprintln!("signal-poll: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-signal-poll:0");
        return Ok(());
    }
    // The lab's messages-unpaired driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("messages-unpaired") {
        if let Err(err) = chatd::adapters::messages::prove_messages_unpaired() {
            eprintln!("messages-unpaired: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-messages-unpaired:0");
        return Ok(());
    }
    // The lab's messages-delete driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("messages-delete") {
        if let Err(err) = chatd::adapters::messages::prove_messages_delete() {
            eprintln!("messages-delete: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-messages-delete:0");
        return Ok(());
    }
    // The lab's slack-profile-fields driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("slack-profile-fields") {
        if let Err(err) = chatd::adapters::prove_slack_profile_fields() {
            eprintln!("slack-profile-fields: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-slack-profile-fields:0");
        return Ok(());
    }
    // The lab's slack-star-channel driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("slack-star-channel") {
        if let Err(err) = chatd::adapters::prove_slack_channel_star() {
            eprintln!("slack-star-channel: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-slack-star-channel:0");
        return Ok(());
    }
    // The lab's slack-call-cards driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("slack-call-cards") {
        if let Err(err) = chatd::adapters::prove_slack_call_cards() {
            eprintln!("slack-call-cards: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-slack-call-cards:0");
        return Ok(());
    }
    // The lab's slack-rich-text driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("slack-rich-text") {
        if let Err(err) = chatd::adapters::prove_slack_rich_text() {
            eprintln!("slack-rich-text: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-slack-rich-text:0");
        return Ok(());
    }
    // The lab's telegram-delete-placed driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("telegram-delete-placed") {
        if let Err(err) = chatd::adapters::telegram::prove_telegram_delete_placed() {
            eprintln!("telegram-delete-placed: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-telegram-delete-placed:0");
        return Ok(());
    }
    // The lab's telegram-view-once driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("telegram-view-once") {
        if let Err(err) = chatd::adapters::telegram::prove_telegram_view_once() {
            eprintln!("telegram-view-once: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-telegram-view-once:0");
        return Ok(());
    }
    // The lab's telegram-checklist driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("telegram-checklist") {
        if let Err(err) = chatd::adapters::telegram::prove_telegram_checklist() {
            eprintln!("telegram-checklist: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-telegram-checklist:0");
        return Ok(());
    }
    // The lab's signal-text-styles driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("signal-text-styles") {
        if let Err(err) = chatd::adapters::signal::prove_signal_styles() {
            eprintln!("signal-text-styles: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-signal-text-styles:0");
        return Ok(());
    }
    // The lab's signal-reaction-restart driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("signal-reaction-restart") {
        if let Err(err) = chatd::adapters::signal::prove_signal_reaction_restart() {
            eprintln!("signal-reaction-restart: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-signal-reaction-restart:0");
        return Ok(());
    }
    // The lab's signal-username driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("signal-username") {
        if let Err(err) = chatd::adapters::signal::prove_signal_username() {
            eprintln!("signal-username: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-signal-username:0");
        return Ok(());
    }
    // The lab's signal-delete-for-me driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("signal-delete-for-me") {
        if let Err(err) = chatd::adapters::signal::prove_signal_delete_for_me() {
            eprintln!("signal-delete-for-me: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-signal-delete-for-me:0");
        return Ok(());
    }
    // The lab's signal-group-rename driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("signal-group-rename") {
        if let Err(err) = chatd::adapters::signal::prove_signal_group_rename() {
            eprintln!("signal-group-rename: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-signal-group-rename:0");
        return Ok(());
    }
    // The lab's signal-send-file driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("signal-send-file") {
        if let Err(err) = chatd::adapters::signal::prove_signal_send_file() {
            eprintln!("prove-signal-send-file: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-signal-send-file:0");
        return Ok(());
    }
    let _log = protocol::log_policy::install("chatd", protocol::log_policy::LogPolicy::from_env());
    protocol::crash::install_panic_hook("chatd");
    let dir = protocol::support_dir();
    let demo = chatd::demo::requested(std::env::args());
    let seed_dir = dir.clone();
    let core = spawn_core(dir, Host::Process, move |store| {
        if let Some(mode) = demo {
            if std::env::var_os("TINYCLIENT_DIR").is_none() {
                tracing::warn!("chatd: --seed-demo needs TINYCLIENT_DIR so it cannot write to the real store");
                std::process::exit(64);
            }
            chatd::demo::seed(store, &seed_dir, mode).map_err(io::Error::other)?;
        }
        Ok(())
    });
    let core = match core {
        Ok(core) => core,
        // A shell starts chatd whenever it finds none answering, so two may start at once: the
        // second finds the first one's lock and leaves it be. The operator file gets the path.
        // Stderr always gets the short line: this process is about to exit, and the launcher
        // reads stderr even when it is not a terminal.
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
            tracing::warn!("chatd: {err}");
            let _ = writeln!(io::stderr(), "chatd: another chatd is already running");
            return Ok(());
        }
        Err(err) => return Err(err),
    };
    // The core serves until the process exits: a `Shutdown` exits it.
    core.join().map_err(|_| io::Error::other("chatd stopped"))
}
