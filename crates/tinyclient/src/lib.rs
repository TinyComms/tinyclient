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
//!              imply `gui-shot`, which pulls GPUI's test support and renders to a file (a
//!              desktop design-review tool; on a phone screenshots come from `adb`/`simctl`).
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
/// Called by the Swift shell. At the pinned GPUI revision there is no iOS platform backend
/// (`gpui_platform` has `gpui_apple|linux|macos|web|wgpu|windows` only), so this is the seam,
/// not a working app: the gpui-ce / gpui-pre migration is what makes it link and run. Keep the
/// body to one call so the migration changes one line.
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
/// The `AndroidApp` handle is the shell's; the window itself is created by the platform layer,
/// so it is not used here yet. Same caveat as [`ios_main`]: the pin has no Android backend.
#[cfg(all(feature = "mobile", target_os = "android"))]
#[no_mangle]
pub extern "C" fn android_main(app: android_activity::AndroidApp) {
    let _ = app;
    gui::run();
}
