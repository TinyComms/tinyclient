//! Quick switcher matching: which names a few typed letters mean, best first.

use std::ops::Range;

/// How well `query` names `title`, and which bytes of the title it matched. `None`: not at all.
/// A name that starts with the letters beats a word inside it that does, which beats the
/// letters anywhere, which beats the letters merely in order.
pub fn matches(query: &str, title: &str) -> Option<(u32, Vec<Range<usize>>)> {
    let query = query.trim().trim_start_matches(['#', '@']).to_lowercase();
    if query.is_empty() {
        return Some((0, Vec::new()));
    }
    // Lowercased per character, keeping each one's place in the title as written.
    let letters: Vec<(usize, char)> =
        title.char_indices().flat_map(|(at, c)| c.to_lowercase().map(move |c| (at, c))).collect();
    let wanted: Vec<char> = query.chars().collect();
    let span = |from: usize| -> Range<usize> {
        let last = letters[from + wanted.len() - 1].0;
        letters[from].0..last + title[last..].chars().next().map_or(0, char::len_utf8)
    };
    let found = (0..letters.len().saturating_sub(wanted.len() - 1))
        .filter(|from| wanted.iter().enumerate().all(|(ix, c)| letters[from + ix].1 == *c))
        .map(|from| {
            let word_start = from == 0 || !letters[from - 1].1.is_alphanumeric();
            let score = match (from, word_start, letters.len() == wanted.len()) {
                (0, _, true) => 1000,
                (0, _, false) => 800,
                (_, true, _) => 600,
                (_, false, _) => 400,
            };
            (score, from)
        })
        .max_by_key(|(score, from)| (*score, std::cmp::Reverse(*from)));
    if let Some((score, from)) = found {
        return Some((score, vec![span(from)]));
    }

    // The letters in order with gaps: `dsg` finds `design`. Fewer gaps is better.
    let mut ranges: Vec<Range<usize>> = Vec::new();
    let mut next = 0;
    for (at, c) in &letters {
        if next < wanted.len() && *c == wanted[next] {
            let end = at + title[*at..].chars().next().map_or(0, char::len_utf8);
            match ranges.last_mut() {
                Some(last) if last.end == *at => last.end = end,
                _ => ranges.push(*at..end),
            }
            next += 1;
        }
    }
    (next == wanted.len()).then(|| (200u32.saturating_sub(10 * ranges.len() as u32), ranges))
}

/// What lifts a match of the same quality: someone waiting there, then how lately it spoke.
pub fn boost(mention: bool, unread: bool, direct: bool, updated: i64, now: i64) -> u32 {
    let age = (now - updated).max(0);
    let recent = match age {
        0..=3_599 => 40,
        3_600..=86_399 => 30,
        86_400..=604_799 => 15,
        _ => 0,
    };
    u32::from(mention) * 60 + u32::from(unread) * 30 + u32::from(direct) * 20 + recent
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order(query: &str, titles: &[&'static str]) -> Vec<&'static str> {
        let mut found: Vec<(u32, &str)> =
            titles.iter().filter_map(|title| Some((matches(query, title)?.0, *title))).collect();
        found.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
        found.into_iter().map(|(_, title)| title).collect()
    }

    #[test]
    fn a_prefix_beats_a_word_start_beats_a_substring_beats_scattered_letters() {
        let titles = ["redesign", "ui-design", "design-crit", "des", "Odessa Lindqvist", "dock-schedules", "general"];
        assert_eq!(
            order("des", &titles),
            ["des", "design-crit", "ui-design", "redesign", "Odessa Lindqvist", "dock-schedules"]
        );
    }

    #[test]
    fn the_matched_bytes_are_those_of_the_title_as_written() {
        assert_eq!(matches("#DES", "#design"), Some((600, vec![1..4])));
        assert_eq!(matches("zoë", "Zoë Å"), Some((800, vec![0..4])));
        assert_eq!(matches("dsg", "design").map(|(_, ranges)| ranges), Some(vec![0..1, 2..3, 4..5]));
        assert_eq!(matches("xyz", "design"), None);
        assert_eq!(matches("  ", "anything"), Some((0, vec![])));
    }

    #[test]
    fn someone_waiting_outranks_recency_which_outranks_nothing() {
        let now = 1_000_000;
        let quiet_old = boost(false, false, false, 0, now);
        let quiet_new = boost(false, false, false, now - 60, now);
        let mention_old = boost(true, true, false, 0, now);
        assert!(mention_old > quiet_new && quiet_new > quiet_old);
        // But never enough to lift a worse match over a better one.
        assert!(matches("des", "redesign").unwrap().0 + boost(true, true, true, now, now) < matches("des", "design").unwrap().0);
    }
}
