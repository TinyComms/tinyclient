//! Colours for a fenced block, from its language name. The shell draws these runs. A block
//! longer than [`PLAIN_LINES`] stays as it was typed: highlighting it is not worth the wait.

/// Lines past this stay plain. A few hundred is already more than a screen of code.
pub const PLAIN_LINES: usize = 300;

/// How a range of the returned text is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ink {
    Keyword,
    String,
    Comment,
    Number,
    Type,
    Function,
    /// A diff line that was added. The whole line, not only the `+`.
    Added,
    /// A diff line that was removed. The whole line, not only the `-`.
    Removed,
    /// An ANSI SGR colour. `0` is the default for that side.
    Ansi {
        fg: u8,
        bg: u8,
        bold: bool,
    },
}

/// A coloured range. `start` and `end` are byte offsets into [`Lit::text`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Paint {
    pub start: u32,
    pub end: u32,
    pub ink: Ink,
}

/// The text to draw, and the ranges that are not plain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lit {
    pub text: String,
    pub runs: Vec<Paint>,
}

/// `language` is the name on the fence (`rust`, `diff`, `ansi`). Unknown names stay plain.
pub fn highlight(language: &str, source: &str) -> Lit {
    if source.lines().count() > PLAIN_LINES {
        return plain(source);
    }
    match family(language) {
        Family::Plain => plain(source),
        Family::Ansi => ansi(source),
        Family::Diff => diff(source),
        Family::Html => html(source),
        Family::Code(lang) => code(lang, source),
    }
}

/// The lab's discord-code-colors driver. A rust keyword, a diff's added and removed lines,
/// and an ansi colour with the escapes gone. A build without this stays up and the driver fails.
#[allow(dead_code)]
pub(super) fn prove_code_colors() -> Result<(), String> {
    let rust = highlight("rust", "fn main() {}");
    let keyword = rust
        .runs
        .iter()
        .find(|run| rust.text.get(run.start as usize..run.end as usize) == Some("fn"));
    if keyword.map(|run| run.ink) != Some(Ink::Keyword) {
        return Err("fn was not a keyword".to_owned());
    }
    let diff = highlight("diff", "--- a/f\n+++ b/f\n-old\n+new\n");
    let added = diff.runs.iter().any(|run| {
        run.ink == Ink::Added && diff.text.get(run.start as usize..run.end as usize) == Some("+new")
    });
    let removed = diff.runs.iter().any(|run| {
        run.ink == Ink::Removed
            && diff.text.get(run.start as usize..run.end as usize) == Some("-old")
    });
    let header = diff.runs.iter().any(|run| {
        let slice = diff
            .text
            .get(run.start as usize..run.end as usize)
            .unwrap_or("");
        slice.starts_with("+++") || slice.starts_with("---")
    });
    if !added || !removed || header {
        return Err("diff lines were not coloured, or a file header was".to_owned());
    }
    let ansi = highlight("ansi", "\u{1b}[31mred\u{1b}[0m");
    if ansi.text != "red" {
        return Err(format!(
            "ansi text was {:?}, escapes were not dropped",
            ansi.text
        ));
    }
    let red = ansi.runs.first().is_some_and(|run| {
        run.ink
            == Ink::Ansi {
                fg: 31,
                bg: 0,
                bold: false,
            }
            && run.end - run.start == 3
    });
    if !red {
        return Err("ansi red was not kept".to_owned());
    }
    let long = "let x = 1;\n".repeat(PLAIN_LINES + 1);
    let plain = highlight("rust", &long);
    if !plain.runs.is_empty() || plain.text != long {
        return Err("a long block was coloured".to_owned());
    }
    Ok(())
}

fn plain(source: &str) -> Lit {
    Lit {
        text: source.to_owned(),
        runs: Vec::new(),
    }
}

