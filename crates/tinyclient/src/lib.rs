//! The library half of `tinyclient`: the same window code as the desktop binary, reachable from
//! an iOS or Android shell.
//!
//! `src/main.rs` stays the desktop entry point and is not compiled for mobile: a phone has no
//! terminal to print `--version` or `--text` to, and `main` would be dead weight in a
//! static library. Instead this crate is built as a library for mobile targets and the shells
//! call the entry points at the bottom of this file.
//!
//! Features:
//! * `gui`    — the window (GPUI), shared with the desktop binary.
//! * `mobile` — adds the platform entry points. It implies `gui` and deliberately does not
//!   imply `gui-shot`, which pulls GPUI's test support and renders to a file (a desktop
//!   design-review tool; on a phone screenshots come from `adb`/`simctl`).
//!
//! `mobile` + the desktop binary can be built together (`cargo check -p tinyclient --features
//! "gui mobile"`); the entry points themselves are behind `cfg(target_os)`, so on a desktop
//! host that command compiles the desktop arms and nothing else.

// `main.rs` declares these modules for the binary, and `gui` reaches them through `crate::…`.
// The library has to declare them too, or `gui/mod.rs` would not compile into it. They are
// gated on mobile targets so a desktop `cargo check --features gui` does not compile the whole
// window twice (once for the binary, once for an unused library).
#[cfg(all(feature = "gui", any(target_os = "ios", target_os = "android")))]
mod client;
#[cfg(all(feature = "gui", any(target_os = "ios", target_os = "android")))]
mod frames;
#[cfg(all(feature = "gui", any(target_os = "ios", target_os = "android")))]
mod gui;
#[cfg(all(feature = "gui", any(target_os = "ios", target_os = "android")))]
mod host;
#[cfg(all(feature = "gui", any(target_os = "ios", target_os = "android")))]
mod lottie;
#[cfg(all(feature = "gui", any(target_os = "ios", target_os = "android")))]
mod update;
#[cfg(all(feature = "gui", any(target_os = "ios", target_os = "android")))]
mod view;

/// Starts the window on iOS.
///
/// Called by the Swift shell. GPUI is gpui-ce 0.3.0 (`Cargo.toml`), whose `gpui_ce_platform`
/// carries the iOS backend. Keep the body to one call.
#[cfg(all(feature = "gui", target_os = "ios"))]
pub fn ios_main() {
    gui::run();
}

/// The C symbol the Swift target declares in `Tiny-Bridging-Header.h`.
///
/// `#[no_mangle]` + `extern "C"` is the whole ABI for one function; UniFFI is not worth its
/// version-lock for a single entry point (see `research_notes/mobile/rust-core-native-shells.md`
/// §1). It is in the static library because `[lib] crate-type` includes `staticlib`.
#[cfg(all(feature = "gui", target_os = "ios"))]
#[no_mangle]
pub extern "C" fn tinyclient_ios_main() {
    ios_main();
}

/// Starts the window on Android.
///
/// `android_activity` exports `ANativeActivity_onCreate` and calls this on the thread it owns;
/// `AndroidManifest.xml` points NativeActivity at `libtinyclient.so` (`android.app.lib_name`).
/// Before the window starts this points `TINYCLIENT_DIR` at the app's internal data directory:
/// Android has no `HOME`, so `protocol::support_dir` would otherwise put the socket and database
/// under `/tmp`. GPUI's Android backend is `gpui_ce_mobile`, linked by `Cargo.toml`.
#[cfg(all(feature = "mobile", target_os = "android"))]
#[no_mangle]
pub extern "C" fn android_main(app: android_activity::AndroidApp) {
    android_log::install();
    match app.internal_data_path() {
        // Set before any thread of ours starts, so nothing reads the environment concurrently.
        Some(dir) => std::env::set_var("TINYCLIENT_DIR", dir),
        None => log::error!("tinyclient: Android gave no internal data path; the data falls back to /tmp"),
    }
    gui::run();
}

/// `log` records to logcat. NativeActivity discards stderr, so `eprintln!` is lost on a phone.
#[cfg(all(feature = "mobile", target_os = "android"))]
mod android_log {
    use std::os::raw::{c_char, c_int};

    #[link(name = "log")]
    extern "C" {
        fn __android_log_write(prio: c_int, tag: *const c_char, text: *const c_char) -> c_int;
    }

    struct Logcat;

    static LOGCAT: Logcat = Logcat;

    impl log::Log for Logcat {
        fn enabled(&self, metadata: &log::Metadata) -> bool {
            metadata.level() <= log::Level::Info
        }

        fn log(&self, record: &log::Record) {
            if !self.enabled(record.metadata()) {
                return;
            }
            // ANDROID_LOG_* priorities: ERROR 6, WARN 5, INFO 4, DEBUG 3, VERBOSE 2.
            let prio = match record.level() {
                log::Level::Error => 6,
                log::Level::Warn => 5,
                log::Level::Info => 4,
                log::Level::Debug => 3,
                log::Level::Trace => 2,
            };
            let tag = std::ffi::CString::new("tinyclient").unwrap_or_default();
            let text = std::ffi::CString::new(record.args().to_string().replace('\0', "")).unwrap_or_default();
            // SAFETY: both strings are NUL-terminated and live for the call.
            unsafe { __android_log_write(prio, tag.as_ptr(), text.as_ptr()) };
        }

        fn flush(&self) {}
    }

    pub fn install() {
        if log::set_logger(&LOGCAT).is_ok() {
            log::set_max_level(log::LevelFilter::Info);
        }
    }
}
