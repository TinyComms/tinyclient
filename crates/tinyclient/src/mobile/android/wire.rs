//! What crosses the JNI boundary as plain text and bytes, kept free of JNI so host tests check
//! it (`lib.rs` includes this file under `cfg(test)`): the decoder's picture, a sign-in page's
//! state, a navigation decision, and the words for a refused permission.

use protocol::web_sign_in::{Navigation, PageState};

/// `AvcDecoder.decode`'s answer: width and height (big-endian), then tight I420. The size and
/// the I420 bytes; `None` when empty, short, zero-sized or not whole.
pub fn decoded_picture(out: &[u8]) -> Option<(u32, u32, &[u8])> {
    if out.len() < 8 {
        return None;
    }
    let width = u32::from_be_bytes([out[0], out[1], out[2], out[3]]);
    let height = u32::from_be_bytes([out[4], out[5], out[6], out[7]]);
    let data = &out[8..];
    let pixels = width as usize * height as usize;
    let whole = data.len() == pixels + 2 * ((width as usize / 2) * (height as usize / 2));
    (width > 0 && height > 0 && whole).then_some((width, height, data))
}

/// `WebSession.state`: a closed flag, a load-failed flag ("1"/"0"), then the URL.
pub fn page_state(text: &str) -> PageState {
    let mut chars = text.chars();
    let closed = chars.next() == Some('1');
    let load_failed = chars.next() == Some('1');
    PageState { url: chars.as_str().to_owned(), closed, load_failed }
}

/// A navigation decision as `WebSignInActivity` reads it: "A", "C", or "L" and the URL.
pub fn navigation_word(decision: Navigation) -> String {
    match decision {
        Navigation::Allow => "A".to_owned(),
        Navigation::Cancel => "C".to_owned(),
        Navigation::Load(other) => format!("L{other}"),
    }
}

/// `TinyPlatform.requestPermission`'s answer in words: 1 refused, 2 refused for good (only
/// Settings can allow it now), 3 no window to ask from.
pub fn refusal(microphone: bool, code: i32) -> String {
    let (thing, why) = if microphone { ("microphone", "for calls") } else { ("camera", "to send video") };
    match code {
        2 => format!("Tiny isn't allowed to use the {thing}. Turn it on in Settings to use it {why}."),
        3 => format!("Tiny can't ask for the {thing} right now. Open the app and try again."),
        _ => format!("Tiny needs the {thing} {why}."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_decoded_picture_is_its_header_and_whole_i420() {
        let mut out = vec![0, 0, 0, 4, 0, 0, 0, 2];
        out.extend(std::iter::repeat_n(7u8, 4 * 2 * 3 / 2));
        let (width, height, data) = decoded_picture(&out).expect("a whole picture");
        assert_eq!((width, height, data.len()), (4, 2, 12));
        // Nothing out yet, a cut picture, a zero size: none.
        assert!(decoded_picture(&[]).is_none());
        assert!(decoded_picture(&out[..out.len() - 1]).is_none());
        assert!(decoded_picture(&[0, 0, 0, 0, 0, 0, 0, 0]).is_none());
    }

    #[test]
    fn a_page_state_reads_its_flags_then_the_url() {
        let state = page_state("01https://discord.com/login");
        assert!(!state.closed && state.load_failed);
        assert_eq!(state.url, "https://discord.com/login");
        let closed = page_state("10");
        assert!(closed.closed && !closed.load_failed && closed.url.is_empty());
        assert_eq!(page_state("").url, "");
    }

    #[test]
    fn a_navigation_is_one_letter_and_a_url_to_load() {
        assert_eq!(navigation_word(Navigation::Allow), "A");
        assert_eq!(navigation_word(Navigation::Cancel), "C");
        assert_eq!(navigation_word(Navigation::Load("https://app.slack.com/".into())), "Lhttps://app.slack.com/");
    }

    #[test]
    fn a_refusal_says_what_and_where_to_allow_it() {
        assert!(refusal(true, 1).contains("microphone"));
        assert!(refusal(false, 2).contains("Settings"));
        assert!(refusal(true, 3).contains("Open the app"));
    }
}