enum Family {
    Plain,
    Ansi,
    Diff,
    Html,
    Code(&'static str),
}

fn named(language: &str, names: &[&str]) -> bool {
    names
        .iter()
        .any(|name| language.trim().eq_ignore_ascii_case(name))
}

fn family(language: &str) -> Family {
    if named(language, &["ansi"]) {
        Family::Ansi
    } else if named(language, &["diff", "patch"]) {
        Family::Diff
    } else if named(language, &["html", "xml"]) {
        Family::Html
    } else if named(language, &["rust", "rs"]) {
        Family::Code("rust")
    } else if named(language, &["python", "py"]) {
        Family::Code("python")
    } else if named(language, &["js", "javascript", "jsx"]) {
        Family::Code("js")
    } else if named(language, &["ts", "typescript", "tsx"]) {
        Family::Code("ts")
    } else if named(language, &["json"]) {
        Family::Code("json")
    } else if named(language, &["yaml", "yml"]) {
        Family::Code("yaml")
    } else if named(language, &["bash", "sh", "shell", "zsh"]) {
        Family::Code("bash")
    } else if named(language, &["c", "h"]) {
        Family::Code("c")
    } else if named(language, &["cpp", "c++", "cc", "cxx", "hpp"]) {
        Family::Code("cpp")
    } else if named(language, &["go", "golang"]) {
        Family::Code("go")
    } else if named(language, &["sql"]) {
        Family::Code("sql")
    } else if named(language, &["css"]) {
        Family::Code("css")
    } else {
        Family::Plain
    }
}

struct Rules {
    hash: bool,
    directive: bool,
    slash: bool,
    block: bool,
    dashes: bool,
}

fn rules_of(lang: &str) -> Rules {
    match lang {
        "rust" | "go" | "js" | "ts" => Rules {
            hash: false,
            directive: false,
            slash: true,
            block: true,
            dashes: false,
        },
        "c" | "cpp" => Rules {
            hash: false,
            directive: true,
            slash: true,
            block: true,
            dashes: false,
        },
        "python" | "yaml" | "bash" => Rules {
            hash: true,
            directive: false,
            slash: false,
            block: false,
            dashes: false,
        },
        "sql" => Rules {
            hash: false,
            directive: false,
            slash: false,
            block: true,
            dashes: true,
        },
        "css" => Rules {
            hash: false,
            directive: false,
            slash: false,
            block: true,
            dashes: false,
        },
        _ => Rules {
            hash: false,
            directive: false,
            slash: false,
            block: false,
            dashes: false,
        },
    }
}

fn push(runs: &mut Vec<Paint>, start: usize, end: usize, ink: Ink) {
    if end > start {
        runs.push(Paint {
            start: start as u32,
            end: end as u32,
            ink,
        });
    }
}

fn line_end(source: &str, i: usize) -> usize {
    source[i..]
        .find(['\n', '\r'])
        .map(|at| i + at)
        .unwrap_or(source.len())
}

fn ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn ident_end(source: &str, i: usize) -> usize {
    let mut at = i;
    while let Some(c) = source[at..].chars().next() {
        let start = at == i;
        if !(if start {
            ident_start(c)
        } else {
            ident_continue(c)
        }) {
            break;
        }
        at += c.len_utf8();
    }
    at
}

fn keyword(lang: &str, word: &str) -> bool {
    let words: &[&str] = match lang {
        "rust" => &[
            "fn", "let", "mut", "pub", "struct", "enum", "impl", "trait", "use", "mod", "const",
            "static", "if", "else", "match", "for", "while", "loop", "return", "async", "await",
            "where", "type", "self", "Self", "crate", "super", "in", "as", "ref", "move", "dyn",
            "true", "false", "unsafe",
        ],
        "python" => &[
            "def", "class", "if", "elif", "else", "for", "while", "return", "import", "from", "as",
            "with", "try", "except", "finally", "lambda", "yield", "async", "await", "True",
            "False", "None", "and", "or", "not", "in", "is", "pass", "break", "continue", "raise",
        ],
        "js" | "ts" => &[
            "function",
            "const",
            "let",
            "var",
            "if",
            "else",
            "return",
            "async",
            "await",
            "class",
            "extends",
            "new",
            "import",
            "export",
            "from",
            "for",
            "while",
            "of",
            "in",
            "this",
            "true",
            "false",
            "null",
            "undefined",
            "type",
            "interface",
            "enum",
            "public",
            "private",
            "protected",
            "readonly",
            "implements",
            "typeof",
            "instanceof",
        ],
        "go" => &[
            "func",
            "package",
            "import",
            "var",
            "const",
            "type",
            "struct",
            "interface",
            "if",
            "else",
            "for",
            "range",
            "return",
            "go",
            "defer",
            "select",
            "switch",
            "case",
            "default",
            "map",
            "chan",
            "true",
            "false",
            "nil",
        ],
        "c" | "cpp" => &[
            "if", "else", "for", "while", "return", "struct", "enum", "typedef", "const", "static",
            "sizeof", "int", "char", "void", "bool", "float", "double", "long", "short",
            "unsigned", "signed", "switch", "case", "default", "break", "continue", "do", "true",
            "false",
        ],
        "sql" => &[
            "select", "from", "where", "and", "or", "not", "insert", "into", "values", "update",
            "set", "delete", "join", "left", "right", "inner", "outer", "on", "group", "by",
            "order", "limit", "as", "create", "table", "index", "drop", "alter", "null", "true",
            "false", "distinct", "having", "union", "case", "when", "then", "else", "end",
        ],
        "bash" => &[
            "if", "then", "else", "elif", "fi", "for", "while", "do", "done", "case", "esac", "in",
            "function", "return", "local", "export", "echo",
        ],
        "json" | "yaml" => &["true", "false", "null"],
        _ => &[],
    };
    if words.contains(&word) {
        return true;
    }
    lang == "cpp"
        && matches!(
            word,
            "class"
                | "public"
                | "private"
                | "protected"
                | "namespace"
                | "template"
                | "typename"
                | "virtual"
                | "override"
                | "new"
                | "delete"
                | "nullptr"
                | "auto"
                | "using"
        )
}

fn type_name(lang: &str, word: &str) -> bool {
    matches!(lang, "rust" | "go" | "python" | "cpp" | "c" | "ts")
        && word.chars().next().is_some_and(|c| c.is_ascii_uppercase())
}

fn after_word(source: &str, end: usize, mark: char) -> bool {
    source[end..].trim_start().starts_with(mark)
}

fn number_end(source: &str, start: usize) -> usize {
    let bytes = source.as_bytes();
    let mut i = start;
    if bytes[i] == b'0' && matches!(bytes.get(i + 1), Some(b'x' | b'X')) {
        i += 2;
        while i < bytes.len() && (bytes[i].is_ascii_hexdigit() || bytes[i] == b'_') {
            i += 1;
        }
        return i;
    }
    if bytes[i] == b'.' {
        i += 1;
    }
    while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'_') {
        i += 1;
    }
    if bytes.get(i) == Some(&b'.') && bytes.get(i + 1).is_some_and(|byte| byte.is_ascii_digit()) {
        i += 1;
        while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'_') {
            i += 1;
        }
    }
    if matches!(bytes.get(i), Some(b'e' | b'E')) {
        let mut j = i + 1;
        if matches!(bytes.get(j), Some(b'+' | b'-')) {
            j += 1;
        }
        if bytes.get(j).is_some_and(|byte| byte.is_ascii_digit()) {
            i = j;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
        }
    }
    i
}

