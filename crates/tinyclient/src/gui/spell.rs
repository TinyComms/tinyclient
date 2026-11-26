//! Marks for words the composer should underline.
//!
//! The policy is pure: a test hands in a checker, and the system checker is asked only when
//! one is present. A missing checker marks nothing. Words go to a spelling service on this
//! machine, never to a network.

use std::ops::Range;
use std::sync::{Mutex, Once, OnceLock};

/// A word the checker rejected, in bytes of the whole field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Miss {
    pub range: Range<usize>,
    pub suggestions: Vec<String>,
}

/// The underline a misspelling uses. Composition keeps a straight line.
pub(crate) fn wavy() -> bool {
    true
}

/// What a right-click on a marked word offers: the suggestions, then the two ways to keep it.
pub(crate) fn menu_entries(suggestions: &[String]) -> Vec<String> {
    let mut entries: Vec<String> = suggestions.iter().take(5).cloned().collect();
    entries.push("Add to dictionary".to_owned());
    entries.push("Ignore".to_owned());
    entries
}

/// Marks when `on` is set. Off marks nothing, whatever the checker would say.
fn consider(
    on: bool,
    text: &str,
    caret: usize,
    extra: &[Range<usize>],
    ask: impl FnMut(&str) -> Option<Vec<String>>,
) -> Vec<Miss> {
    if !on {
        Vec::new()
    } else {
        marks(text, caret, extra, ask)
    }
}

/// Misspellings in the paragraph the caret is in. A word still being typed is left alone,
/// and so is anything in code, a mention, a link, or an emoji token.
fn marks(
    text: &str,
    caret: usize,
    extra: &[Range<usize>],
    mut ask: impl FnMut(&str) -> Option<Vec<String>>,
) -> Vec<Miss> {
    let caret = boundary(text, caret);
    let para = paragraph(text, caret);
    let mut skip = protected(text);
    skip.extend(extra.iter().cloned());
    let mut found = Vec::new();
    for range in word_ranges(text) {
        if range.end <= para.start || range.start >= para.end {
            continue;
        }
        if still_typing(&range, caret, &para) || overlaps(&range, &skip) {
            continue;
        }
        let word = bare(&text[range.clone()]);
        if !word.chars().any(char::is_alphabetic) {
            continue;
        }
        if let Some(suggestions) = ask(word) {
            found.push(Miss { range, suggestions });
        }
    }
    found
}

/// The field's check, off the UI thread. Nothing is marked when the setting is off or this
/// machine has no checker.
pub(crate) fn check(text: &str, caret: usize, extra: &[Range<usize>]) -> Vec<Miss> {
    load_allowed();
    if !super::app::prefs::choices().spell {
        return Vec::new();
    }
    let mut session = Session::open();
    session.prepare(&text[paragraph(text, boundary(text, caret))]);
    marks(text, caret, extra, |word| {
        if allowed(word) {
            None
        } else {
            session.suggest(word)
        }
    })
}

/// Remember `word` so it is not marked again, and tell the system checker when it has one.
pub(crate) fn allow(word: &str, learn: bool) {
    let word = word.trim();
    if word.is_empty() || word.chars().count() > 64 {
        return;
    }
    extra()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(word.to_lowercase());
    save_allowed();
    teach(word, learn);
}

/// The lab's spell-check driver. An injected checker marks one "teh", skips one inside code,
/// and marking stops when the setting is off.
pub(crate) fn prove() -> Result<(), &'static str> {
    fn toy(word: &str) -> Option<Vec<String>> {
        (word == "teh").then(|| vec!["the".to_owned()])
    }
    let text = "teh quick `teh`";
    let found = consider(true, text, text.len(), &[], toy);
    if found.len() != 1 || found[0].range != (0..3) || found[0].suggestions != ["the"] {
        return Err("teh was not the only mark, or the suggestion was not the");
    }
    if !consider(false, text, text.len(), &[], toy).is_empty() {
        return Err("words are still marked when spell checking is off");
    }
    if !wavy() {
        return Err("a misspelling is not a wavy underline");
    }
    if menu_entries(&["the".to_owned()]) != ["the", "Add to dictionary", "Ignore"] {
        return Err("a marked word does not offer a suggestion, Add to dictionary, and Ignore");
    }
    if !super::app::prefs::Choices::default().spell {
        return Err("spell checking is off by default");
    }
    if super::settings::rows::not_yet("spell").is_some() {
        return Err("search still says there is no spell checking");
    }
    if !super::settings::rows::search("check spelling")
        .iter()
        .any(|row| row.id == "M6")
    {
        return Err("Check spelling is not in Messages");
    }
    Ok(())
}

