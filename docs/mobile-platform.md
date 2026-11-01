# Mobile platform interfaces

A phone runs chatd and mediad inside the app (`chatd::start_in_process`, `mediad::inproc`) and
cannot start helper binaries. Three things a desktop gets from separate processes or desktop APIs
are therefore plugged in by the phone at startup. This page is the contract for the Android and
iOS work: what each side registers, which thread calls what, what may block, and the frame
formats.

| Hook | Defined in | Registered by | Used by |
|---|---|---|---|
| mediad in process | `protocol::media_host` | `crate::mobile::register` (`mediad::inproc::start`, done) | chatd calls, `--play`, `--sound`, `test`; Settings' device list |
| Call platform | `mediad::platform::Platform` | `crate::mobile::register` (stub `Phone`) | mediad: permissions, audio session, camera, screen, H.264 |
| Web sign-in | `protocol::web_sign_in::WebSignIn` | `crate::mobile::register` (stub `Phone`) | chatd: Discord, Slack, Teams sign-in; a Tiny server's OIDC page |

`crates/tinyclient/src/mobile.rs` registers all three from `android_main` and `ios_main`,
before chatd starts. The platform agents replace the bodies of its `Phone` stubs (or register
their own types there) and touch nothing else in the core. Desktop builds register nothing and
behave exactly as before.

## 1. mediad in process (`protocol::media_host`)

Done; nothing for the platforms to write. Every place that started the mediad binary now calls
`protocol::media_host::spawn(&mut command)` (or `output` for `--devices`). With no starter
registered that is `command.stdin(piped).stdout(piped).spawn()`, unchanged. With
`mediad::inproc::start` registered, the command's arguments and environment go to a mediad on
threads of the app, and the same length-prefixed frames travel over an in-memory pipe.

Process-wide behaviour is contained per mediad (`mediad::out`):

* `exit(code)` ends that mediad (its stdout closes, chatd reads `code`), not the app.
* A panic on any of its threads ends that mediad with code 101 and is logged; the app goes on.
  This needs `panic = "unwind"` (the default); do not set `panic = "abort"` for mobile profiles.
* Environment variables are read from the command first (`out::var`), so two mediads in one
  process can differ (the tests run one in process beside one process).
* Killing it closes its input; its threads see `out::over()` and stop.
* No logging setup, panic hook, Info.plist or signal handling: the app already has its own.

