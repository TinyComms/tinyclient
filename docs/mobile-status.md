# Mobile v1 status — phone shell and build scaffolding

Date: 2026-10-31. Branch: `feat/mobile-shell` (also carries `feat/mobile-build`).
Host: Windows, Rust stable 1.99.1, no Android SDK/NDK, no Gradle, no Mac, no device.

This is a UAT record, not a release note. It separates what was executed on this host from
what was only read, and it names the defects the screenshots show. The v1 scope and acceptance
criteria are in `research_notes/mobile/product/BRIEF.md`.

---

## 1. Verified / unverified matrix

| Check | Command | Result |
|---|---|---|
| Unit tests, no window | `cargo test -p tinyclient` | **pass**: `17 passed; 0 failed` |
| Unit tests, window | `cargo test -p tinyclient --features gui` | **pass**: `381 passed; 0 failed; 1 ignored` |
| Screenshot build | `cargo build -p tinyclient --features gui-shot` | **pass**, `Finished dev profile` |
| Clippy, soft | `cargo clippy -p tinyclient --features gui` | **pass** (`EXIT:0`), 22 warnings: 1 `protocol`, 21 `tinyclient`; **0 in `gui/phone`** |
| Clippy, strict | `cargo clippy -p tinyclient --features gui -- -D warnings` | **fails**, pre-existing `clippy::large_enum_variant` in `crates/protocol/src/play.rs:73` (not changed here) |
| Host mobile feature check | `cargo check -p tinyclient --features "gui mobile"` | **pass**, `Finished dev profile` |
| Workflow YAML | PyYAML load of `mobile.yml`, `ci.yml`, `project.yml` | **pass** |
| Android manifest / iOS plist | `xml.dom.minidom` parse | **pass** |
| Gradle properties | key/value parse of `gradle.properties`, `gradle-wrapper.properties` | **pass** |
| Gradle Kotlin DSL | brace/paren/bracket balance of 3 `.kts` files | **balanced**; not a semantic Gradle parse (no Gradle on this host) |
| Shell scripts | `bash -n scripts/mobile/build-android.sh`, `build-ios.sh` | **pass** |
| PowerShell scripts | AST parse of `build-android.ps1`, `seed-demo.ps1` | **pass** |
| Script guard paths | `seed-demo.ps1 -NoStart -Dir …`; `build-android.ps1 -SkipGradle` | **pass** / expected early failure: `Rust targets not installed: x86_64-linux-android` |
| 390 px render | 12 shots, dimensions read with System.Drawing | **390×844** for every phone shot; desktop **1120×720**, **600×720** |
| Desktop boundary | `--size 600x720` and `1120x720` | both draw the desktop (rail, no tab bar); `phone()` is false at 600 |
| Empty first run | fresh `TINYCLIENT_DIR` + `chatd` | `uat-welcome-390.png`, sign-in list fits |
| 200 % text | `--text-scale 2` | inbox and a scrolled conversation render; row text wraps/truncates, no horizontal overflow after the fix below |

**Not verified, and not claimed:** any Android or iOS compile of the actual `gui` tree; any
APK, IPA or simulator run; Gradle `assembleDebug`; `cargo ndk`; the Swift seam; signing or
upload; push, pairing, offline send, read-marking, frame rate, cold start, memory; touch
gestures or keyboard insets. There is no macOS, Android SDK, NDK, `adb`, Gradle or physical
device on this host. The known Android cross-compile stop is `openssl-sys` →
`native-tls` → `ureq` (documented in `docs/mobile-build.md`), and the pinned GPUI revision
(`cb13b37`) has no iOS or Android platform crate at all.

---

## 2. Screenshots

All are in `reports/` (ignored by git, kept on disk). Both dimensions and content were checked.