fn boundary(text: &str, mut caret: usize) -> usize {
    if caret > text.len() {
        caret = text.len();
    }
    while caret > 0 && !text.is_char_boundary(caret) {
        caret -= 1;
    }
    caret
}

/// The blank-line paragraph that holds the caret.
fn paragraph(text: &str, caret: usize) -> Range<usize> {
    let start = text[..caret].rfind("\n\n").map(|at| at + 2).unwrap_or(0);
    let end = text[caret..]
        .find("\n\n")
        .map(|at| caret + at)
        .unwrap_or(text.len());
    start..end
}

fn still_typing(word: &Range<usize>, caret: usize, para: &Range<usize>) -> bool {
    (word.start < caret && caret < word.end) || (caret == word.end && word.end == para.end)
}

fn overlaps(range: &Range<usize>, skips: &[Range<usize>]) -> bool {
    skips
        .iter()
        .any(|skip| range.start < skip.end && skip.start < range.end)
}

fn bare(word: &str) -> &str {
    word.trim_matches(['\'', '’'])
}

fn word_char(ch: char) -> bool {
    ch.is_alphabetic() || ch == '\'' || ch == '’'
}

fn word_ranges(text: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut chars = text.char_indices().peekable();
    while let Some((start, ch)) = chars.next() {
        if !ch.is_alphabetic() {
            continue;
        }
        let mut end = start + ch.len_utf8();
        while let Some(&(at, next)) = chars.peek() {
            if !word_char(next) {
                break;
            }
            end = at + next.len_utf8();
            chars.next();
        }
        out.push(start..end);
    }
    out
}

fn protected(text: &str) -> Vec<Range<usize>> {
    let fenced = fences(text);
    let mut skip = fenced.clone();
    skip.extend(inline_code(text, &fenced));
    skip.extend(links(text));
    skip.extend(mentions(text));
    skip.extend(shortcodes(text));
    skip.extend(custom_emoji(text));
    skip
}

fn fences(text: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut open = None;
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        if line
            .trim_end_matches(['\r', '\n'])
            .trim_start()
            .starts_with("```")
        {
            if let Some(start) = open.take() {
                out.push(start..at + line.len());
            } else {
                open = Some(at);
            }
        }
        at += line.len();
    }
    if let Some(start) = open {
        out.push(start..text.len());
    }
    out
}

fn inline_code(text: &str, fences: &[Range<usize>]) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < text.len() {
        if let Some(fence) = fences
            .iter()
            .find(|fence| fence.start <= at && at < fence.end)
        {
            at = fence.end;
            continue;
        }
        if text[at..].starts_with("```") {
            at += 3;
            continue;
        }
        if text[at..].starts_with('`') {
            if let Some(close) = single_backtick(&text[at + 1..]) {
                out.push(at..at + 1 + close + 1);
                at = at + 1 + close + 1;
                continue;
            }
        }
        at += text[at..].chars().next().map_or(1, char::len_utf8);
    }
    out
}

/// The next backtick that is not one of a fence.
fn single_backtick(text: &str) -> Option<usize> {
    let mut at = 0;
    while at < text.len() {
        if text[at..].starts_with("```") {
            at += 3;
            continue;
        }
        if text[at..].starts_with('`') {
            return Some(at);
        }
        at += text[at..].chars().next().map_or(1, char::len_utf8);
    }
    None
}

fn links(text: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < text.len() {
        let rest = &text[at..];
        let https = rest.find("https://");
        let http = rest.find("http://");
        let rel = match (https, http) {
            (Some(a), Some(b)) => a.min(b),
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (None, None) => break,
        };
        let start = at + rel;
        let mut end = start
            + if text[start..].starts_with("https://") {
                8
            } else {
                7
            };
        while end < text.len() {
            let ch = text[end..].chars().next().unwrap_or(' ');
            if ch.is_whitespace() {
                break;
            }
            end += ch.len_utf8();
        }
        out.push(start..end);
        at = end.max(start + 1);
    }
    out
}

fn mentions(text: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < text.len() {
        let rest = &text[at..];
        if rest.starts_with("<@") || rest.starts_with("<#") {
            if let Some(end) = rest.find('>').filter(|end| *end < 80) {
                out.push(at..at + end + 1);
                at += end + 1;
                continue;
            }
        }
        if let Some(name) = rest.strip_prefix('@') {
            let prev = text[..at].chars().next_back();
            if prev.is_none_or(char::is_whitespace) {
                let end_rel = name
                    .char_indices()
                    .find(|(_, ch)| !ch.is_alphanumeric() && *ch != '_' && *ch != '-')
                    .map(|(index, _)| index)
                    .unwrap_or(name.len());
                if end_rel > 0 {
                    let end = at + 1 + end_rel;
                    out.push(at..end);
                    at = end;
                    continue;
                }
            }
        }
        at += rest.chars().next().map_or(1, char::len_utf8);
    }
    out
}