The watched video picture is written as a JPEG under `protocol::support_dir()/pictures` on a
phone (Android's temp dir is not writable by apps), and its path is sent to the shell as on a
desktop.

## 2. Call platform (`mediad::platform`)

```rust
pub trait Platform: Send + Sync {
    fn request_permission(&self, permission: Permission) -> Result<(), String>; // Microphone | Camera
    fn start_audio_session(&self) -> Result<(), String>;
    fn stop_audio_session(&self);
    fn set_speaker(&self, on: bool) -> Result<(), String>;
    fn start_camera(&self, request: CameraRequest, sink: FrameSink) -> Result<(), String>;
    fn stop_camera(&self);
    fn start_screen_share(&self, sink: FrameSink) -> Result<(), String>;
    fn stop_screen_share(&self);
    fn h264_encoder(&self, config: EncoderConfig) -> Result<Box<dyn H264Encoder>, String>;
    fn h264_decoder(&self) -> Result<Box<dyn H264Decoder>, String>;
}
pub trait H264Encoder: Send { fn encode(&mut self, frame: &VideoFrame, keyframe: bool) -> Result<Vec<u8>, String>; }
pub trait H264Decoder: Send { fn decode(&mut self, annex_b: &[u8]) -> Result<Option<VideoFrame>, String>; }
pub type FrameSink = Arc<dyn Fn(VideoFrame) + Send + Sync>;
```

Words in an `Err` are shown to the user as they are: write a sentence.

### When mediad calls it

| Call | When | Thread | Blocks? |
|---|---|---|---|
| `request_permission(Microphone)` | each call's start, before devices open (`permission::microphone`) | the call's loop thread | yes, until the user answers |
| `request_permission(Camera)` | camera turned on (`probe_camera`), and when the capture starts | chatd request thread / call loop | yes |
| `start_audio_session` / `stop_audio_session` | start and end of a call's loop (Discord or Tiny), however it ends | the call's loop thread | briefly |
| `set_speaker` | not called by mediad yet (see open questions) | — | — |
| `start_camera` / `stop_camera` | camera on, at the first frame wanted; off within one tick (66 ms) of the button | the call's loop thread | `start` until capture runs or fails |
| `start_screen_share` / `stop_screen_share` | as the camera, for a share | the call's loop thread | `start` until running or refused |
| `h264_encoder` | first frame of a layer, and when its size changes | call loop | briefly |
| `encode` | 15 times a second per layer (share: 1; camera: 2) | call loop | must return within ~60 ms |
| `h264_decoder` / `decode` | the first watched frame / each watched frame, at most 10 a second | call loop | within ~60 ms |

Never call back into mediad from these methods, and hop to your own main thread yourself
(`DispatchQueue.main.sync`, `runOnUiThread` + latch) when an API needs it: mediad's threads are
never a platform UI thread.

### What each platform implements

* **Audio session.** iOS: `AVAudioSession` category `.playAndRecord`, mode `.voiceChat`
  (options `.allowBluetooth`, `.defaultToSpeaker` for video), `setActive(true)`; deactivate with
  `.notifyOthersOnDeactivation`. Android: audio focus `AUDIOFOCUS_GAIN_TRANSIENT`,
  `AudioManager.mode = MODE_IN_COMMUNICATION`, then `MODE_NORMAL` and abandon focus. Route:
  `overrideOutputAudioPort(.speaker / .none)` / `setCommunicationDevice` (API 31+) or
  `isSpeakerphoneOn`. cpal opens AAudio / CoreAudio itself after `start_audio_session`.
* **Permissions.** `RECORD_AUDIO`, `CAMERA` (Android runtime permission via the activity);
  `AVCaptureDevice.requestAccess(for: .audio/.video)` on iOS. `Info.plist` needs
  `NSMicrophoneUsageDescription`, `NSCameraUsageDescription`.
* **Camera.** `CameraRequest { facing, width: 640, height: 360, fps: 15 }`; deliver the nearest
  size you have, any aspect; mediad fits and scales to 640×360 and 320×180 layers. Android
  CameraX/Camera2 `ImageAnalysis` YUV_420_888; iOS `AVCaptureVideoDataOutput` with
  `kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange`. Rotate to upright before delivering.
* **Screen share.** Android `MediaProjection` + `ImageReader` (RGBA_8888); iOS ReplayKit
  (`RPScreenRecorder.startCapture` in app, or a Broadcast Upload Extension for system-wide).
  Any size; mediad fits it inside 1280×720.
* **H.264.** Constrained baseline, level 3.1, Annex B, no B-frames, SPS/PPS before every IDR,
  one slice per frame preferred, `keyframe == true` forces an IDR now. Sizes and rates mediad
  asks for: share up to 1280×720 at 1.5 Mbit/s (`screen: true`), camera 640×360 at 500 kbit/s
  and 320×180 at 150 kbit/s, all 15 fps. Android `MediaCodec` `video/avc` with
  `COLOR_FormatYUV420SemiPlanar`, `KEY_PROFILE = AVCProfileConstrainedBaseline`; iOS
  `VTCompressionSession` with `kVTProfileLevel_H264_ConstrainedBaseline_AutoLevel`,
  `kVTCompressionPropertyKey_RealTime`, converting AVCC to Annex B. Use them synchronously
  (dequeue with a short timeout / `VTCompressionSessionCompleteFrames`); returning an empty
  vector means "nothing out yet" and that tick sends nothing. Decoders likewise:
  `MediaCodec` or `VTDecompressionSession`, `Ok(None)` until a picture comes out.

### Frames (`VideoFrame`)

```rust
pub struct VideoFrame { width: u32, height: u32, format: PixelFormat, data: Vec<u8>, timestamp_us: u64 }
pub enum PixelFormat { I420, Nv12, Rgba, Bgra }
```

Always tightly packed: no row padding (copy rows out of a strided buffer), top-down rows, even
width and height for the YUV formats. `I420` is Y, then U, then V; `Nv12` is Y, then interleaved
CbCr. A frame whose `data` length does not match (`VideoFrame::is_whole`) is dropped.

* Sinks take any format. Phones should send `Nv12` from cameras and `Rgba`/`Bgra` from screens.
* Encoders always get `Nv12` at exactly the configured size.
* Decoders return any format; `Nv12` or `I420` is natural.
* A `FrameSink` may be called from any thread, as often as frames come. It stores the newest
  frame and returns at once; it never blocks.

### Test platform

`mediad::platform::fake::FakePlatform`: a camera and screen that paint moving NV12 stripes, a
passthrough codec (the frame behind an IDR-looking header, not real H.264), and a `Log` of what
it was asked. Host tests drive the phone picture path (`picture::phone`) with it.

## 3. Web sign-in (`protocol::web_sign_in`)

```rust
pub trait WebSignIn: Send + Sync {
    fn open_page(&self, page: WebPage) -> Result<Box<dyn WebPageHandle>, String>;
    fn open_auth(&self, auth: AuthRequest) -> Result<Box<dyn AuthHandle>, String>;
}
pub struct WebPage { network, title, url, user_agent, init_script: Option<String>,
                     navigate: Arc<dyn Fn(&str) -> Navigation + Send + Sync> }
pub enum Navigation { Allow, Cancel, Load(String) }
pub trait WebPageHandle: Send {
    fn state(&mut self) -> PageState;            // { url, closed, load_failed }
    fn evaluate(&mut self, script: &str) -> Option<String>;   // JSON text of the result
    fn cookie(&mut self, domain: &str, name: &str) -> Option<String>;
    fn load(&mut self, url: &str);
    fn close(&mut self);
}
pub struct AuthRequest { url, callback, redirected: Arc<dyn Fn(String) + Send + Sync> }
pub trait AuthHandle: Send { fn closed(&mut self) -> bool; fn close(&mut self); }
```

All decisions stay in Rust: `login::web::sign_in` (the same logic as the desktop helper,
`crates/login`) chooses the page, the navigation policy, the probes and the session line;
chatd keeps the session exactly as the helper's printed line. The platform only shows pages.

### Discord, Slack, Teams: an in-app web view (`open_page`)

These need a web view whose storage and cookies can be read (`localStorage` token, Slack's
HttpOnly `d` cookie), so: Android `android.webkit.WebView`, iOS `WKWebView`, with a persistent
data store per `WebPage::network` (a second sign-in resumes the first).

* Show `page.url` with `page.user_agent` as the User-Agent (the session records it; chatd keeps
  using it). Run `init_script` at document start if `Some` (none is used yet).
* For every main-frame navigation and every new-window request, call `page.navigate(url)`
  before it happens and obey: `Allow` (for a new window: load it in this page), `Cancel`, or
  `Load(other)` (cancel and load `other`). It returns at once; call it on your UI thread
  (`shouldOverrideUrlLoading` / `decidePolicyFor navigationAction`, `onCreateWindow` /
  `createWebViewWith`). Teams' redirect is cancelled this way and read in Rust.
* `state()`: current URL, whether the user closed the page, whether the last load failed for
  want of a network (no connection, DNS, TLS, timeout; not 4xx/5xx).
* `evaluate(script)`: run in the main frame, answer the result as JSON text (Android's
  `evaluateJavascript` already does; iOS JSON-encodes what `evaluateJavaScript` returns),
  `None` on an exception or after ~5 s.
* `cookie(domain, name)`: from the page's own store, HttpOnly included (`CookieManager`,
  `WKHTTPCookieStore.getAllCookies`); `domain` matches its subdomains.
