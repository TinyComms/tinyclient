//! What the Logs window (Help › Logs) shows: the tail of each process's file in
//! `log_policy::logs_dir()`, kept to the last [`KEEP`] lines a process, and the filter over them.
//!
//! The files are what is read, not the in-memory ring: they are what the operator sees on disk and
//! what Save debug log is checked against, and chatd and mediad write theirs without being asked.
//! Every line in them went through `log_policy::render`, so it is already redacted; nothing here
//! adds to a line but the name of the file it came from.
//!
//! Nothing here touches the window, so it is tested without one.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Arc;

/// Lines kept for each process. A line is about 150 bytes, so three full rings are a few MB.
pub const KEEP: usize = 5000;
/// How far back from the end the first read starts: enough for [`KEEP`] lines, and less than
/// the 2 MiB a file rolls at.
const BACKFILL: u64 = 1024 * 1024;
/// A line longer than this with no end yet is shown as it is rather than held.
const PARTIAL_LIMIT: usize = 64 * 1024;

/// Lines that say nothing about tinyclient: GPUI on Windows logs the first when a window
/// redraws inside another's update, and the second when a monitor sleeps. "Hide noise" drops them.
pub const NOISE: &[&str] = &["RefCell already borrowed", "No monitor detected!"];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    Tinyclient,
    Chatd,
    Mediad,
}

impl Source {
    pub const ALL: [Source; 3] = [Source::Tinyclient, Source::Chatd, Source::Mediad];

    pub const fn name(self) -> &'static str {
        match self {
            Source::Tinyclient => "tinyclient",
            Source::Chatd => "chatd",
            Source::Mediad => "mediad",
        }
    }

    fn index(self) -> usize {
        self as usize
    }

    pub fn path(self) -> PathBuf {
        protocol::log_policy::logs_dir().join(format!("{}.log", self.name()))
    }
}

/// Ordered from the fewest lines to the most, so a filter keeps what is at or below it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl Level {
    /// The choices the window offers. Debug shows trace too.
    pub const SHOWN: [Level; 4] = [Level::Error, Level::Warn, Level::Info, Level::Debug];

    pub const fn name(self) -> &'static str {
        match self {
            Level::Error => "Error",
            Level::Warn => "Warn",
            Level::Info => "Info",
            Level::Debug => "Debug",
            Level::Trace => "Trace",
        }
    }
}

/// The level a file line starts with (`render` writes `warn chatd::media ...`). `None` for a
/// line that does not, such as the second line of a message with a newline in it.
pub fn level_of(line: &str) -> Option<Level> {
    Some(match line.split_once(' ').map_or(line, |(word, _)| word) {
        "error" => Level::Error,
        "warn" => Level::Warn,
        "info" => Level::Info,
        "debug" => Level::Debug,
        "trace" => Level::Trace,
        _ => return None,
    })
}

pub fn is_noise(text: &str) -> bool {
    NOISE.iter().any(|noise| text.contains(noise))
}

/// One line of a file. `seq` orders lines across the three files in the order they were read.
#[derive(Debug, PartialEq, Eq)]
pub struct Line {
    pub source: Source,
    pub level: Option<Level>,
    pub seq: u64,
    pub text: String,
}

/// Splits what was read into whole lines. A line with no end yet waits in `partial` for the next
/// read, unless it has grown past [`PARTIAL_LIMIT`]. Bytes are kept until the line ends, so a
/// character cut by the read is whole when it is decoded.
pub fn split_lines(partial: &mut Vec<u8>, chunk: &[u8]) -> Vec<String> {
    partial.extend_from_slice(chunk);
    let mut lines = Vec::new();
    let mut start = 0;
    while let Some(end) = partial[start..].iter().position(|byte| *byte == b'\n') {
        let line = &partial[start..start + end];
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        lines.push(String::from_utf8_lossy(line).into_owned());
        start += end + 1;
    }
    partial.drain(..start);
    if partial.len() > PARTIAL_LIMIT {
        lines.push(String::from_utf8_lossy(partial).into_owned());
        partial.clear();
    }
    lines
}

