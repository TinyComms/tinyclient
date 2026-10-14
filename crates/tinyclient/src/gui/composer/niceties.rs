//! Two small reads of the draft: who a broadcast mention would notify, and which links the
//! network would unfurl. Plain functions over text.

use protocol::Network;

/// More than this many people notified asks for a second Return.
pub const LARGE_PING: u32 = 25;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Broadcast {
    /// `@everyone`, `@channel`: every member.
    All,
    /// `@here`: those online now.
    Here,
}

/// The widest broadcast mention in the text, where the network has one.
pub fn broadcast(text: &str, network: Network) -> Option<Broadcast> {
    let words: &[(&str, Broadcast)] = match network {
        Network::Discord => &[("@everyone", Broadcast::All), ("@here", Broadcast::Here)],
        Network::Slack => &[("@channel", Broadcast::All), ("@everyone", Broadcast::All), ("@here", Broadcast::Here)],
        Network::Telegram | Network::Teams | Network::Signal => &[],
    };
    let found = |word: &str| {
        text.match_indices(word).any(|(at, _)| {
            let before = text[..at].chars().next_back();
            let after = text[at + word.len()..].chars().next();
            before.is_none_or(|ch| !ch.is_alphanumeric() && ch != '`') && after.is_none_or(|ch| !ch.is_alphanumeric() && ch != '_')
        })
    };
    words.iter().filter(|(word, _)| found(word)).map(|(_, kind)| *kind).min_by_key(|kind| *kind != Broadcast::All)
}

/// How many a broadcast reaches, from the conversation's counts. `None` when they are unknown.
pub fn reach(kind: Broadcast, members: Option<u32>, online: Option<u32>) -> Option<u32> {
    match kind {
        Broadcast::All => members,
        Broadcast::Here => online.or(members),
    }
}

/// The count that needs the confirm, if it does.
pub fn needs_confirm(text: &str, network: Network, members: Option<u32>, online: Option<u32>) -> Option<u32> {
    broadcast(text, network).and_then(|kind| reach(kind, members, online)).filter(|count| *count > LARGE_PING)
}

/// What the large-ping confirm says, or `None` when nothing needs it. `count` is a broadcast's
/// reach already over [`LARGE_PING`]; `roles` are the role and user-group mentions (`@admins`),
/// whose member counts the network does not give, so each is "everyone in" it, without a number.
pub fn confirm_words(count: Option<u32>, place: &str, roles: &[String]) -> Option<String> {
    let groups = match roles {
        [] => None,
        [one] => Some(format!("everyone in {one}")),
        [rest @ .., last] => Some(format!("everyone in {} and {last}", rest.join(", "))),
    };
    let whom = match (count, groups) {
        (None, None) => return None,
        (Some(count), None) => format!("{} people in {place}", grouped(count)),
        (None, Some(groups)) => groups,
        (Some(count), Some(groups)) => format!("{} people in {place} and {groups}", grouped(count)),
    };
    Some(format!("This notifies {whom}. Return again to send."))
}