fn shortcodes(text: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b':' {
            let rest = &text[at + 1..];
            let name = rest
                .chars()
                .take_while(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '+' | '-'))
                .count();
            let name_bytes = rest.chars().take(name).map(char::len_utf8).sum::<usize>();
            if name > 0 && rest[name_bytes..].starts_with(':') {
                out.push(at..at + 1 + name_bytes + 1);
                at = at + 1 + name_bytes + 1;
                continue;
            }
        }
        at += 1;
    }
    out
}

fn custom_emoji(text: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(rel) = text[at..].find('<') {
        let start = at + rel;
        let rest = &text[start..];
        let Some(body) = rest.strip_prefix("<a:").or_else(|| rest.strip_prefix("<:")) else {
            at = start + 1;
            continue;
        };
        let Some(end) = body.find('>') else {
            at = start + 1;
            continue;
        };
        let Some((name, id)) = body[..end].split_once(':') else {
            at = start + 1;
            continue;
        };
        let name_ok = !name.is_empty()
            && name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '+' | '-'));
        let id_ok = !id.is_empty() && id.chars().all(|ch| ch.is_ascii_digit());
        if name_ok && id_ok {
            let len = (rest.len() - body.len()) + end + 1;
            out.push(start..start + len);
            at = start + len;
        } else {
            at = start + 1;
        }
    }
    out
}

fn extra() -> &'static Mutex<std::collections::HashSet<String>> {
    static EXTRA: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();
    EXTRA.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

fn allowed(word: &str) -> bool {
    extra()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains(&word.to_lowercase())
}

fn load_allowed() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let Ok(text) = std::fs::read_to_string(protocol::support_dir().join("spell.words")) else {
            return;
        };
        let mut set = extra()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for line in text.lines() {
            let word = line.trim();
            if !word.is_empty() && word.chars().count() <= 64 {
                set.insert(word.to_lowercase());
            }
        }
    });
}

fn save_allowed() {
    let mut words: Vec<String> = extra()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .cloned()
        .collect();
    words.sort();
    let text = words.join("\n");
    let _ = std::fs::write(protocol::support_dir().join("spell.words"), text);
}

struct Session {
    #[cfg(windows)]
    hold: Option<windows_spell::Hold>,
}

impl Session {
    fn open() -> Session {
        Session {
            #[cfg(windows)]
            hold: windows_spell::Hold::open().ok(),
        }
    }

    fn prepare(&mut self, paragraph: &str) {
        #[cfg(target_os = "macos")]
        cocoa::prepare(paragraph);
        #[cfg(not(target_os = "macos"))]
        let _ = paragraph;
    }

    fn suggest(&mut self, word: &str) -> Option<Vec<String>> {
        #[cfg(windows)]
        {
            self.hold.as_ref()?.suggest(word)
        }
        #[cfg(target_os = "linux")]
        {
            enchant::suggest(word)
        }
        #[cfg(target_os = "macos")]
        {
            cocoa::suggest(word)
        }
        #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
        {
            let _ = word;
            None
        }
    }
}

fn teach(word: &str, learn: bool) {
    #[cfg(windows)]
    windows_spell::teach(word, learn);
    #[cfg(target_os = "linux")]
    enchant::teach(word, learn);
    #[cfg(target_os = "macos")]
    cocoa::teach(word, learn);
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    let _ = (word, learn);
}