| File | Size | What it proves |
|---|---|---|
| `uat-inbox-390.png` | 390×844 | four tabs, 64 pt rows, unread counts, day heading |
| `uat-needs-390.png` | 390×844 | Needs filter is a real subset; muted respected by the predicate/test |
| `uat-conversation-short-390.png` | 390×844 | 44 pt back, 32 pt avatar, message rows, composer dock |
| `uat-conversation-long-390.png` | 390×844 | scrolled to the newest message (`TINYCLIENT_SHOT_BOTTOM=1`), 8 new divider, channels |
| `uat-search-390.png` | 390×844 | search screen chrome and local row list |
| `uat-you-390.png` | 390×844 | account home, 64 pt settings rows |
| `uat-scope-390.png` | 390×844 | account sheet over the list, 12 pt corners, 56 pt rows |
| `uat-welcome-390.png` | 390×844 | first run with an empty store |
| `uat-text200-inbox-390.png` | 390×844 | 200 % type, 96 pt rows, no horizontal clip |
| `uat-text200-conversation-390.png` | 390×844 | 200 % conversation at the newest message |
| `uat-desktop-1120x720.png` | 1120×720 | desktop unchanged at ≥600 |
| `uat-desktop-600x720.png` | 600×720 | the boundary itself: still desktop, still no tab bar |

### Visible defects, with coordinates

| Screen | Defect | Where |
|---|---|---|
| Search | The field is inert: no focus, no input, no query state; the list is every conversation, not results; the "searched / not searched" line is missing | field `16,111`–`374,155`; heading `16,181` |
| Conversation | Message actions are hover-driven. At 200 % a toolbar (reactions, copy, edit, …) is drawn over the "New" divider because a pointer is synthesised | `uat-text200-conversation-390.png`, about `x 210–375, y 340–375` |
| Conversation | Send failure is 12 pt: `Not sent. Slow mode; wait 8 s before sending again.` at `x 8, y 407`, with `Retry` / `Delete` links at `x 310–370` — far under 44 pt and outside the 16 pt gutter | `uat-conversation-long-390.png` |
| Conversation | The poll card's second line is clipped/blank under "Poll: Where should the aut…" | `uat-conversation-long-390.png`, `x 56–370, y 300–323` |
| Welcome | `Sign in` buttons render about 34 pt tall (target floor is 44) | `uat-welcome-390.png`, e.g. `x 272–345, y 289–323` |
| First run / scope | Signal and Google Messages are offered although the brief defers both for v1; the scope sheet also lists Signal | welcome rows `y 512–565`, `y 637–691`; scope `y 515` |
| 200 % nav | The 34 pt title fills a 52 pt strip; at 300 % it would spill. It fits at 200 % but has no headroom | `uat-text200-inbox-390.png`, `y 44–96` |
| All lists | A row scrolled past the top could paint over the nav strip (seen once at 200 %); fixed below and re-shot | n/a after fix |

No crash, blank frame, wrong size or desktop regression was observed. The app connected to a
fresh `chatd --seed-demo=live`; the "first list" log line was 530–700 ms on this desktop host,
which says nothing about a phone cold start.

---

## 3. Fixes made this pass

1. **Scroller clipping (phone only).** The inbox and search scrollers now sit in an
   `overflow_hidden`, `min_w_0`, `flex_col` wrapper. A row scrolled above the viewport can no
   longer paint over the 52 pt nav strip at 200 %. The first attempt used a row wrapper and
   made 200 % rows wider than 390; that was corrected and both screens were re-shot.
   Files: `crates/tinyclient/src/gui/phone/inbox.rs`, `search.rs`.
2. **iOS floor aligned with the brief.** `mobile/ios/project.yml` and its README said iOS 17.0;
   the v1 brief requires iOS 16+. `deploymentTarget` and `IPHONEOS_DEPLOYMENT_TARGET` are now
   `16.0`. The Swift shell uses only `UIWindowScene`/`UISceneSession` (iOS 13+), so nothing in
   the skeleton needed to change. Still unverified until it links on a Mac/CI.
3. **New clippy warnings removed.** The doc-list continuation in `crates/tinyclient/src/lib.rs`
   was over-indented; that was 2 of the new warnings. The phone module contributes none.
4. **Repo hygiene.** 44 generated PNGs were untracked with `git rm --cached`; `/reports/` is in
   `.gitignore`; the files remain on disk.