fn is_number(source: &str, i: usize) -> bool {
    let bytes = source.as_bytes();
    bytes[i].is_ascii_digit()
        || (bytes[i] == b'.' && bytes.get(i + 1).is_some_and(|byte| byte.is_ascii_digit()))
}

/// A Rust `'a` is a lifetime. `'a'` and `'\n'` are characters, so they stay strings.
fn rust_lifetime(source: &str, i: usize) -> bool {
    let rest = &source[i + 1..];
    let Some(first) = rest.chars().next() else {
        return false;
    };
    if first == '\\' || !ident_start(first) {
        return false;
    }
    source.as_bytes().get(i + 1 + first.len_utf8()) != Some(&b'\'')
}

fn end_of_string(source: &str, open: usize, quote: char) -> usize {
    let bytes = source.as_bytes();
    let mut i = open + 1;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 1;
            if i < source.len() {
                i += source[i..]
                    .chars()
                    .next()
                    .map(|c| c.len_utf8())
                    .unwrap_or(1);
            }
            continue;
        }
        if bytes[i] == quote as u8 {
            return i + 1;
        }
        if bytes[i] == b'\n' {
            return i;
        }
        i += source[i..].chars().next().unwrap().len_utf8();
    }
    source.len()
}

fn code(lang: &str, source: &str) -> Lit {
    let rules = rules_of(lang);
    let mut runs = Vec::new();
    let mut i = 0;
    let mut at_line = true;
    while i < source.len() {
        let rest = &source[i..];
        let c = rest.chars().next().unwrap();
        if c == '\n' {
            i += 1;
            at_line = true;
            continue;
        }
        if c == '\r' || c.is_whitespace() {
            i += c.len_utf8();
            continue;
        }
        if rules.directive && at_line && rest.starts_with('#') {
            let end = line_end(source, i);
            push(&mut runs, i, end, Ink::Keyword);
            i = end;
            at_line = false;
            continue;
        }
        if rules.hash && rest.starts_with('#') {
            let end = line_end(source, i);
            push(&mut runs, i, end, Ink::Comment);
            i = end;
            at_line = false;
            continue;
        }
        if rules.slash && rest.starts_with("//") {
            let end = line_end(source, i);
            push(&mut runs, i, end, Ink::Comment);
            i = end;
            at_line = false;
            continue;
        }
        if rules.block && rest.starts_with("/*") {
            let end = source[i + 2..]
                .find("*/")
                .map(|at| i + 2 + at + 2)
                .unwrap_or(source.len());
            push(&mut runs, i, end, Ink::Comment);
            i = end;
            at_line = false;
            continue;
        }
        if rules.dashes && rest.starts_with("--") {
            let end = line_end(source, i);
            push(&mut runs, i, end, Ink::Comment);
            i = end;
            at_line = false;
            continue;
        }
        if lang == "python" && (rest.starts_with("\"\"\"") || rest.starts_with("'''")) {
            let closer = &rest[..3];
            let end = source[i + 3..]
                .find(closer)
                .map(|at| i + 3 + at + 3)
                .unwrap_or(source.len());
            push(&mut runs, i, end, Ink::String);
            i = end;
            at_line = false;
            continue;
        }
        if c == '\'' && lang == "rust" && rust_lifetime(source, i) {
            i += 1;
            at_line = false;
            continue;
        }
        if c == '"' || c == '\'' || (c == '`' && (lang == "js" || lang == "ts")) {
            let end = end_of_string(source, i, c);
            push(&mut runs, i, end, Ink::String);
            i = end;
            at_line = false;
            continue;
        }
        if is_number(source, i) {
            let end = number_end(source, i);
            push(&mut runs, i, end, Ink::Number);
            i = end;
            at_line = false;
            continue;
        }
        if ident_start(c) {
            let end = ident_end(source, i);
            let word = &source[i..end];
            let ink = if keyword(lang, word) {
                Some(Ink::Keyword)
            } else if after_word(source, end, '(') {
                Some(Ink::Function)
            } else if lang == "css" && after_word(source, end, ':') {
                Some(Ink::Keyword)
            } else if type_name(lang, word) {
                Some(Ink::Type)
            } else {
                None
            };
            if let Some(ink) = ink {
                push(&mut runs, i, end, ink);
            }
            i = end;
            at_line = false;
            continue;
        }
        i += c.len_utf8();
        at_line = false;
    }
    Lit {
        text: source.to_owned(),
        runs,
    }
}