/// Where one file has been read to.
#[derive(Debug)]
pub struct Follow {
    path: PathBuf,
    /// `None` until the first read, which starts near the end.
    offset: Option<u64>,
    partial: Vec<u8>,
}

impl Follow {
    pub fn new(path: PathBuf) -> Follow {
        Follow { path, offset: None, partial: Vec::new() }
    }

    /// The whole lines written since the last read. The first read takes the last
    /// [`BACKFILL`] bytes and drops the line it starts inside. A file shorter than where the last
    /// read ended was rolled (`log_policy::rotate_log`): the new one is read from its start. A
    /// file that is not there yet reads as nothing.
    pub fn read(&mut self) -> io::Result<Vec<String>> {
        let mut file = match File::open(&self.path) {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err),
        };
        let len = file.metadata()?.len();
        let (from, mid_line) = match self.offset {
            None => (len.saturating_sub(BACKFILL), len > BACKFILL),
            Some(offset) if offset > len => {
                self.partial.clear();
                (0, false)
            }
            Some(offset) => (offset, false),
        };
        if from == len {
            self.offset = Some(len);
            return Ok(Vec::new());
        }
        file.seek(SeekFrom::Start(from))?;
        let mut chunk = Vec::with_capacity((len - from) as usize);
        file.take(len - from).read_to_end(&mut chunk)?;
        self.offset = Some(from + chunk.len() as u64);
        let mut lines = split_lines(&mut self.partial, &chunk);
        if mid_line && !lines.is_empty() {
            lines.remove(0);
        }
        Ok(lines)
    }
}

/// The last [`KEEP`] lines of each file, and the next sequence number.
#[derive(Debug)]
pub struct Tail {
    rings: [VecDeque<Arc<Line>>; 3],
    keep: usize,
    seq: u64,
}

impl Default for Tail {
    fn default() -> Self {
        Tail::with_keep(KEEP)
    }
}

impl Tail {
    pub fn with_keep(keep: usize) -> Tail {
        Tail { rings: Default::default(), keep, seq: 0 }
    }

    /// Adds lines read from `source`'s file. A line with no level of its own continues the one
    /// before it and takes its level, so a level filter keeps a message's lines together.
    pub fn push(&mut self, source: Source, lines: Vec<String>) {
        let ring = &mut self.rings[source.index()];
        let mut level = ring.back().and_then(|line| line.level);
        for text in lines {
            level = level_of(&text).or(level);
            self.seq += 1;
            ring.push_back(Arc::new(Line { source, level, seq: self.seq, text }));
            if ring.len() > self.keep {
                ring.pop_front();
            }
        }
    }

    /// Lines read so far, kept or not: the window counts what arrived while it was paused.
    pub fn seq(&self) -> u64 {
        self.seq
    }

    pub fn len(&self, source: Source) -> usize {
        self.rings[source.index()].len()
    }

    /// The lines `filter` keeps, oldest first. With every source, the three files interleave
    /// in the order their lines were read.
    pub fn visible(&self, filter: &Filter) -> Vec<Arc<Line>> {
        let mut out: Vec<Arc<Line>> = match filter.source {
            Some(source) => self.rings[source.index()].iter().filter(|line| filter.admits(line)).cloned().collect(),
            None => self.rings.iter().flatten().filter(|line| filter.admits(line)).cloned().collect(),
        };
        if filter.source.is_none() {
            out.sort_by_key(|line| line.seq);
        }
        out
    }
}

/// What the window shows of the tail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Filter {
    /// `None` is every process.
    pub source: Option<Source>,
    /// The most detailed level shown; Debug includes trace.
    pub level: Level,
    /// Shown only if the line holds these words, in any case. Empty keeps everything.
    pub text: String,
    pub hide_noise: bool,
}