* `close()`: dismiss. Called once at the end whatever happened.

chatd calls these from a request thread and polls once a second; every method may block briefly
while you hop to the UI thread. Results map to the helper's exit codes and the same messages:
closed (1), unreachable (3), blocked by the organization (4), Slack with no token (5), Teams
refused (6).

### A Tiny server: the system authentication session (`open_auth`)

A Tiny server signs in with its OIDC provider (Pocket ID, passkeys). Passkeys do not work in a
plain WKWebView or `android.webkit.WebView`, so this uses the system's session: iOS
`ASWebAuthenticationSession` (`prefersEphemeralWebBrowserSession = false`), Android Custom
Tabs (Chrome's, with Credential Manager passkeys).

The robust path, chosen here: **chatd's loopback listener stays the only receiver.** chatd binds
`http://127.0.0.1:<port>/callback`, registers it with the server (`POST /v1/auth/start`), and
calls `open_auth { url, callback }`. The provider's page redirects the session's browser to that
loopback address and chatd, running in the foregrounded app, receives it as on a desktop; the
redirect URI the server knows does not change, and nothing about the flow is phone-specific.

* Show `auth.url` and return at once. On iOS give `ASWebAuthenticationSession` a callback scheme
  you never expect (the loopback redirect is plain `http`); the session is closed by chatd.
* If your session does see the navigation to `auth.callback` (one that cannot reach loopback,
  or one that stops there), pass the whole URL to `auth.redirected(url)`, from any thread. chatd
  delivers it to its own listener, which checks the state as always.
* `close()` is called when the sign-in finished, failed or was cancelled. Android cannot close a
  Custom Tab from outside: bring the app's activity back to the front instead.
* `closed()` reports a user dismissal (not yet consulted; the sheet's "Open the page again"
  covers it).

## Android, as built

`crates/tinyclient/src/mobile/android` (Rust, JNI through `jni` 0.21 and `ndk-context`) over
small Java classes in `mobile/android/app/src/main/java/dev/tinyclient/app`. Rust finds them
through the activity's class loader (a thread attached from Rust sees only system classes), runs
every call in its own local-reference frame, and turns a Java exception into an `Err`. Java calls
back through three natives (`TinyPlatform.nativeFrame`, `nativeNavigate`, `nativeRedirected`),
which name Rust callbacks by number and catch panics.

| Interface | Java | What it does |
|---|---|---|
| `request_permission` | `TinyPlatform.requestPermission` via `TinyActivity` | `requestPermissions` on the UI thread, blocks on a latch until `onRequestPermissionsResult`. The microphone request also asks `POST_NOTIFICATIONS` (33+) and `BLUETOOTH_CONNECT` (31+). Refused for good (no rationale) says "Turn it on in Settings"; the call screen's state is noted (`hooks::note_permission`). |
| Open Settings (`hooks`) | `TinyPlatform.openSettings` | `ACTION_APPLICATION_DETAILS_SETTINGS` for this package. |
| `start/stop_audio_session` | `CallAudio`, `CallService` | `AUDIOFOCUS_GAIN_TRANSIENT` (voice communication), `MODE_IN_COMMUNICATION`, the route; a foreground service (`microphone`, plus `camera` / `mediaProjection` while used) with an ongoing call notification. Stop: route cleared, the earlier mode back, focus abandoned, service stopped. |
| `set_speaker`, route menu (`hooks::route_audio`) | `CallAudio.routes/setRoute` | `getAvailableCommunicationDevices` / `setCommunicationDevice` (31+), else speakerphone and SCO. Earpiece by default; the speaker once the camera starts unless the user chose. |
| `start/stop_camera`, flip (`hooks::route_flip`) | `CameraCapture` | Camera2, `ImageReader` YUV_420_888 at the size nearest the request, AE range nearest the rate, throttled to it, copied to tight NV12 and turned upright (sensor and display rotation). `start` blocks until the first frame (6 s). The newest frame also feeds the call screen's own tile (`selfie.rs`, `hooks::self_frame`, mirrored for the front camera). |
| `start/stop_screen_share` | `ScreenCapture` | The consent dialog (`createScreenCaptureIntent`), then the service's `mediaProjection` type, then `getMediaProjection`, a `VirtualDisplay` into an RGBA `ImageReader` at most 1280 on the long side, 15 fps, tight RGBA to the sink. |
| `h264_encoder` | `AvcEncoder` | `MediaCodec` `video/avc`, `COLOR_FormatYUV420Flexible` filled through `getInputImage`, constrained baseline (baseline, then the codec's default if refused) level 3.1, `KEY_PREPEND_HEADER_TO_SYNC_FRAMES`, no B-frames; `PARAMETER_KEY_REQUEST_SYNC_FRAME` for `keyframe`; SPS/PPS put in front of an IDR that lacks them. 20 ms input / 30 ms output waits. |
| `h264_decoder` | `AvcDecoder` | `MediaCodec` decoder, waits for an SPS, newest output picture as tight I420 (crop rect honoured). |
| `open_page` | `WebSession`, `WebSignInActivity` | A WebView activity with a Cancel bar. JavaScript, DOM storage, third-party cookies, the page's user agent. The first URL, every main-frame navigation and every new window (caught in a throwaway view) go through `navigate`. `state`, `evaluate` (5 s), `cookie` (`CookieManager`, HttpOnly included, the domain plus `www.`/`app.`), `load`, `close`. One WebView profile for the app, so cookies persist across sign-ins and networks. |
| `open_auth` | `AuthSession` | A Custom Tab without androidx: `ACTION_VIEW` with the `EXTRA_SESSION` binder extra, Chrome first. chatd's loopback listener receives the redirect. `closed()` is the app coming back while the tab was open; `close()` brings `TinyActivity` back (`CLEAR_TOP|SINGLE_TOP`), which closes the tab. |

The demo build (`-Demo`) adds a call with nobody in it (`gui::shot_demo`): starting it asks for
the microphone and holds the audio session; its camera button runs mediad's real phone pictures
(`mobile/android/loopback.rs`): camera → both layers encoded with MediaCodec → the 640×360 layer
decoded with MediaCodec → shown as the other side's picture, next to my own tile.

## Threading summary

* chatd and mediad call the platform from their own threads and may block there. The platform
  hops to its UI thread itself.
* Callbacks into Rust (`WebPage::navigate`, `AuthRequest::redirected`, `FrameSink`) are called
  from any platform thread, are quick, and never call back into the platform synchronously.
* Nothing in the core runs on the platform's UI thread, and the core never assumes one.

## iOS (`crates/tinyclient/src/mobile/ios`)

Registered by `crate::mobile::register` on iOS instead of the stub. All through objc2; no Swift.

| Piece | File | How |
|---|---|---|
| Permissions | `audio.rs` | `AVCaptureDevice.authorizationStatus` / `requestAccess(for:)`, the caller's thread waiting on a channel; refusals go to the call screen (`hooks::note_permission`); Settings opens `UIApplication.openSettingsURLString` |
| Audio session | `audio.rs` | `.playAndRecord`, `.voiceChat`, `.allowBluetooth`, active; `.notifyOthersOnDeactivation` at the end. Routes for the call screen (`hooks::route_audio`): earpiece or wired, speaker, Bluetooth HFP, by `overrideOutputAudioPort` and `setPreferredInput`. A camera that starts moves an unchosen route to the speaker. `UIBackgroundModes` `audio` keeps a call going in the background |
| Camera | `camera.rs` | `AVCaptureSession` (640×480 preset), front or back wide-angle camera, `AVCaptureVideoDataOutput` in `420v`, portrait, frames copied tight to NV12. Flip (`hooks::route_flip`) restarts on the other camera. No camera (the simulator) is the words "This device has no camera." |
| Screen share | `screen.rs` | `RPScreenRecorder.startCapture` (this app's screen), at most 5 frames a second to the sink. System-wide sharing needs a Broadcast Upload Extension (not built) |
| H.264 | `codec.rs` | `VTCompressionSession` (constrained baseline, real time, no reordering, keyframe on request) with AVCC turned into Annex B and SPS/PPS before each IDR; `VTDecompressionSession` made from the stream's SPS/PPS, decoding synchronously to NV12 |
| Web pages | `web.rs` | `WKWebView` in a page sheet over the window's top view controller, with a Close button; one `WKWebsiteDataStore(forIdentifier:)` per network (iOS 17+). objc2 binds `WKWebView` for macOS only, so WebKit is messaged dynamically |
| Tiny server | `auth.rs` | `ASWebAuthenticationSession`, `prefersEphemeralWebBrowserSession = false`, a callback scheme nothing uses; chatd's loopback listener receives the redirect |

Debug builds take two launch arguments for checking the platform on a phone or simulator
(`selftest.rs`): `-tinySelfTest` (permissions, session, routes, camera, H.264 round trips at
the three sizes mediad uses, ReplayKit) and `-tinyAuthTest <provider page>` (open_auth with a
loopback listener, as chatd does). Records go to the unified log and to
`Library/Application Support/tinyclient/ios-platform.log`.

## Open questions

* A speaker/earpiece control in the call UI needs a request to mediad (`MediaRequest`); until then
  the platform keeps its own default route (earpiece for voice, speaker for video).
* Screen share on iOS beyond the app needs a Broadcast Upload Extension, a separate process: its
  frames must reach the app (App Group shared memory) before the sink.
* Background audio: Android runs its call under a foreground service (above); iOS sets the `audio`
  background mode, so a call keeps playing when the app is backgrounded.
* CallKit / ConnectionService integration for incoming rings is not part of this interface.