fn html(source: &str) -> Lit {
    let mut runs = Vec::new();
    let bytes = source.as_bytes();
    let mut i = 0;
    while i < source.len() {
        let rest = &source[i..];
        if rest.starts_with("<!--") {
            let end = rest
                .find("-->")
                .map(|at| i + at + 3)
                .unwrap_or(source.len());
            push(&mut runs, i, end, Ink::Comment);
            i = end;
            continue;
        }
        if bytes[i] == b'<' {
            i += 1;
            if bytes.get(i) == Some(&b'/') {
                i += 1;
            }
            let start = i;
            if i < source.len() && ident_start(source[i..].chars().next().unwrap()) {
                i = ident_end(source, i);
                push(&mut runs, start, i, Ink::Keyword);
            }
            continue;
        }
        if bytes[i] == b'"' || bytes[i] == b'\'' {
            let end = end_of_string(source, i, bytes[i] as char);
            push(&mut runs, i, end, Ink::String);
            i = end;
            continue;
        }
        i += source[i..].chars().next().unwrap().len_utf8();
    }
    Lit {
        text: source.to_owned(),
        runs,
    }
}

fn diff(source: &str) -> Lit {
    let mut runs = Vec::new();
    let mut at = 0;
    for line in source.split_inclusive('\n') {
        let body = line.trim_end_matches(['\n', '\r']);
        let ink = if body.starts_with("+++") || body.starts_with("---") || body.starts_with("@@") {
            None
        } else if body.starts_with('+') {
            Some(Ink::Added)
        } else if body.starts_with('-') {
            Some(Ink::Removed)
        } else {
            None
        };
        if let Some(ink) = ink {
            push(&mut runs, at, at + body.len(), ink);
        }
        at += line.len();
    }
    Lit {
        text: source.to_owned(),
        runs,
    }
}

