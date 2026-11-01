# Building tinyclient for mobile

Status: **scaffolding, not a working app.** This branch adds the build graph, the feature
switch, the two host skeletons and CI. The pinned GPUI revision has no iOS or Android platform
backend, so `gui/` cannot yet run on a phone; see
`research_notes/mobile/design/20-implementation-map.md` §4 for the migration that changes that.

Everything below says which parts were run on the Windows machine that wrote this and which
parts were not. Nothing here was built for a phone.

---

## 1. What exists

| Path | What |
|---|---|
| `crates/tinyclient/src/lib.rs` | library target: `ios_main()`, the `tinyclient_ios_main` C symbol, `android_main` |
| `crates/tinyclient/Cargo.toml` | `[lib]` with `rlib`/`staticlib`/`cdylib`; `mobile` feature; Android-only `android-activity` dependency |
| `mobile/android/` | Gradle project: NativeActivity, `jniLibs`, AGP 9.5.0, minSdk 29, targetSdk 36 |
| `mobile/ios/` | XcodeGen project, `Info.plist`, Swift `AppDelegate`/`SceneDelegate`, bridging header |
| `scripts/mobile/build-android.ps1` | Windows: NDK check → `cargo ndk` → `gradlew assembleDebug` |
| `scripts/mobile/build-android.sh` | Linux/macOS: the same |
| `scripts/mobile/build-ios.sh` | macOS: Rust device + simulator → `lipo` → `xcodebuild -create-xcframework` |
| `scripts/mobile/seed-demo.ps1` | `chatd --seed-demo` in a scratch `TINYCLIENT_DIR`, prints the env |
| `.github/workflows/mobile.yml` | `host-check`, `android-libs`, `android-app`, `ios-libs`, `ios-app` |

`src/main.rs` is unchanged: the desktop binary keeps its own module tree and its own entry point.

---

## 2. Prerequisites

### Android (works on Windows, Linux and macOS)

| Need | Version used | Why |
|---|---|---|
| Rust + rustup | stable 1.99.1 | `aarch64-linux-android`, `x86_64-linux-android` targets |
| `cargo-ndk` | 4.1.2 | `cargo install cargo-ndk --version 4.1.2 --locked`; no release since 2025-08 but `main` is active |
| Android NDK | r30 LTS `30.0.16248370` | compiles 16 KB-aligned by default; r28+ is the floor |
| Android SDK | platform 36, build-tools 36 | `compileSdk = 36` |
| JDK | 17 or newer | AGP 9 requires it; JDK 21 is what CI uses |
| Gradle | 9.6 | AGP 9.5 requires it. No wrapper jar is committed — see §3 |

```powershell
rustup target add aarch64-linux-android x86_64-linux-android
cargo install cargo-ndk --version 4.1.2 --locked
$env:ANDROID_NDK_HOME = "$env:LOCALAPPDATA\Android\Sdk\ndk\30.0.16248370"
```

```bash
rustup target add aarch64-linux-android x86_64-linux-android
cargo install cargo-ndk --version 4.1.2 --locked
export ANDROID_NDK_HOME="$HOME/Android/Sdk/ndk/30.0.16248370"
```

### iOS (macOS only)

| Need | Version used | Why |
|---|---|---|
| Xcode | 26.6 today; 27.2 current | the iOS SDK and `lipo` |
| XcodeGen | current (Homebrew) | `project.yml` → `.xcodeproj`, editable from Windows |
| Rust targets | `aarch64-apple-ios`, `aarch64-apple-ios-sim`, `x86_64-apple-ios` | device, Apple-silicon sim, Intel sim |

```bash
rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios
brew install xcodegen
```

There is no no-Mac path for the *app*: `xtool` 1.24 can build a bundle on Windows/WSL but cannot
upload to App Store Connect and cannot compile asset catalogs. CI's `macos-26` runner is the
supported route.

---

## 3. Exact commands

### Android

```powershell
# Windows, from the repository root
.\scripts\mobile\build-android.ps1                    # debug APK, arm64-v8a + x86_64
.\scripts\mobile\build-android.ps1 -Profile release
.\scripts\mobile\build-android.ps1 -SkipGradle        # only the .so files
```

```bash
# Linux/macOS
scripts/mobile/build-android.sh
scripts/mobile/build-android.sh --release
scripts/mobile/build-android.sh --skip-gradle
```

The scripts resolve the NDK from `ANDROID_NDK_HOME`, `ANDROID_NDK_ROOT` or the SDK's `ndk/`
directory, check every Rust target, check `cargo-ndk`, and then run:

