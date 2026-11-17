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
    // The lab's linkify driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("linkify") {
        if let Err(err) = chatd::adapters::tiny::prove_linkify() {
            eprintln!("linkify: {err}");
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
    // The lab's discord-nitro-length driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("discord-nitro-length") {
        if let Err(err) = chatd::adapters::prove_discord_nitro_length() {
            eprintln!("discord-nitro-length: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-discord-nitro-length:0");
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
    // The lab's discord-forum-landing driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("discord-forum-landing") {
        if let Err(err) = chatd::demo::prove_discord_forum_landing() {
            eprintln!("discord-forum-landing: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-discord-forum-landing:0");
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
    // The lab's telegram-member-sections driver. A build without this stays up and the driver fails.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("telegram-member-sections") {
        if let Err(err) = chatd::adapters::telegram::prove_telegram_member_sections() {
            eprintln!("telegram-member-sections: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-telegram-member-sections:0");
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
    // The lab's slack-connect-org driver. A build without this returns to the server below and stays up.
    if std::env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("slack-connect-org") {
        if let Err(err) = chatd::adapters::prove_slack_connect_org() {
            eprintln!("slack-connect-org: {err}");
            std::process::exit(1);
        }
        eprintln!("prove-slack-connect-org:0");
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