fn ansi(source: &str) -> Lit {
    let bytes = source.as_bytes();
    let mut text = String::with_capacity(source.len());
    let mut runs = Vec::new();
    let mut fg = 0u8;
    let mut bg = 0u8;
    let mut bold = false;
    let mut open: Option<usize> = None;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            close_ansi(&mut runs, &text, &mut open, fg, bg, bold);
            let (next, params) = take_sgr(bytes, i);
            i = next;
            if let Some(params) = params {
                apply_sgr(params, &mut fg, &mut bg, &mut bold);
            }
            continue;
        }
        if open.is_none() && (fg != 0 || bg != 0 || bold) {
            open = Some(text.len());
        }
        let c = source[i..].chars().next().unwrap();
        text.push(c);
        i += c.len_utf8();
    }
    close_ansi(&mut runs, &text, &mut open, fg, bg, bold);
    Lit { text, runs }
}

fn close_ansi(
    runs: &mut Vec<Paint>,
    text: &str,
    open: &mut Option<usize>,
    fg: u8,
    bg: u8,
    bold: bool,
) {
    let Some(start) = open.take() else {
        return;
    };
    if text.len() > start && (fg != 0 || bg != 0 || bold) {
        runs.push(Paint {
            start: start as u32,
            end: text.len() as u32,
            ink: Ink::Ansi { fg, bg, bold },
        });
    }
}

/// Drops one escape. `Some` is the parameter text of an SGR (`m`) sequence.
fn take_sgr(bytes: &[u8], i: usize) -> (usize, Option<&[u8]>) {
    if bytes.get(i + 1) != Some(&b'[') {
        return ((i + 1).min(bytes.len()), None);
    }
    let start = i + 2;
    let mut j = start;
    while j < bytes.len() && j - start < 64 {
        let byte = bytes[j];
        j += 1;
        if byte.is_ascii_alphabetic() {
            if byte == b'm' {
                return (j, Some(&bytes[start..j - 1]));
            }
            return (j, None);
        }
        if !(byte.is_ascii_digit() || byte == b';') {
            // A byte of a multi-byte character is not part of the escape: leave it for the caller
            // so the text is never cut mid-character.
            return (if byte.is_ascii() { j } else { j - 1 }, None);
        }
    }
    (j.min(bytes.len()), None)
}

fn apply_sgr(params: &[u8], fg: &mut u8, bg: &mut u8, bold: &mut bool) {
    if params.is_empty() {
        *fg = 0;
        *bg = 0;
        *bold = false;
        return;
    }
    let mut rest = params;
    while !rest.is_empty() {
        let (part, next) = split_param(rest);
        rest = next;
        let code = parse_u16(part);
        match code {
            0 => {
                *fg = 0;
                *bg = 0;
                *bold = false;
            }
            1 => *bold = true,
            22 => *bold = false,
            30..=37 | 90..=97 => *fg = code as u8,
            39 => *fg = 0,
            40..=47 | 100..=107 => *bg = code as u8,
            49 => *bg = 0,
            38 | 48 => {
                let (kind, next) = split_param(rest);
                let (n, next) = split_param(next);
                rest = next;
                if kind == b"5" {
                    let mapped = sgr256(parse_u16(n));
                    if code == 38 {
                        *fg = mapped;
                    } else {
                        *bg = mapped;
                    }
                }
            }
            _ => {}
        }
    }
}

