//! Tiny file sends: chips stay until a scanning server has a verdict, and archives stay off
//! until that server allows them. Other networks, and a Tiny server that does not advertise
//! `file_scan`, keep today's upload. Plain values, so the rules are tested without a window.

use protocol::{Capabilities, Network};

/// A Tiny server that scans files keeps the chips until the upload and the message have both
/// settled. An older server, and every other network, clears them when the pending row is stored.
pub fn hold_chips(network: Network, files: bool, can: &Capabilities) -> bool {
    files && network == Network::Tiny && can.file_scan
}

/// Words left on a chip a Tiny upload refused.
pub fn blocked_reason(detail: &str) -> Option<String> {
    let detail = detail.trim();
    (!detail.is_empty()).then(|| detail.to_owned())
}

/// Why an archive cannot be staged. `None` when it may go, including on a server that does not scan.
pub fn archive_reason(network: Network, name: &str, can: &Capabilities) -> Option<String> {
    if network == Network::Tiny && can.file_scan && !can.allow_archives && is_archive(name) {
        Some("This server doesn't take archives.".to_owned())
    } else {
        None
    }
}

fn is_archive(name: &str) -> bool {
    let extension = name.rsplit_once('.').map(|(_, extension)| extension.to_ascii_lowercase()).unwrap_or_default();
    matches!(extension.as_str(), "zip" | "7z" | "rar" | "tar" | "gz" | "tgz" | "bz2")
}

/// Shown under the card while a scanning server still has the file.
pub fn scanning_words(count: usize) -> String {
    if count <= 1 { "Scanning that file before it sends.".to_owned() } else { "Scanning those files before they send.".to_owned() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scanning(allow_archives: bool) -> Capabilities {
        Capabilities { file_scan: true, allow_archives, max_file_bytes: 8 << 20, max_files: 10, ..Capabilities::default() }
    }

    #[test]
    fn a_blocked_tiny_file_keeps_its_chip_and_names_the_reason() {
        let scanning = scanning(false);
        let older = Capabilities { max_file_bytes: 8 << 20, max_files: 10, ..Capabilities::default() };
        assert!(hold_chips(Network::Tiny, true, &scanning), "a scanning server holds the chip until the upload settles");
        assert!(!hold_chips(Network::Tiny, true, &older), "an older server still clears the tray when the pending row is stored");
        assert!(!hold_chips(Network::Discord, true, &scanning));
        assert!(!hold_chips(Network::Telegram, true, &scanning));
        assert!(!hold_chips(Network::Slack, true, &scanning));
        assert!(!hold_chips(Network::Teams, true, &scanning));
        assert!(!hold_chips(Network::Tiny, false, &scanning));
        assert_eq!(blocked_reason("Tiny blocked that file").as_deref(), Some("Tiny blocked that file"));
        assert_eq!(blocked_reason("  Tiny couldn't scan that file  ").as_deref(), Some("Tiny couldn't scan that file"));
        assert!(blocked_reason("  ").is_none());
        assert_eq!(scanning_words(1), "Scanning that file before it sends.");
        assert_eq!(scanning_words(2), "Scanning those files before they send.");
    }

    #[test]
    fn archives_are_refused_on_the_chip_until_the_server_allows_them() {
        let off = scanning(false);
        let on = scanning(true);
        let older = Capabilities { max_file_bytes: 8 << 20, ..Capabilities::default() };
        assert_eq!(archive_reason(Network::Tiny, "mods.zip", &off).as_deref(), Some("This server doesn't take archives."));
        assert!(archive_reason(Network::Tiny, "mods.zip", &on).is_none());
        assert!(archive_reason(Network::Tiny, "notes.pdf", &off).is_none());
        assert!(archive_reason(Network::Tiny, "mods.zip", &older).is_none(), "an older server keeps today's upload");
        assert!(archive_reason(Network::Discord, "mods.zip", &off).is_none());
        for extension in ["7z", "rar", "tar", "gz", "tgz", "bz2"] {
            assert!(archive_reason(Network::Tiny, &format!("pack.{extension}"), &off).is_some(), "{extension}");
        }
        assert!(archive_reason(Network::Tiny, "photo.png", &off).is_none());
    }
}
