//! `log` records to the unified log (NSLog), so `log stream` and Console show what the app did:
//! the iOS counterpart of Android's logcat logger in `lib.rs`.

use objc2_foundation::NSString;

extern "C" {
    /// Foundation's `NSLog(NSString *format, ...)`.
    fn NSLog(format: *const NSString, ...);
}

struct Unified;

static LOGGER: Unified = Unified;

/// The log file, opened at the first record (the support directory is named before that).
static FILE: std::sync::Mutex<Option<std::fs::File>> = std::sync::Mutex::new(None);

impl log::Log for Unified {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Info
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let text = format!("[{}] {}", record.level(), record.args());
        let line = NSString::from_str(&text);
        let format = NSString::from_str("%@");
        // SAFETY: a "%@" format with one NSString argument, both alive for the call.
        unsafe { NSLog(&*format, &*line as *const NSString) };
        // And a file beside the app's data, for a simulator or a device whose unified log is
        // not at hand (`simctl get_app_container … data`, Library/Application Support).
        if let Ok(mut file) = FILE.lock() {
            if file.is_none() {
                let _ = std::fs::create_dir_all(protocol::support_dir());
                let path = protocol::support_dir().join("ios-platform.log");
                *file = std::fs::OpenOptions::new().create(true).append(true).open(path).ok();
            }
            if let Some(file) = file.as_mut() {
                use std::io::Write;
                let _ = writeln!(file, "{} {text}", super::pixels::now_us() / 1000);
            }
        }
    }

    fn flush(&self) {}
}

/// Installs the logger once; a logger already set (none is, today) is kept.
pub(super) fn install() {
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
    // A panic's message goes to stderr, which nothing reads on a phone: say it in the log too.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        log::error!("panic on thread {}: {info}", thread.name().unwrap_or("?"));
        previous(info);
    }));
}
