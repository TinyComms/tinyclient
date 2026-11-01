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

/// A file still with the scanner, in the conversation it was sent from.
pub struct OpenScan {
    pub conversation: protocol::Key,
    pub chips: usize,
}

/// What the newest page says about a file that is still scanning. Missing from that page is
/// not a verdict: a busy channel can push the pending row off it before the upload finishes.
pub fn scan_settled(state: Option<&protocol::SendState>) -> Option<Result<(), String>> {
    match state {
        Some(protocol::SendState::Sent) => Some(Ok(())),
        Some(protocol::SendState::Failed { reason }) => Some(Err(reason.clone())),
        Some(protocol::SendState::Pending) | None => None,
    }
}

/// Keep asking after the row leaves the newest page, but only for the scan that is still open.
pub fn stay_in_flight(watching: bool, state: Option<&protocol::SendState>) -> bool {
    watching && scan_settled(state).is_none()
}

/// Send stays available in every conversation except the one whose file is still scanning.
pub fn scan_blocks_send(open: Option<&OpenScan>, here: &protocol::Key) -> bool {
    open.is_some_and(|open| open.conversation == *here)
}

/// The scanning sentence for the conversation on screen. Another conversation shows none,
/// and coming back to the upload shows it again while the hold is open.
pub fn scan_caption(open: Option<&OpenScan>, here: &protocol::Key) -> Option<String> {
    open.filter(|open| open.conversation == *here).map(|open| scanning_words(open.chips))
}

/// One chip hold per in-flight client id. Inserting a second id leaves the first in place.
pub struct Holds<T> {
    rows: Vec<(String, T)>,
}

impl<T> Default for Holds<T> {
    fn default() -> Self {
        Self { rows: Vec::new() }
    }
}

impl<T> Holds<T> {
    pub fn insert(&mut self, client_id: impl Into<String>, value: T) {
        let client_id = client_id.into();
        self.rows.retain(|(id, _)| id != &client_id);
        self.rows.push((client_id, value));
    }

    pub fn remove(&mut self, client_id: &str) -> Option<T> {
        let at = self.rows.iter().position(|(id, _)| id == client_id)?;
        Some(self.rows.remove(at).1)
    }

    pub fn values(&self) -> impl Iterator<Item = &T> {
        self.rows.iter().map(|(_, value)| value)
    }
}

/// Verdicts to record for a Tiny file send. An id that is not in flight yet is included when
/// it is awaiting the send ack, so a Failed update that wins that race is not dropped.
pub fn file_verdicts(
    inflight: &[String],
    awaiting: &[String],
    settled: impl Fn(&str) -> Option<Result<(), String>>,
) -> Vec<(String, Result<(), String>)> {
    let mut seen = Vec::new();
    let mut out = Vec::new();
    for id in inflight.iter().chain(awaiting.iter()) {
        if seen.iter().any(|kept: &String| kept == id) {
            continue;
        }
        seen.push(id.clone());
        if let Some(verdict) = settled(id) {
            out.push((id.clone(), verdict));
        }
    }
    out
}

/// Remember a verdict by client id. A later one for the same id replaces it. The hold reads
/// this once, when it starts, including when the verdict arrived first.
pub fn remember_verdict(queued: &mut Vec<(String, Result<(), String>)>, client_id: String, outcome: Result<(), String>) {
    queued.retain(|(id, _)| id != &client_id);
    queued.push((client_id, outcome));
    if queued.len() > 32 {
        let _ = queued.remove(0).1.ok();
    }
}

pub fn take_verdict(queued: &mut Vec<(String, Result<(), String>)>, client_id: &str) -> Option<Result<(), String>> {
    let at = queued.iter().position(|(id, _)| id == client_id)?;
    Some(queued.remove(at).1)
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

    fn key(id: &str) -> protocol::Key {
        protocol::Key { network: Network::Tiny, account: "me".into(), id: id.into() }
    }

    #[test]
    fn a_row_off_the_newest_page_is_not_a_clean_scan() {
        assert!(scan_settled(None).is_none(), "missing from the newest page is not posted");
        assert!(scan_settled(Some(&protocol::SendState::Pending)).is_none());
        assert!(stay_in_flight(true, None), "the scan stays watched until it actually settles");
        assert!(stay_in_flight(true, Some(&protocol::SendState::Pending)));
        assert!(!stay_in_flight(false, None), "a row that was not a held scan can leave the page");
        assert_eq!(scan_settled(Some(&protocol::SendState::Sent)), Some(Ok(())));
        assert!(!stay_in_flight(true, Some(&protocol::SendState::Sent)));
        let failed = protocol::SendState::Failed { reason: "Tiny blocked that file".into() };
        assert_eq!(scan_settled(Some(&failed)), Some(Err("Tiny blocked that file".into())));
        assert!(!stay_in_flight(true, Some(&failed)));
    }

    #[test]
    fn a_scan_hold_blocks_only_the_conversation_that_is_uploading() {
        let here = key("general");
        let other = key("random");
        let open = OpenScan { conversation: here.clone(), chips: 1 };
        assert!(scan_blocks_send(Some(&open), &here));
        assert!(!scan_blocks_send(Some(&open), &other), "another channel can still send");
        assert!(!scan_blocks_send(None, &here));
        assert_eq!(scan_caption(Some(&open), &here).as_deref(), Some("Scanning that file before it sends."));
        assert!(scan_caption(Some(&open), &other).is_none());
        let two = OpenScan { chips: 2, ..open };
        assert_eq!(scan_caption(Some(&two), &here).as_deref(), Some("Scanning those files before they send."));
    }

    #[test]
    fn a_verdict_before_the_hold_is_applied_when_the_hold_starts() {
        let failed = protocol::SendState::Failed { reason: "Tiny blocked that file".into() };
        let state = |id: &str| (id == "early").then_some(&failed);
        let got = file_verdicts(&[], &["early".into()], |id| scan_settled(state(id)));
        assert_eq!(got, vec![("early".into(), Err("Tiny blocked that file".into()))]);
        let mut queued = Vec::new();
        let mut holds: Holds<&str> = Holds::default();
        remember_verdict(&mut queued, got[0].0.clone(), got[0].1.clone());
        match take_verdict(&mut queued, "early") {
            Some(outcome) => assert_eq!(outcome, Err("Tiny blocked that file".into())),
            None => holds.insert("early", "general"),
        }
        assert!(holds.values().next().is_none(), "Send must not stay on Scanning after an early Failed");
        assert!(take_verdict(&mut queued, "early").is_none());
    }

    #[test]
    fn a_second_channel_does_not_drop_the_first_hold() {
        let mut holds: Holds<&str> = Holds::default();
        holds.insert("one", "general");
        holds.insert("two", "random");
        assert_eq!(holds.remove("two"), Some("random"));
        assert_eq!(holds.remove("one"), Some("general"), "the first conversation's hold is still there");
        assert!(holds.remove("one").is_none());
    }
}