```bash
cargo ndk -t arm64-v8a -t x86_64 \
  -o mobile/android/app/src/main/jniLibs \
  build --release -p tinyclient --lib --features mobile
cd mobile/android && gradle :app:assembleDebug        # or ./gradlew, if bootstrapped
```

Output: `mobile/android/app/build/outputs/apk/debug/app-debug.apk`.

**Gradle wrapper.** The wrapper jar is a binary and is not committed, so `./gradlew` does not
exist in a fresh checkout. Either bootstrap it once:

```bash
cd mobile/android && gradle wrapper --gradle-version 9.6
```

or install Gradle 9.6+ and call `gradle`. The scripts try `gradlew`/`gradlew.bat` first, then
`gradle`, and otherwise fail with that instruction. CI supplies Gradle through
`gradle/actions/setup-gradle`.

### iOS

```bash
scripts/mobile/build-ios.sh                 # release static libs + xcframework
cd mobile/ios
xcodegen generate
xcodebuild -project Tiny.xcodeproj -scheme Tiny \
  -destination 'generic/platform=iOS Simulator' \
  CODE_SIGNING_ALLOWED=NO build
```

`build-ios.sh` runs three `cargo build --lib --features mobile` (device, arm64 sim, x86_64 sim),
`lipo`s the two simulator slices into one, and calls `xcodebuild -create-xcframework` into
`mobile/ios/TinyCore.xcframework`. Two simulator slices cannot be passed to
`-create-xcframework` separately: it refuses two libraries with the same platform+variant.

### Demo data for a desktop check

```powershell
.\scripts\mobile\seed-demo.ps1              # live demo, %TEMP%\tinyclient-demo
.\scripts\mobile\seed-demo.ps1 -Mode many
.\scripts\mobile\seed-demo.ps1 -NoStart     # print the env vars only
```

It builds `chatd`, sets `TINYCLIENT_DIR` for the child only (so the real store is untouched —
`chatd` refuses to seed without it), waits for `chatd listening`, and prints the two commands to
run `tinyclient` against it.

---

## 4. What is proven, and where

Run on this Windows machine, Rust stable 1.99.1:

| Check | Result |
|---|---|
| `cargo check -p tinyclient --features gui` | **passes** (9 pre-existing dead-code warnings in `gui/call/permission.rs`) |
| `cargo check -p tinyclient --features "gui mobile"` | **passes** — the desktop arms compile with the feature on; the `cfg(target_os = "ios"/"android")` entry points are *not* type-checked on Windows |
| `cargo test -p tinyclient` | **passes**, 17 tests |
| `rustup target add aarch64-linux-android` + `cargo check --target aarch64-linux-android -p tinyclient --features mobile` | **fails, as expected** — see below |
| YAML (`mobile.yml`, `project.yml`), XML (`AndroidManifest.xml`, `Info.plist`) | parse cleanly (PyYAML / minidom) |
| `build-android.ps1`, `seed-demo.ps1` | PowerShell parser: clean |
| `build-android.sh`, `build-ios.sh` | `bash -n`: clean |

TLS on Android is rustls. `native-tls` on Android means OpenSSL, which is not cross-built here, so
the Android build uses rustls wherever the desktop uses the system TLS: `chatd` (`Cargo.toml`
target tables, `src/tls.rs`), and tinyclient's Wikipedia client (`ureq` under
`cfg(target_os = "android")`, `gui/conversation/wiki.rs`). `cargo ndk -t arm64-v8a check -p chatd
--no-default-features` passes with this. Before this change the Android check stopped at
`openssl-sys 0.9.117` through `native-tls ← ureq ← tinyclient`.

Behind that, from reading the pin (`Cargo.toml`: Zed `cb13b37`): `gpui_platform` has arms for
macOS, Windows, Linux/FreeBSD and wasm only, and the pin has no `gpui_ios`/`gpui_android` crate.
An Android build would fail there next, and no amount of Cargo configuration fixes that. A
platform backend exists off the pin: `gpui_ce_mobile` 0.3.0 on crates.io is an Android backend
for `gpui-ce` 0.3.0. Adopting it is the gpui-ce migration, which this branch has not made. Its
Java half (`dev.gpui.mobile.GpuiPlatformView`, `GpuiPathPicker`) is not in the crate and is not
vendored either.

**Not run at all** (so not claimed): a Gradle configuration parse or `assembleDebug`; any APK; any
`cargo ndk` invocation; anything on macOS; any iOS compile, link, archive or simulator run. There
is no NDK, no Android SDK, no `adb` and no Java-to-NDK toolchain wired up on this machine, and no
Mac. `gradle tasks` was unavailable (no Gradle installed), so `build.gradle.kts` is unvalidated
beyond review.

