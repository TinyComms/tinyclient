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
mod tiny_attach;
#[cfg(all(feature = "gui", any(target_os = "ios", target_os = "android")))]
mod update;
#[cfg(all(feature = "gui", any(target_os = "ios", target_os = "android")))]
mod view;

/// Starts the window on iOS.
///
/// Called by the Swift shell. The pinned Zed GPUI revision (`Cargo.toml`) has no iOS platform
/// backend: its `gpui_platform` has `gpui_apple|linux|macos|web|wgpu|windows` arms only, so this
/// is a seam, not a working app. Moving to gpui-ce (`gpui-ce` 0.3.0 on crates.io) is what would
/// add one; that migration is not made yet. The `mobile` build first starts chatd's core in the app.
#[cfg(all(feature = "gui", target_os = "ios"))]
pub fn ios_main() {
    #[cfg(feature = "mobile")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            protocol::set_support_dir(std::path::PathBuf::from(home).join("Library/Application Support/tinyclient"));
        }
        inprocess::register();
        inprocess::start_now();
    }
    gui::run(None, None);
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
/// Before the window starts this names the app's internal data directory for chatd's support
/// directory (`protocol::set_support_dir`): Android has no `HOME`, so `protocol::support_dir` would
/// otherwise put the socket and database under `/tmp`. Then chatd's core starts in this process.
/// GPUI (gpui-ce) runs on the NativeActivity session that `init_platform` opens for this `app`.
/// The session's guard stays alive until the window's event loop returns. The platform must come
/// from that session: `gpui_platform::application()` on Android builds a platform with no session,
/// which fires the launch callback before any window exists.
#[cfg(all(feature = "mobile", target_os = "android"))]
#[no_mangle]
pub extern "C" fn android_main(app: android_activity::AndroidApp) {
    android_log::install();
    match app.internal_data_path() {
        Some(dir) => protocol::set_support_dir(dir),
        None => log::error!("tinyclient: Android gave no internal data path; the data falls back to /tmp"),
    }
    inprocess::register();
    inprocess::start_now();
    let _session = match gpui_mobile::android::jni::init_platform(&app) {
        Ok(session) => session,
        Err(err) => {
            log::error!("tinyclient: the NativeActivity session did not start: {err}");
            return;
        }
    };
    let Some(platform) = gpui_mobile::android::jni::shared_platform() else {
        log::error!("tinyclient: the NativeActivity session has no platform");
        return;
    };
    gui::run_with(gpui::Application::with_platform(platform.into_rc()), None, None);
}

/// chatd on a phone: the core runs in this process, on a thread of its own, over the socket in the
/// app's data directory. The shell's start path (`protocol::launch::start_chatd`) calls the
/// starter registered here instead of spawning a binary that a phone does not have.
#[cfg(all(feature = "mobile", any(target_os = "android", target_os = "ios")))]
mod inprocess {
    use std::sync::Mutex;
    use std::thread::JoinHandle;

    /// The serving thread, while there is one. Replaced only once it has finished.
    static CORE: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);

    pub fn register() {
        protocol::launch::register_in_process(start);
    }

    /// Starts the core unless one is still serving: the shell may ask again while it waits.
    fn start() -> std::io::Result<()> {
        let mut core = CORE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if core.as_ref().is_some_and(|handle| !handle.is_finished()) {
            return Ok(());
        }
        *core = Some(chatd::start_in_process(protocol::support_dir())?);
        Ok(())
    }

    /// Starts the core before the window, so the first `Ping` already finds it. A failure is logged
    /// and the shell's own start path tries again and reports it.
    pub fn start_now() {
        if let Err(err) = start() {
            log::error!("tinyclient: chatd did not start in the app: {err}");
        }
    }
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