#[cfg(windows)]
mod windows_spell {
    use std::ffi::c_void;
    use windows_sys::core::GUID;
    use windows_sys::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER,
    };

    const FACTORY_CLASS: GUID = GUID::from_u128(0x7ab36653_1796_484b_bdfa_e74f1db7c1dc);
    const FACTORY_IFACE: GUID = GUID::from_u128(0x8e018a9d_2415_4677_bf08_794ea61f94bb);
    const S_OK: i32 = 0;
    const S_FALSE: i32 = 1;
    /// `COINIT_APARTMENTTHREADED`.
    const APARTMENT: u32 = 2;

    #[allow(dead_code)]
    #[repr(C)]
    struct FactoryVtbl {
        query: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> i32,
        add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
        release: unsafe extern "system" fn(*mut c_void) -> u32,
        languages: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32,
        is_supported: unsafe extern "system" fn(*mut c_void, *const u16, *mut i32) -> i32,
        create: unsafe extern "system" fn(*mut c_void, *const u16, *mut *mut c_void) -> i32,
    }

    #[allow(dead_code)]
    #[repr(C)]
    struct CheckerVtbl {
        query: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> i32,
        add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
        release: unsafe extern "system" fn(*mut c_void) -> u32,
        language: unsafe extern "system" fn(*mut c_void, *mut *mut u16) -> i32,
        check: unsafe extern "system" fn(*mut c_void, *const u16, *mut *mut c_void) -> i32,
        suggest: unsafe extern "system" fn(*mut c_void, *const u16, *mut *mut c_void) -> i32,
        add: unsafe extern "system" fn(*mut c_void, *const u16) -> i32,
        ignore: unsafe extern "system" fn(*mut c_void, *const u16) -> i32,
    }

    #[allow(dead_code)]
    #[repr(C)]
    struct EnumVtbl {
        query: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> i32,
        add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
        release: unsafe extern "system" fn(*mut c_void) -> u32,
        next: unsafe extern "system" fn(*mut c_void, u32, *mut *mut u16, *mut u32) -> i32,
    }

    #[allow(dead_code)]
    #[repr(C)]
    struct ErrorEnumVtbl {
        query: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> i32,
        add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
        release: unsafe extern "system" fn(*mut c_void) -> u32,
        next: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetUserDefaultLocaleName(name: *mut u16, chars: i32) -> i32;
    }

    pub(super) struct Hold {
        checker: *mut c_void,
        factory: *mut c_void,
        com: bool,
    }

    impl Hold {
        pub(super) fn open() -> Result<Hold, i32> {
            // SAFETY: a null reserved pointer and the apartment flag. A negative result means this
            // thread was already initialized another way, and must not be uninitialized here.
            let hr = unsafe { CoInitializeEx(std::ptr::null(), APARTMENT) };
            let com = hr >= 0;
            let mut factory = std::ptr::null_mut();
            // SAFETY: the class and interface ids are the spell-checker factory's. The out pointer
            // is written only on success.
            let hr = unsafe {
                CoCreateInstance(
                    &FACTORY_CLASS,
                    std::ptr::null_mut(),
                    CLSCTX_INPROC_SERVER,
                    &FACTORY_IFACE,
                    &mut factory,
                )
            };
            if hr < 0 || factory.is_null() {
                if com {
                    unsafe { CoUninitialize() };
                }
                return Err(hr);
            }
            let tag = language(factory);
            // SAFETY: `factory` is the factory interface just created. `create` is its third method.
            let checker = unsafe {
                let vtbl = *(factory as *const *const FactoryVtbl);
                let mut checker = std::ptr::null_mut();
                let hr = ((*vtbl).create)(factory, tag.as_ptr(), &mut checker);
                if hr < 0 || checker.is_null() {
                    release(factory);
                    if com {
                        CoUninitialize();
                    }
                    return Err(hr);
                }
                checker
            };
            Ok(Hold {
                checker,
                factory,
                com,
            })
        }

        pub(super) fn suggest(&self, word: &str) -> Option<Vec<String>> {
            if self.checker.is_null() || !self.misspelled(word) {
                return None;
            }
            let wide = wide(word);
            // SAFETY: `suggest` is the checker method after Check. S_FALSE means the word is correct.
            let (hr, list) = unsafe {
                let vtbl = *(self.checker as *const *const CheckerVtbl);
                let mut list = std::ptr::null_mut();
                let hr = ((*vtbl).suggest)(self.checker, wide.as_ptr(), &mut list);
                (hr, list)
            };
            if hr == S_FALSE {
                release(list);
                return None;
            }
            if hr != S_OK {
                release(list);
                return None;
            }
            let words = read_strings(list);
            if words.len() == 1 && words[0].eq_ignore_ascii_case(word) {
                None
            } else {
                Some(words)
            }
        }

        /// `Check` is what says a word is wrong. `Suggest` on this system also returns neighbours for a correct word.
        fn misspelled(&self, word: &str) -> bool {
            let wide = wide(word);
            // SAFETY: `check` returns an error enumerator. No error means the word is fine.
            unsafe {
                let vtbl = *(self.checker as *const *const CheckerVtbl);
                let mut list = std::ptr::null_mut();
                let hr = ((*vtbl).check)(self.checker, wide.as_ptr(), &mut list);
                if hr < 0 || list.is_null() {
                    release(list);
                    return false;
                }
                let errors = *(list as *const *const ErrorEnumVtbl);
                let mut err = std::ptr::null_mut();
                let hr = ((*errors).next)(list, &mut err);
                let bad = hr == S_OK && !err.is_null();
                release(err);
                release(list);
                bad
            }
        }

        fn teach_one(&self, word: &str, learn: bool) {
            if self.checker.is_null() {
                return;
            }
            let wide = wide(word);
            // SAFETY: Add and Ignore take one wide string. A failure leaves the local list in place.
            unsafe {
                let vtbl = *(self.checker as *const *const CheckerVtbl);
                if learn {
                    ((*vtbl).add)(self.checker, wide.as_ptr());
                } else {
                    ((*vtbl).ignore)(self.checker, wide.as_ptr());
                }
            }
        }
    }

    impl Drop for Hold {
        fn drop(&mut self) {
            release(self.checker);
            release(self.factory);
            if self.com {
                unsafe { CoUninitialize() };
            }
        }
    }

    pub(super) fn teach(word: &str, learn: bool) {
        if let Ok(hold) = Hold::open() {
            hold.teach_one(word, learn);
        }
    }

    #[cfg(test)]
    pub(super) fn probe(word: &str) -> Result<Option<Vec<String>>, i32> {
        Ok(Hold::open()?.suggest(word))
    }

    fn language(factory: *mut c_void) -> Vec<u16> {
        let user = locale();
        // SAFETY: `is_supported` writes one BOOL. A failure falls through to the user's tag.
        let supported = unsafe {
            let vtbl = *(factory as *const *const FactoryVtbl);
            let mut yes = 0i32;
            let hr = ((*vtbl).is_supported)(factory, user.as_ptr(), &mut yes);
            hr >= 0 && yes != 0
        };
        if supported {
            return user;
        }
        // SAFETY: `languages` returns an `IEnumString`. The first tag is the one we ask for.
        let first = unsafe {
            let vtbl = *(factory as *const *const FactoryVtbl);
            let mut list = std::ptr::null_mut();
            let hr = ((*vtbl).languages)(factory, &mut list);
            if hr < 0 {
                None
            } else {
                read_strings(list).into_iter().next()
            }
        };
        first.map(|tag| wide(&tag)).unwrap_or(user)
    }

    fn locale() -> Vec<u16> {
        let mut buf = [0u16; 85];
        // SAFETY: the buffer holds 85 code units, the documented maximum for a locale name.
        let wrote = unsafe { GetUserDefaultLocaleName(buf.as_mut_ptr(), buf.len() as i32) };
        if wrote > 1 {
            buf[..wrote as usize].to_vec()
        } else {
            wide("en-US")
        }
    }

    /// Whether the user's locale is English. The checker is built for that locale, and only an
    /// English dictionary knows that "teh" is wrong and "the" is a word.
    #[cfg(test)]
    pub(super) fn english_locale() -> bool {
        let name = String::from_utf16_lossy(&locale());
        name.trim_end_matches('\0').split(['-', '_']).next().is_some_and(|language| language.eq_ignore_ascii_case("en"))
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn from_wide(ptr: *const u16) -> String {
        if ptr.is_null() {
            return String::new();
        }
        let mut len = 0usize;
        // SAFETY: an OLE string is null terminated. The cap stops a missing terminator.
        unsafe {
            while len < 256 && *ptr.add(len) != 0 {
                len += 1;
            }
            String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len))
        }
    }

    fn read_strings(list: *mut c_void) -> Vec<String> {
        if list.is_null() {
            return Vec::new();
        }
        let mut out = Vec::new();
        // SAFETY: `next` is `IEnumString::Next`. Each string is freed with `CoTaskMemFree`.
        unsafe {
            let vtbl = *(list as *const *const EnumVtbl);
            for _ in 0..8 {
                let mut ptr = std::ptr::null_mut();
                let mut fetched = 0u32;
                let hr = ((*vtbl).next)(list, 1, &mut ptr, &mut fetched);
                if hr != S_OK || fetched == 0 || ptr.is_null() {
                    if !ptr.is_null() {
                        CoTaskMemFree(ptr as *const c_void);
                    }
                    break;
                }
                out.push(from_wide(ptr));
                CoTaskMemFree(ptr as *const c_void);
            }
        }
        release(list);
        out
    }

    fn release(obj: *mut c_void) {
        if obj.is_null() {
            return;
        }
        // SAFETY: the first slot of every one of these objects is `IUnknown`.
        unsafe {
            let vtbl = *(obj as *const *const windows_sys::core::IUnknown_Vtbl);
            ((*vtbl).Release)(obj);
        }
    }
}