5. This document.

---

## 4. Reproduce

```powershell
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
cd C:\Users\gollum\Development\Personal\tiny\_wt-mobile-shell

cargo test -p tinyclient
cargo test -p tinyclient --features gui
cargo build -p tinyclient --features gui-shot
cargo clippy -p tinyclient --features gui            # 22 warnings, none in gui/phone
cargo check -p tinyclient --features "gui mobile"

python -c "import yaml; [yaml.safe_load(open(p, encoding='utf-8')) for p in ['.github/workflows/mobile.yml','.github/workflows/ci.yml','mobile/ios/project.yml']]"
python -c "import xml.dom.minidom as m; [m.parse(p) for p in ['mobile/android/app/src/main/AndroidManifest.xml','mobile/ios/Info.plist']]"
bash -n scripts/mobile/build-android.sh
bash -n scripts/mobile/build-ios.sh
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/mobile/seed-demo.ps1 -NoStart
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/mobile/build-android.ps1 -SkipGradle
```

Seed a live demo and shoot the phone (the daemon must already be listening; `--shot` does not
start one unless `--start-chatd` is passed):

```powershell
$env:TINYCLIENT_DIR = "$env:TEMP\tinyclient-uat"
target\debug\chatd.exe --seed-demo=live            # in another shell

target\debug\tinyclient.exe --shot reports\uat-inbox-390.png --size 390x844 --tab inbox
target\debug\tinyclient.exe --shot reports\uat-needs-390.png --size 390x844 --tab needs
target\debug\tinyclient.exe --shot reports\uat-scope-390.png --size 390x844 --tab inbox --scope
target\debug\tinyclient.exe --shot reports\uat-you-390.png --size 390x844 --tab you
target\debug\tinyclient.exe --shot reports\uat-search-390.png --size 390x844 --tab search
target\debug\tinyclient.exe --shot reports\uat-conversation-short-390.png --size 390x844 --tab inbox --open 1
$env:TINYCLIENT_SHOT_BOTTOM = "1"
target\debug\tinyclient.exe --shot reports\uat-conversation-long-390.png --size 390x844 --tab inbox --open 0
target\debug\tinyclient.exe --shot reports\uat-text200-conversation-390.png --size 390x844 --tab inbox --open 3 --text-scale 2
Remove-Item Env:TINYCLIENT_SHOT_BOTTOM
target\debug\tinyclient.exe --shot reports\uat-text200-inbox-390.png --size 390x844 --tab inbox --text-scale 2
target\debug\tinyclient.exe --shot reports\uat-desktop-1120x720.png --size 1120x720
target\debug\tinyclient.exe --shot reports\uat-desktop-600x720.png --size 600x720
```

Empty first run: point `TINYCLIENT_DIR` at a fresh directory, start `chatd` there (no
`--seed-demo`), and shoot with no `--tab`.

---

## 5. Known gaps, by severity