impl Default for Filter {
    fn default() -> Self {
        Filter { source: None, level: Level::Debug, text: String::new(), hide_noise: true }
    }
}

impl Filter {
    pub fn admits(&self, line: &Line) -> bool {
        if self.source.is_some_and(|source| source != line.source) {
            return false;
        }
        let ceiling = if self.level == Level::Debug { Level::Trace } else { self.level };
        if line.level.is_some_and(|level| level > ceiling) {
            return false;
        }
        if self.hide_noise && is_noise(&line.text) {
            return false;
        }
        let words = self.text.trim();
        words.is_empty() || contains_folded(&line.text, words)
    }
}

/// Whether `haystack` holds `needle`, ignoring ASCII case. Log lines are mostly ASCII, and this
/// allocates nothing per line.
fn contains_folded(haystack: &str, needle: &str) -> bool {
    let (haystack, needle) = (haystack.as_bytes(), needle.as_bytes());
    needle.len() <= haystack.len() && haystack.windows(needle.len()).any(|window| window.eq_ignore_ascii_case(needle))
}

/// The lines as Copy visible puts them on the clipboard: with every source shown, each line
/// says which file it came from.
pub fn copy_text(lines: &[Arc<Line>], every_source: bool) -> String {
    let mut out = String::new();
    for line in lines {
        if every_source {
            out.push_str(line.source.name());
            out.push_str(": ");
        }
        out.push_str(&line.text);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn texts(lines: &[Arc<Line>]) -> Vec<&str> {
        lines.iter().map(|line| line.text.as_str()).collect()
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tinyclient-log-tail-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("chatd.log")
    }

    #[test]
    fn levels_are_read_from_the_first_word() {
        assert_eq!(level_of("warn chatd::media call state session=ab"), Some(Level::Warn));
        assert_eq!(level_of("error log RefCell already borrowed"), Some(Level::Error));
        assert_eq!(level_of("trace mediad x"), Some(Level::Trace));
        assert_eq!(level_of("debug"), Some(Level::Debug));
        assert_eq!(level_of("  at line 2 of a message"), None);
        assert_eq!(level_of("warning chatd"), None);
        assert_eq!(level_of(""), None);
        assert!(Level::Error < Level::Warn && Level::Info < Level::Debug && Level::Debug < Level::Trace);
    }

    #[test]
    fn split_holds_a_line_until_it_ends_and_a_character_until_it_is_whole() {
        let mut partial = Vec::new();
        assert_eq!(split_lines(&mut partial, b"info a\r\nwarn b"), ["info a"]);
        assert_eq!(partial, b"warn b");
        // "é" is two bytes; the read ends between them.
        assert!(split_lines(&mut partial, b" \xc3").is_empty());
        assert_eq!(split_lines(&mut partial, b"\xa9\n\n"), ["warn b é", ""]);
        assert!(partial.is_empty());
        let long = vec![b'x'; PARTIAL_LIMIT + 1];
        assert_eq!(split_lines(&mut partial, &long).len(), 1);
        assert!(partial.is_empty());
    }

    #[test]
    fn the_ring_keeps_the_last_lines_of_each_source_alone() {
        let mut tail = Tail::with_keep(3);
        tail.push(Source::Chatd, (0..5).map(|n| format!("info chatd {n}")).collect());
        tail.push(Source::Mediad, vec!["warn mediad one".into()]);
        assert_eq!(tail.len(Source::Chatd), 3);
        assert_eq!(tail.len(Source::Mediad), 1);
        assert_eq!(tail.seq(), 6);
        let chatd = tail.visible(&Filter { source: Some(Source::Chatd), ..Filter::default() });
        assert_eq!(texts(&chatd), ["info chatd 2", "info chatd 3", "info chatd 4"]);
        // Every source: in the order read.
        tail.push(Source::Chatd, vec!["info chatd 5".into()]);
        let all = tail.visible(&Filter::default());
        assert_eq!(texts(&all), ["info chatd 3", "info chatd 4", "warn mediad one", "info chatd 5"]);
    }

    #[test]
    fn a_line_without_a_level_takes_the_one_before_it() {
        let mut tail = Tail::default();
        tail.push(Source::Tinyclient, vec!["debug tinyclient first".into(), "  second line".into()]);
        tail.push(Source::Tinyclient, vec!["  third line".into(), "warn tinyclient other".into()]);
        let info = tail.visible(&Filter { level: Level::Info, ..Filter::default() });
        assert_eq!(texts(&info), ["warn tinyclient other"]);
        assert_eq!(tail.visible(&Filter::default()).len(), 4);
    }

    #[test]
    fn the_filter_takes_source_level_words_and_noise() {
        let mut tail = Tail::default();
        tail.push(Source::Tinyclient, vec![
            "error log RefCell already borrowed log.target=gpui::window".into(),
            "warn tinyclient No monitor detected!".into(),
            "info tinyclient::gui opened Settings".into(),
            "debug tinyclient::gui drew 3 rows".into(),
            "trace tinyclient::gui deep".into(),
        ]);
        tail.push(Source::Chatd, vec!["error chatd Discord said 401".into()]);
        let shown = |filter: Filter| texts(&tail.visible(&filter)).into_iter().map(str::to_owned).collect::<Vec<_>>();
        assert_eq!(shown(Filter::default()).len(), 4);
        assert_eq!(shown(Filter { hide_noise: false, ..Filter::default() }).len(), 6);
        assert_eq!(shown(Filter { level: Level::Warn, ..Filter::default() }), ["error chatd Discord said 401"]);
        assert_eq!(shown(Filter { level: Level::Info, source: Some(Source::Tinyclient), ..Filter::default() }), ["info tinyclient::gui opened Settings"]);
        assert_eq!(shown(Filter { text: " DISCORD ".into(), ..Filter::default() }), ["error chatd Discord said 401"]);
        assert!(shown(Filter { text: "nothing like this".into(), ..Filter::default() }).is_empty());
        let copied = copy_text(&tail.visible(&Filter { level: Level::Warn, ..Filter::default() }), true);
        assert_eq!(copied, "chatd: error chatd Discord said 401\n");
    }

    #[test]
    fn follow_backfills_then_reads_appends_and_starts_over_after_a_roll() {
        let path = scratch("follow");
        let mut follow = Follow::new(path.clone());
        assert!(follow.read().unwrap().is_empty());
        let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&path).unwrap();
        file.write_all(b"info chatd one\ninfo chatd tw").unwrap();
        assert_eq!(follow.read().unwrap(), ["info chatd one"]);
        file.write_all(b"o\nwarn chatd three\n").unwrap();
        assert_eq!(follow.read().unwrap(), ["info chatd two", "warn chatd three"]);
        assert!(follow.read().unwrap().is_empty());
        drop(file);
        // Rolled: the live file starts again, shorter than where the last read ended.
        std::fs::rename(&path, path.with_extension("log.1")).unwrap();
        std::fs::write(&path, b"info chatd new\n").unwrap();
        assert_eq!(follow.read().unwrap(), ["info chatd new"]);
        // A first read from the middle of a big file drops the line it lands in.
        let big = path.with_file_name("mediad.log");
        let line = "debug mediad::video uplink video=0 estimate=Some(1000000) loss=Some(0.0)\n";
        let mut text = String::new();
        while (text.len() as u64) < BACKFILL + 4096 {
            text.push_str(line);
        }
        text.push_str("info mediad last\n");
        std::fs::write(&big, &text).unwrap();
        let lines = Follow::new(big).read().unwrap();
        assert!(lines.iter().all(|read| format!("{read}\n") == line || read == "info mediad last"));
        assert_eq!(lines.last().map(String::as_str), Some("info mediad last"));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
