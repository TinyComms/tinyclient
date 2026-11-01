//! `log` records to the unified log (NSLog), so `log stream` and Console show what the app did:
//! the iOS counterpart of Android's logcat logger in `lib.rs`.

use objc2_foundation::NSString;

extern "C" {
    /// Foundation's `NSLog(NSString *format, ...)`.
    fn NSLog(format: *const NSString, ...);
}

struct Unified;

static LOGGER: Unified = Unified;

impl log::Log for Unified {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Info
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = NSString::from_str(&format!("[{}] {}", record.level(), record.args()));
        let format = NSString::from_str("%@");
        // SAFETY: a "%@" format with one NSString argument, both alive for the call.
        unsafe { NSLog(&*format, &*line as *const NSString) };
    }

    fn flush(&self) {}
}

/// Installs the logger once; a logger already set (none is, today) is kept.
pub(super) fn install() {
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
}