| # | Gap | Severity | Evidence / next owner |
|---|---|---|---|
| 1 | **No mobile platform backend.** The GPUI pin has no iOS/Android crate; `ios_main`/`android_main` call `gpui_platform::application()`, which has no mobile arm. Nothing links or runs. | **Blocker** | `20-implementation-map.md` §4; `docs/mobile-build.md` §7.1 |
| 2 | **Android link stops at OpenSSL.** `ureq`/`native-tls` pulls `openssl-sys`; the cross-check dies before GPUI. Needs rustls + platform verifier, not a flag. | **Blocker** | `docs/mobile-build.md` §4 |
| 3 | **No toolchain here.** No NDK r30, SDK 36, Gradle 9.6, `adb`, or device; no Mac/Xcode. No APK/IPA exists. | **Blocker (environmental)** | this host |
| 4 | **Keyboard/IME inset is unreachable at the pin.** `WindowInsets` exists but `Window` exposes no getter; the composer cannot reflow above the keyboard. Safe areas are hard-coded 44/34 pt, not read. | **Critical** | `tokens.rs` comments; map §3.3/§3.4 |
| 5 | **Search is not a search.** The field is static; local search and the honest "what was not searched" readout are absent. | High | `phone/search.rs`; brief Must #5 |
| 6 | **Message actions are hover-only.** No long-press sheet; reactions/edit/delete/copy are unreachable by touch, and a synthesized hover draws a toolbar over content (seen at 200 %). | High | `conversation/actions.rs`; map §3.2 |
| 7 | **44 pt target floor is not universal.** First-run `Sign in` (~34), send-failure `Retry`/`Delete` (~16), several strip buttons; the brief's accessibility floor is 44/48. | High | welcome + long-conversation shots |
| 8 | **Back is not a real gesture.** Edge drag uses mouse move/down, not `TouchEvent`; `--back` still drives desktop history, not `phone_back`. No Android back hook. | Medium | `phone/mod.rs`; map §3.5 |
| 9 | **Text size is a global zoom, not dynamic type.** `--text-scale` is an atomic used by phone tokens; it does not follow the OS setting, and at 300 % the nav strip is too short. | Medium | `phone/tokens.rs`; shot at 300 % during UAT |
| 10 | **v1 scope leakage.** Signal and Google Messages appear in first run/demo data though the brief defers them; the demo includes a Telegram login code in a preview (local only, but must never reach a relay). | Medium | welcome shot; brief "Networks" / constraints |
| 11 | **Settings is still a desktop second window** from the phone `You` rows (720×560, `WindowOptions`). It happens to open on this host, but it is not a phone route. | Medium | `phone/you.rs`; `settings/mod.rs:32,219` |
| 12 | **iOS is unbuilt and unsigned.** Floor fixed to 16.0 but unproven; `ITSAppUsesNonExemptEncryption` is a placeholder; no team, keystore or export plist. | Medium | `mobile/ios/`; `docs/mobile-build.md` §6–7 |
| 13 | **Gradle wrapper jar absent.** A fresh checkout has no `gradlew`; CI supplies Gradle, local users must bootstrap it. | Low | `docs/mobile-build.md` §3 |
| 14 | **Demo banner copy.** "Not sent. Slow mode…" starts at the 8 pt edge with tiny links; it should use the 16 pt gutter and a 44 pt retry control. | Low | long-conversation shot, `x 8, y 407` |
| 15 | **Performance budgets unmeasured.** No cold start, frame time or RSS number exists on a phone. | Unknown (cannot claim) | brief "Budgets" |

Pre-existing, not caused by this branch: clippy `large_enum_variant` in `protocol::play`;
dead-code/unused-import warnings in `gui/call/permission.rs`; deprecated `fetch_update` in
`gui/anim.rs` and `composer.rs`.

---

## 6. Next three steps for a real device build

1. **Make Android compile.** Install NDK r30 (`30.0.16248370`), SDK platform 36, JDK 17+,
   Gradle 9.6 and `cargo-ndk 4.1.2`; run
   `.\scripts\mobile\build-android.ps1 -SkipGradle`. The first failure will be
   `openssl-sys`; replace `ureq`'s native-tls path with rustls + `rustls-platform-verifier`
   and repeat until `libtinyclient.so` is produced for arm64-v8a and x86_64.
2. **Move the framework.** Take gpui-ce 0.3 (or gpui-pre + mobile) so an iOS/Android platform
   backend exists; gate `chrome.rs`, the menu bar, the Settings second window, `drags_window`
   and `gui-shot` for desktop, then prove `cargo check --target aarch64-linux-android
   -p tinyclient --features mobile` and `gradle :app:assembleDebug` produce an APK.
3. **Run it on hardware.** Install on a physical Android 10+ device (and a 16 KB-page emulator),
   type in the composer with the keyboard up, verify the inset reflow, the 44 pt targets, the
   back gesture, 200 % text, then measure cold start / frame stats / RSS against the brief.
   Only after that does an iOS run on a Mac or the `macos-26` CI job mean anything.

Until step 1 and 2 are green, "mobile v1" is a verified 390 px design render and a build
skeleton — not an app that can be installed on a phone.