#[cfg(target_os = "linux")]
mod enchant {
    use std::ffi::{c_char, c_void, CStr, CString};
    use std::sync::Mutex;

    type Init = unsafe extern "C" fn() -> *mut c_void;
    type Request = unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void;
    type Check = unsafe extern "C" fn(*mut c_void, *const c_char, isize) -> i32;
    type Suggest =
        unsafe extern "C" fn(*mut c_void, *const c_char, isize, *mut usize) -> *mut *mut c_char;
    type FreeList = unsafe extern "C" fn(*mut c_void, *mut *mut c_char);
    type Add = unsafe extern "C" fn(*mut c_void, *const c_char, isize);

    struct Api {
        broker: *mut c_void,
        dict: *mut c_void,
        check: Check,
        suggest: Suggest,
        free_list: FreeList,
        add: Add,
        add_session: Add,
    }

    // SAFETY: the pointers are used only while `API` is held, and the library stays loaded.
    unsafe impl Send for Api {}

    static API: Mutex<Option<Api>> = Mutex::new(None);
    static TRIED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    fn load() -> bool {
        let mut slot = API
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if slot.is_some() {
            return true;
        }
        if TRIED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            return false;
        }
        let Some(api) = open() else {
            return false;
        };
        *slot = Some(api);
        true
    }

    fn open() -> Option<Api> {
        let handle = ["libenchant-2.so.2", "libenchant-2.so"]
            .iter()
            .find_map(|name| {
                let c = CString::new(*name).ok()?;
                // SAFETY: a null-terminated soname. A null handle means the library is absent.
                let handle = unsafe { libc::dlopen(c.as_ptr(), libc::RTLD_LAZY) };
                (!handle.is_null()).then_some(handle)
            })?;
        let symbol = |name: &CStr| {
            // SAFETY: `name` is null terminated. A null result means that symbol is absent.
            let ptr = unsafe { libc::dlsym(handle, name.as_ptr()) };
            (!ptr.is_null()).then_some(ptr)
        };
        let (
            Some(init_ptr),
            Some(request_ptr),
            Some(check_ptr),
            Some(suggest_ptr),
            Some(free_ptr),
            Some(add_ptr),
            Some(session_ptr),
        ) = (
            symbol(c"enchant_broker_init"),
            symbol(c"enchant_broker_request_dict"),
            symbol(c"enchant_dict_check"),
            symbol(c"enchant_dict_suggest"),
            symbol(c"enchant_dict_free_string_list"),
            symbol(c"enchant_dict_add"),
            symbol(c"enchant_dict_add_to_session"),
        )
        else {
            return None;
        };
        // SAFETY: each symbol is non-null and matches enchant's public header.
        let init: Init = unsafe { std::mem::transmute(init_ptr) };
        let request: Request = unsafe { std::mem::transmute(request_ptr) };
        let check: Check = unsafe { std::mem::transmute(check_ptr) };
        let suggest: Suggest = unsafe { std::mem::transmute(suggest_ptr) };
        let free_list: FreeList = unsafe { std::mem::transmute(free_ptr) };
        let add: Add = unsafe { std::mem::transmute(add_ptr) };
        let add_session: Add = unsafe { std::mem::transmute(session_ptr) };
        // SAFETY: `init` is `enchant_broker_init`. A null broker means the library did not start.
        let broker = unsafe { init() };
        if broker.is_null() {
            return None;
        }
        let tag = dict_tag();
        let dict = unsafe { request(broker, tag.as_ptr()) };
        let dict = if dict.is_null() {
            unsafe { request(broker, c"en_US".as_ptr()) }
        } else {
            dict
        };
        if dict.is_null() {
            return None;
        }
        Some(Api {
            broker,
            dict,
            check,
            suggest,
            free_list,
            add,
            add_session,
        })
    }

    fn dict_tag() -> CString {
        let lang = std::env::var("LANG").unwrap_or_default();
        let tag = lang.split('.').next().unwrap_or("en_US");
        let tag = if tag.is_empty() || tag == "C" {
            "en_US"
        } else {
            tag
        };
        CString::new(tag).unwrap_or_else(|_| c"en_US".to_owned())
    }

    pub(super) fn suggest(word: &str) -> Option<Vec<String>> {
        if !load() {
            return None;
        }
        let Ok(c_word) = CString::new(word) else {
            return None;
        };
        let slot = API
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let api = slot.as_ref()?;
        // SAFETY: `check` is `enchant_dict_check`. Zero is spelled correctly, negative is an error.
        let status = unsafe { (api.check)(api.dict, c_word.as_ptr(), word.len() as isize) };
        if status <= 0 {
            return None;
        }
        let mut count = 0usize;
        // SAFETY: `suggest` returns a list freed with `enchant_dict_free_string_list`.
        let list =
            unsafe { (api.suggest)(api.dict, c_word.as_ptr(), word.len() as isize, &mut count) };
        if list.is_null() {
            return Some(Vec::new());
        }
        let mut words = Vec::new();
        unsafe {
            for index in 0..count.min(5) {
                let ptr = *list.add(index);
                if !ptr.is_null() {
                    if let Ok(text) = CStr::from_ptr(ptr).to_str() {
                        words.push(text.to_owned());
                    }
                }
            }
            (api.free_list)(api.dict, list);
        }
        let _ = api.broker;
        Some(words)
    }

    pub(super) fn teach(word: &str, learn: bool) {
        if !load() {
            return;
        }
        let Ok(c_word) = CString::new(word) else {
            return;
        };
        let slot = API
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(api) = slot.as_ref() else {
            return;
        };
        // SAFETY: `add` and `add_session` take the word and its byte length.
        unsafe {
            if learn {
                (api.add)(api.dict, c_word.as_ptr(), word.len() as isize);
            } else {
                (api.add_session)(api.dict, c_word.as_ptr(), word.len() as isize);
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod cocoa {
    use std::ffi::{c_char, c_void, CStr, CString};

    type Id = *mut c_void;

    #[repr(C)]
    struct NSRange {
        location: usize,
        length: usize,
    }

    #[link(name = "objc")]
    extern "C" {
        fn objc_getClass(name: *const c_char) -> Id;
        fn sel_registerName(name: *const c_char) -> Id;
        fn objc_msgSend();
    }

    #[link(name = "AppKit", kind = "framework")]
    extern "C" {}

    fn sel(name: &CStr) -> Id {
        // SAFETY: the name is null terminated. A selector is interned for the process.
        unsafe { sel_registerName(name.as_ptr()) }
    }

    fn string(text: &str) -> Id {
        let Ok(c_text) = CString::new(text) else {
            return std::ptr::null_mut();
        };
        // SAFETY: `alloc` and `initWithUTF8String:` on `NSString`. The caller releases it.
        unsafe {
            let cls = objc_getClass(c"NSString".as_ptr());
            let alloc: extern "C" fn(Id, Id) -> Id =
                std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let init: extern "C" fn(Id, Id, *const c_char) -> Id =
                std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            init(
                alloc(cls, sel(c"alloc")),
                sel(c"initWithUTF8String:"),
                c_text.as_ptr(),
            )
        }
    }

    fn release(obj: Id) {
        if obj.is_null() {
            return;
        }
        // SAFETY: `release` takes no arguments and returns nothing.
        unsafe {
            let send: extern "C" fn(Id, Id) =
                std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            send(obj, sel(c"release"));
        }
    }

    fn checker() -> Id {
        // SAFETY: `+[NSSpellChecker sharedSpellChecker]`.
        unsafe {
            let cls = objc_getClass(c"NSSpellChecker".as_ptr());
            if cls.is_null() {
                return std::ptr::null_mut();
            }
            let send: extern "C" fn(Id, Id) -> Id =
                std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            send(cls, sel(c"sharedSpellChecker"))
        }
    }

    fn pool() -> Id {
        // SAFETY: `[[NSAutoreleasePool alloc] init]`.
        unsafe {
            let cls = objc_getClass(c"NSAutoreleasePool".as_ptr());
            let alloc: extern "C" fn(Id, Id) -> Id =
                std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let init: extern "C" fn(Id, Id) -> Id =
                std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            init(alloc(cls, sel(c"alloc")), sel(c"init"))
        }
    }

    pub(super) fn prepare(paragraph: &str) {
        let kept = pool();
        let checker = checker();
        let text = string(paragraph);
        if !checker.is_null() && !text.is_null() {
            // SAFETY: the setter takes a BOOL. The check lets the checker pick the paragraph's language.
            unsafe {
                let flag: extern "C" fn(Id, Id, i8) =
                    std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
                flag(checker, sel(c"setAutomaticallyIdentifiesLanguages:"), 1);
                let check: extern "C" fn(Id, Id, Id, isize) -> NSRange =
                    std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
                let _ = check(checker, sel(c"checkSpellingOfString:startingAt:"), text, 0);
            }
        }
        release(text);
        release(kept);
    }

    pub(super) fn suggest(word: &str) -> Option<Vec<String>> {
        let kept = pool();
        let checker = checker();
        let text = string(word);
        if checker.is_null() || text.is_null() {
            release(text);
            release(kept);
            return None;
        }
        // SAFETY: `checkSpellingOfString:startingAt:` returns `NSNotFound` when the word is correct.
        let range = unsafe {
            let check: extern "C" fn(Id, Id, Id, isize) -> NSRange =
                std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            check(checker, sel(c"checkSpellingOfString:startingAt:"), text, 0)
        };
        if range.location == usize::MAX || range.length == 0 {
            release(text);
            release(kept);
            return None;
        }
        // SAFETY: `guessesForWordRange:inString:language:inSpellDocumentWithTag:` returns an array.
        let guesses = unsafe {
            let send: extern "C" fn(Id, Id, NSRange, Id, Id, isize) -> Id =
                std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            send(
                checker,
                sel(c"guessesForWordRange:inString:language:inSpellDocumentWithTag:"),
                range,
                text,
                std::ptr::null_mut(),
                0,
            )
        };
        let words = array_strings(guesses);
        release(text);
        release(kept);
        Some(words)
    }

    fn array_strings(array: Id) -> Vec<String> {
        if array.is_null() {
            return Vec::new();
        }
        // SAFETY: `count` and `objectAtIndex:` on `NSArray`, then `UTF8String` on each string.
        unsafe {
            let count_of: extern "C" fn(Id, Id) -> usize =
                std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let at: extern "C" fn(Id, Id, usize) -> Id =
                std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let utf8: extern "C" fn(Id, Id) -> *const c_char =
                std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let count = count_of(array, sel(c"count")).min(5);
            let mut words = Vec::new();
            for index in 0..count {
                let item = at(array, sel(c"objectAtIndex:"), index);
                if item.is_null() {
                    continue;
                }
                let ptr = utf8(item, sel(c"UTF8String"));
                if !ptr.is_null() {
                    if let Ok(text) = CStr::from_ptr(ptr).to_str() {
                        words.push(text.to_owned());
                    }
                }
            }
            words
        }
    }

    pub(super) fn teach(word: &str, learn: bool) {
        let kept = pool();
        let checker = checker();
        let text = string(word);
        if !checker.is_null() && !text.is_null() {
            // SAFETY: `learnWord:` and `ignoreWord:inSpellDocumentWithTag:` take the word.
            unsafe {
                if learn {
                    let send: extern "C" fn(Id, Id, Id) =
                        std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
                    send(checker, sel(c"learnWord:"), text);
                } else {
                    let send: extern "C" fn(Id, Id, Id, isize) =
                        std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
                    send(checker, sel(c"ignoreWord:inSpellDocumentWithTag:"), text, 0);
                }
            }
        }
        release(text);
        release(kept);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toy(word: &str) -> Option<Vec<String>> {
        (word == "teh").then(|| vec!["the".to_owned()])
    }

    #[test]
    fn a_closed_teh_is_marked_and_a_code_span_is_not() {
        let text = "teh quick `teh`";
        let found = marks(text, text.len(), &[], toy);
        assert_eq!(found.len(), 1, "{text} -> {found:?}");
        assert_eq!(found[0].range, 0..3);
        assert_eq!(found[0].suggestions, ["the"]);
    }

    #[test]
    fn a_word_still_being_typed_is_not_marked() {
        assert!(marks("teh", 3, &[], toy).is_empty());
    }

    #[test]
    fn another_paragraph_is_left_alone() {
        let text = "teh\n\nquick";
        assert!(marks(text, text.len(), &[], toy).is_empty());
    }

    #[test]
    fn a_mention_a_link_a_shortcode_and_a_fence_are_left_alone() {
        let text = "@teh https://teh.example/x :teh: <:teh:1> <a:teh:2>\n```\nteh\n```\nteh ";
        let found = marks(text, text.len(), &[], toy);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].range.start, text.rfind("teh").unwrap());
    }

    #[test]
    fn an_extra_range_is_left_alone() {
        let text = "teh quick";
        let skip = 0..3;
        assert!(marks(text, text.len(), &[skip], toy).is_empty());
    }

    #[test]
    fn checking_off_marks_nothing() {
        let text = "teh quick";
        assert!(consider(false, text, text.len(), &[], toy).is_empty());
        assert_eq!(consider(true, text, text.len(), &[], toy).len(), 1);
    }

    #[test]
    fn a_marked_word_offers_the_suggestion_and_the_two_ways_to_keep_it() {
        assert_eq!(
            menu_entries(&["the".to_owned()]),
            ["the", "Add to dictionary", "Ignore"]
        );
    }

    #[test]
    fn the_prove_accepts_a_build_that_marks_teh() {
        assert_eq!(prove(), Ok(()));
    }

    #[cfg(windows)]
    #[test]
    fn the_system_checker_answers_teh() {
        // The checker is built for the user's locale, and only an English one knows that "the"
        // is a word. On any other locale this test says nothing, and says why.
        if !windows_spell::english_locale() {
            eprintln!("skipping: the spell checker is built for the user's locale, which is not English");
            return;
        }
        let words = windows_spell::probe("teh").expect("the spell checker did not open");
        let correct = windows_spell::probe("the").expect("the spell checker did not open");
        assert!(correct.is_none(), "a correct word was marked: {correct:?}");
        let words = words.expect("teh was not marked");
        assert!(
            words.iter().any(|word| word.eq_ignore_ascii_case("the")),
            "{words:?}"
        );
    }
}