/// "1,204"
pub fn grouped(count: u32) -> String {
    let digits = count.to_string();
    let mut out = String::new();
    for (ix, ch) in digits.chars().enumerate() {
        if ix > 0 && (digits.len() - ix).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// The hosts of the links in the text, in order, each once.
pub fn link_hosts(text: &str) -> Vec<String> {
    let mut hosts: Vec<String> = Vec::new();
    for (at, _) in text.match_indices("http") {
        let rest = &text[at..];
        let Some(after) = rest.strip_prefix("https://").or_else(|| rest.strip_prefix("http://")) else {
            continue;
        };
        // A link starts a word; "xhttp://" is not one.
        if text[..at].chars().next_back().is_some_and(|ch| ch.is_alphanumeric()) {
            continue;
        }
        let host: String = after.chars().take_while(|ch| !matches!(ch, '/' | '?' | '#' | ':' | ' ' | '\n' | '\t' | ')' | '>' | '"')).collect();
        let host = host.trim_start_matches("www.").trim_end_matches('.').to_lowercase();
        if host.contains('.') && !hosts.contains(&host) {
            hosts.push(host);
        }
    }
    hosts
}

/// The chip's words: "Preview: example.com", or "Previews: 2 links".
pub fn preview_words(hosts: &[String], suppressed: bool) -> Option<String> {
    match (hosts, suppressed) {
        ([], _) => None,
        ([one], false) => Some(format!("Preview: {one}")),
        ([one], true) => Some(format!("No preview: {one}")),
        (many, false) => Some(format!("Previews: {} links", many.len())),
        (many, true) => Some(format!("No previews: {} links", many.len())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_broadcast_is_a_whole_word_the_network_knows() {
        assert_eq!(broadcast("@everyone the creek route is on", Network::Discord), Some(Broadcast::All));
        assert_eq!(broadcast("ping @here please", Network::Discord), Some(Broadcast::Here));
        assert_eq!(broadcast("@here and @everyone", Network::Discord), Some(Broadcast::All));
        assert_eq!(broadcast("@channel", Network::Slack), Some(Broadcast::All));
        assert_eq!(broadcast("@channel", Network::Discord), None);
        assert_eq!(broadcast("mail me@everyone.com", Network::Discord), None);
        assert_eq!(broadcast("@heresy", Network::Discord), None);
        assert_eq!(broadcast("`@everyone`", Network::Discord), None);
        assert_eq!(broadcast("@everyone", Network::Telegram), None);
    }

    #[test]
    fn more_than_twenty_five_people_asks_first() {
        assert_eq!(needs_confirm("@everyone hi", Network::Discord, Some(1_204), Some(80)), Some(1_204));
        assert_eq!(needs_confirm("@here hi", Network::Discord, Some(1_204), Some(80)), Some(80));
        assert_eq!(needs_confirm("@here hi", Network::Discord, Some(1_204), None), Some(1_204));
        assert_eq!(needs_confirm("@everyone hi", Network::Discord, Some(25), None), None);
        assert_eq!(needs_confirm("@everyone hi", Network::Discord, None, None), None);
        assert_eq!(needs_confirm("hi all", Network::Discord, Some(5_000), None), None);
        assert_eq!(grouped(1_204), "1,204");
        assert_eq!(grouped(26), "26");
        assert_eq!(grouped(1_234_567), "1,234,567");
    }

    #[test]
    fn a_role_or_group_mention_asks_first_and_says_everyone_in_it_without_a_number() {
        let roles = |names: &[&str]| names.iter().map(|name| (*name).to_owned()).collect::<Vec<_>>();
        assert_eq!(confirm_words(None, "#general", &[]), None);
        assert_eq!(confirm_words(Some(1_204), "#general", &[]).as_deref(), Some("This notifies 1,204 people in #general. Return again to send."));
        assert_eq!(confirm_words(None, "#general", &roles(&["@admins"])).as_deref(), Some("This notifies everyone in @admins. Return again to send."));
        assert_eq!(
            confirm_words(Some(30), "#ops", &roles(&["@admins", "@oncall", "@leads"])).as_deref(),
            Some("This notifies 30 people in #ops and everyone in @admins, @oncall and @leads. Return again to send.")
        );
    }

    #[test]
    fn links_are_found_by_host_once_each() {
        let text = "Route notes: https://www.trailnotes.example/creek?x=1 and http://maps.example:8080/a, again https://trailnotes.example/b";
        assert_eq!(link_hosts(text), ["trailnotes.example", "maps.example"]);
        assert!(link_hosts("no links, just http and https:// alone").is_empty());
        assert!(link_hosts("xhttps://a.example").is_empty());
        let one = vec!["example.com".to_owned()];
        assert_eq!(preview_words(&one, false).as_deref(), Some("Preview: example.com"));
        assert_eq!(preview_words(&one, true).as_deref(), Some("No preview: example.com"));
        assert_eq!(preview_words(&["a.b".into(), "c.d".into()], false).as_deref(), Some("Previews: 2 links"));
        assert_eq!(preview_words(&[], false), None);
    }
}