## 5. Feature gating — what is and is not done

Done, at the feature-graph level:

* `mobile = ["gui", "dep:android-activity"]`. It deliberately does **not** include `gui-shot`
  (GPUI test support + `render_to_image`, a desktop harness) or anything else optional.
* The Android dependency is target-gated, so a desktop build never links `android-activity`.
* The library re-declares `gui`'s modules only for mobile targets, so a desktop
  `cargo check --features gui` does not compile the window twice.

Not done, on purpose: the in-file `#[cfg]`s for the desktop-only surfaces — `gui/chrome.rs`,
`gui/app/menubar.rs`, the Settings second window, `src/update.rs`, `drags_window`, the web
sign-in hand-off. `gui/` is owned by another agent in this branch, and none of those gates can be
verified from Windows anyway (the compiler only demands them when the mobile backend exists).
They are listed with file:line in `20-implementation-map.md` §5.4 and must land in the same
change as the gpui-ce pin. Until then, `crate::update::…` is compiled into the mobile library; it
is std-only Rust, so it *compiles*, it just has no job on a phone (the store owns updates).

---

## 6. Store and platform deadlines

| Deadline | What | State here |
|---|---|---|
| **2026-08-31** (in force) | Play requires `targetSdk` 36 for new apps and updates | set: `targetSdk = 36`, `compileSdk = 36` |
| **2027-02-01** | Play blocks *updates* whose 64-bit native code does not support 16 KB pages | configured (NDK r30 default alignment, `useLegacyPackaging = false`, CI ELF check) but **not verified on a 16 KB device** |
| **2026-04-28** (in force) | App Store uploads need the iOS 26 SDK | `macos-26` runner has Xcode 26.6 |
| **April 2027** | App Store uploads need the iOS 27 SDK (Xcode 27) | move the iOS jobs to the `xcode-27` preview image (or a self-hosted Mac) in Q1 2027 |
| 2027 (rolling) | Android developer verification, global | not relevant until distribution |

16 KB specifics: every `PT_LOAD` in `libtinyclient.so` must be `0x4000`-aligned, and the APK must
zip-align native libraries to 16 KB. Verify, do not assume:

```bash
llvm-readelf -lW .../arm64-v8a/libtinyclient.so | grep LOAD   # Align 0x4000
zipalign -c -P 16 4 app-debug.apk
adb shell getconf PAGE_SIZE                                   # 16384 on a 16 KB image
```

A misaligned build segfaults at load on a 16 KB device and looks perfect on 4 KB hardware, so
this needs a 16 KB emulator image or device.

---

## 7. Known gaps

1. **No mobile backend on the pin.** The Zed pin has no `gpui_ios`/`gpui_android`;
   `ios_main`/`android_main` are seams that call `gui::run()`, which calls
   `gpui_platform::application()`. `gpui_ce_mobile` 0.3.0 (gpui-ce) is an Android backend that is
   not adopted yet, and its Java classes must come with it (see "Feature gating" above).
   Phones open links and files through GPUI (`gui::links_through_gpui()`). chatd runs in the
   app on phones (`chatd::start_in_process`, `tinyclient::inprocess`): the `mobile` build
   registers it with `protocol::launch::register_in_process`, so the shell's start path runs
   the core on a thread instead of looking for a binary. Without `mobile`, phones run with no
   chatd (`gui::mobile_core_available()` is false there).
2. **Android sign-ins** are a file in the app's data directory (`chatd::session::app_file`,
   mode 0600, values base64-encoded, not encrypted), not a keystore. Private to the app's
   sandbox. Not run on a device.
3. **No NDK/SDK/adb here**, so Android stops at the compiler and nothing is packaged.
4. **No Mac here**, so iOS is unbuilt and the Swift code is review-only. The first thing a Mac
   or CI run will hit is `gpui_platform` on Android and the same on iOS, before Xcode sees the
   library.
5. **Not signed, not uploaded.** No keystore, no Apple team, no `ExportOptions.plist`, no
   `PrivacyInfo.xcprivacy`.
6. **`ITSAppUsesNonExemptEncryption` is a placeholder** (`true`, no compliance code). An E2EE
   messenger is normally non-exempt; App Store Connect rejects the upload until
   `ITSEncryptionExportComplianceCode` is filled in.
7. **Gradle wrapper jar missing**, as above.
8. **`android_main` ignores its `AndroidApp`**, and the iOS Swift call site assumes the Rust layer
   does not own `UIApplicationMain`. Both are decisions the gpui-ce spike makes; they are marked
   `UNVERIFIED SEAM` in the source.