fn split_param(params: &[u8]) -> (&[u8], &[u8]) {
    match params.iter().position(|byte| *byte == b';') {
        Some(at) => (&params[..at], &params[at + 1..]),
        None => (params, &[]),
    }
}

fn parse_u16(bytes: &[u8]) -> u16 {
    let mut n = 0u16;
    for byte in bytes {
        if !byte.is_ascii_digit() {
            return 0;
        }
        n = n.saturating_mul(10).saturating_add(u16::from(byte - b'0'));
    }
    n
}

fn sgr256(n: u16) -> u8 {
    match n {
        0..=7 => 30 + n as u8,
        8..=15 => 90 + (n as u8 - 8),
        _ => 30 + (n % 8) as u8,
    }
}

#[cfg(test)]
mod tests {
    use super::{highlight, prove_code_colors, Ink, PLAIN_LINES};

    fn slice(text: &str, start: u32, end: u32) -> &str {
        text.get(start as usize..end as usize).unwrap_or("")
    }

    #[test]
    fn rust_fn_is_a_keyword() {
        let lit = highlight("rust", "fn main() {\n    let x = 1;\n}\n");
        let keyword = lit
            .runs
            .iter()
            .find(|run| slice(&lit.text, run.start, run.end) == "fn");
        assert_eq!(keyword.map(|run| run.ink), Some(Ink::Keyword));
        let number = lit
            .runs
            .iter()
            .find(|run| slice(&lit.text, run.start, run.end) == "1");
        assert_eq!(number.map(|run| run.ink), Some(Ink::Number));
    }

    #[test]
    fn diff_marks_added_and_removed_lines_not_headers() {
        let lit = highlight("diff", "--- a/f\n+++ b/f\n-old\n+new\n context\n");
        let ink_of = |line: &str| {
            lit.runs
                .iter()
                .find(|run| slice(&lit.text, run.start, run.end) == line)
                .map(|run| run.ink)
        };
        assert_eq!(ink_of("-old"), Some(Ink::Removed));
        assert_eq!(ink_of("+new"), Some(Ink::Added));
        assert_eq!(ink_of("--- a/f"), None);
        assert_eq!(ink_of("+++ b/f"), None);
        assert_eq!(ink_of(" context"), None);
    }

    #[test]
    fn ansi_escape_followed_by_non_ascii_text_does_not_panic() {
        for text in ["\u{1b}[é", "\u{1b}[1;é", "a\u{1b}[😀b", "\u{1b}[31;你好\u{1b}[0m"] {
            let lit = highlight("ansi", text);
            assert!(lit.text.is_char_boundary(lit.text.len()));
            for run in &lit.runs {
                assert!(lit.text.get(run.start as usize..run.end as usize).is_some());
            }
        }
        assert_eq!(highlight("ansi", "\u{1b}[é").text, "é");
        assert_eq!(highlight("ansi", "a\u{1b}[😀b").text, "a😀b");
    }

    #[test]
    fn ansi_drops_escapes_and_keeps_the_colour() {
        let lit = highlight("ansi", "\u{1b}[1;32mgo\u{1b}[0m \u{1b}[31mstop\u{1b}[0m");
        assert_eq!(lit.text, "go stop");
        assert!(lit
            .runs
            .iter()
            .any(|run| slice(&lit.text, run.start, run.end) == "go"
                && run.ink
                    == Ink::Ansi {
                        fg: 32,
                        bg: 0,
                        bold: true
                    }));
        assert!(lit
            .runs
            .iter()
            .any(|run| slice(&lit.text, run.start, run.end) == "stop"
                && run.ink
                    == Ink::Ansi {
                        fg: 31,
                        bg: 0,
                        bold: false
                    }));
    }

    #[test]
    fn a_long_block_stays_plain() {
        let source = "let x = 1;\n".repeat(PLAIN_LINES + 1);
        let lit = highlight("rust", &source);
        assert!(lit.runs.is_empty());
        assert_eq!(lit.text, source);
    }

    #[test]
    fn the_lab_check_passes() {
        prove_code_colors().unwrap();
    }
}
