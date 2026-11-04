//! Linux screen and camera through xdg-desktop-portal and PipeWire.
//!
//! One path for every desktop that ships a portal: GNOME, KDE, Sway, Hyprland, and the X11
//! desktops that also run one. The session bus is spoken with zbus, which is pure Rust, so this
//! binary does not link a second libdbus beside chatd's. PipeWire is loaded by name
//! (`libpipewire-0.3.so.0`) when a capture starts. A machine without it still launches, and an
//! X11 session never reaches this module: it grabs the root window instead.
//!
//! A thumbnail or `--screens` must not open the portal. Only a share that is actually starting
//! does, and a cancel does not fall through to X11 (an XWayland root is black, and would look
//! like a picture). No session bus fails at once, and so does a seat with neither Wayland nor
//! X11. A headless runner still has a session bus, and a portal call there waits on a dialog
//! that nobody can answer.

use super::pipewire_abi::*;
use super::spa_pod::{self, Fixated};
use super::Rgba;
use libloading::{Library, Symbol};
use std::collections::HashMap;
use std::ffi::{c_void, CStr, CString};
use std::os::fd::IntoRawFd;
use std::os::raw::c_char;
use std::sync::atomic::AtomicU64;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;
use zbus::blocking::Connection;
use zbus::message::Type as MessageType;
use zbus::zvariant::{Array, OwnedFd, OwnedObjectPath, OwnedValue, Value};

const DEST: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";
const SCREENCAST: &str = "org.freedesktop.portal.ScreenCast";
const CAMERA: &str = "org.freedesktop.portal.Camera";
const REQUEST: &str = "org.freedesktop.portal.Request";
const SESSION_IFACE: &str = "org.freedesktop.portal.Session";

const NO_BUS: &str = "This session has no desktop bus, so the screen can't be shared.";
const NO_PIPEWIRE: &str = "This Wayland session has no PipeWire, so the screen can't be shared.";
const PORTAL_DOWN: &str = "This desktop's portal can't share the screen.";
const SCREEN_CANCELLED: &str = "The screen share was cancelled.";
const CAMERA_CANCELLED: &str = "The camera was cancelled.";

/// A portal answer. `refused` is the user cancelling; that must not fall through to X11.
pub(super) struct PortalError {
    pub refused: bool,
    pub words: String,
}

fn refused(words: &str) -> PortalError {
    PortalError {
        refused: true,
        words: words.to_owned(),
    }
}

fn failed(words: &str) -> PortalError {
    PortalError {
        refused: false,
        words: words.to_owned(),
    }
}

fn failed_err(words: &str, err: impl std::fmt::Display) -> PortalError {
    tracing::debug!(%err, "{words}");
    failed(words)
}

struct SlotState {
    frame: Option<Rgba>,
    error: Option<String>,
}

struct Slot {
    inner: Mutex<SlotState>,
    wake: Condvar,
}

impl Slot {
    fn new() -> Arc<Slot> {
        Arc::new(Slot {
            inner: Mutex::new(SlotState {
                frame: None,
                error: None,
            }),
            wake: Condvar::new(),
        })
    }

    fn store(&self, frame: Rgba) {
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.error = None;
        guard.frame = Some(frame);
        self.wake.notify_all();
    }

    fn fail(&self, words: String) {
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.error = Some(words);
        self.wake.notify_all();
    }

    fn latest(&self) -> Option<Rgba> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .frame
            .clone()
    }

    fn wait_frame(&self, limit: Duration) -> Result<Rgba, String> {
        let guard = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (guard, _) = self
            .wake
            .wait_timeout_while(guard, limit, |state| {
                state.frame.is_none() && state.error.is_none()
            })
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(words) = guard.error.clone() {
            return Err(words);
        }
        guard
            .frame
            .clone()
            .ok_or_else(|| "The screen did not produce a picture.".to_owned())
    }
}

struct Live {
    slot: Arc<Slot>,
    thread: Option<JoinHandle<()>>,
    loop_ptr: usize,
    conn: Connection,
    /// ScreenCast session path. A camera portal has none.
    session: Option<String>,
}

fn screen_slot() -> &'static Mutex<Option<Live>> {
    static LIVE: OnceLock<Mutex<Option<Live>>> = OnceLock::new();
    LIVE.get_or_init(|| Mutex::new(None))
}

fn camera_slot() -> &'static Mutex<Option<Live>> {
    static LIVE: OnceLock<Mutex<Option<Live>>> = OnceLock::new();
    LIVE.get_or_init(|| Mutex::new(None))
}

fn lock(slot: &'static Mutex<Option<Live>>) -> std::sync::MutexGuard<'static, Option<Live>> {
    slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The newest screen frame, if a share is already recording. Does not open a dialog.
pub(super) fn latest_screen() -> Option<Rgba> {
    lock(screen_slot())
        .as_ref()
        .and_then(|live| live.slot.latest())
}

/// Starts the screen portal if it is not up, then the newest frame. The dialog is inside the
/// lock so two captures do not ask twice.
pub(super) fn capture_screen() -> Result<Rgba, PortalError> {
    let slot = {
        let mut guard = lock(screen_slot());
        if guard.is_none() {
            *guard = Some(open_screen()?);
        }
        guard.as_ref().unwrap().slot.clone()
    };
    match slot.wait_frame(Duration::from_secs(8)) {
        Ok(frame) => Ok(frame),
        Err(words) => {
            stop_live(screen_slot());
            Err(failed(&words))
        }
    }
}

pub(super) fn stop_screen() {
    stop_live(screen_slot());
}

/// The camera portal, after V4L2 has already failed. `refused` is a cancel; anything else means
/// the caller should keep the V4L2 words.
pub(super) fn open_camera() -> Result<(), PortalError> {
    let mut guard = lock(camera_slot());
    if guard.is_some() {
        return Ok(());
    }
    *guard = Some(open_camera_live()?);
    Ok(())
}

pub(super) fn capture_camera() -> Result<Rgba, String> {
    let slot = lock(camera_slot()).as_ref().map(|live| live.slot.clone());
    let Some(slot) = slot else {
        return Err("The camera portal is not open.".to_owned());
    };
    match slot.wait_frame(Duration::from_secs(8)) {
        Ok(frame) => Ok(frame),
        Err(words) => {
            drop(slot);
            stop_live(camera_slot());
            Err(words)
        }
    }
}

pub(super) fn stop_camera() {
    stop_live(camera_slot());
}

fn stop_live(slot: &'static Mutex<Option<Live>>) {
    let live = lock(slot).take();
    let Some(mut live) = live else {
        return;
    };
    if live.loop_ptr != 0 {
        if let Ok(api) = api() {
            unsafe { (api.main_loop_quit)(live.loop_ptr as *mut c_void) };
        }
    }
    if let Some(thread) = live.thread.take() {
        let _ = thread.join();
    }
    if let Some(session) = live.session.take() {
        close_session(&live.conn, &session);
    }
}

fn open_screen() -> Result<Live, PortalError> {
    let conn = bus()?;
    let screencast = proxy(&conn, SCREENCAST)?;
    let session_token = token('s');
    let handle = token('h');
    let mut wait = Waiter::arm(&conn, &handle)?;
    let options = dict(&[
        ("handle_token", str_value(&handle)),
        ("session_handle_token", str_value(&session_token)),
    ]);
    let _: OwnedObjectPath = screencast
        .call("CreateSession", &(options,))
        .map_err(|err| failed_err(PORTAL_DOWN, err))?;
    let results = wait.finish()?;
    let session = path_of(results.get("session_handle")).ok_or_else(|| failed(PORTAL_DOWN))?;

    let handle = token('h');
    let mut wait = Waiter::arm(&conn, &handle)?;
    let sources = dict(&[
        ("handle_token", str_value(&handle)),
        ("types", Value::from(1u32 | 2u32)),
        ("multiple", Value::from(false)),
        ("cursor_mode", Value::from(2u32)),
    ]);
    let session_path =
        OwnedObjectPath::try_from(session.clone()).map_err(|err| failed_err(PORTAL_DOWN, err))?;
    let _: OwnedObjectPath = screencast
        .call("SelectSources", &(session_path, sources))
        .map_err(|err| failed_err(PORTAL_DOWN, err))?;
    let _selected = wait.finish()?;

    let handle = token('h');
    let mut wait = Waiter::arm(&conn, &handle)?;
    let start = dict(&[("handle_token", str_value(&handle))]);
    let session_path =
        OwnedObjectPath::try_from(session.clone()).map_err(|err| failed_err(PORTAL_DOWN, err))?;
    let _: OwnedObjectPath = screencast
        .call("Start", &(session_path, "", start))
        .map_err(|err| failed_err(PORTAL_DOWN, err))?;
    let started = wait.finish()?;
    let streams = started
        .get("streams")
        .ok_or_else(|| refused(SCREEN_CANCELLED))?;
    let (node, serial) = first_stream(streams).ok_or_else(|| refused(SCREEN_CANCELLED))?;

    let handle_fd = token('h');
    let remote = dict(&[("handle_token", str_value(&handle_fd))]);
    let session_path =
        OwnedObjectPath::try_from(session.clone()).map_err(|err| failed_err(PORTAL_DOWN, err))?;
    let fd: OwnedFd = screencast
        .call("OpenPipeWireRemote", &(session_path, remote))
        .map_err(|err| failed_err(PORTAL_DOWN, err))?;
    let (target, serial) = match serial {
        Some(serial) => (PW_ID_ANY, Some(serial)),
        None => (node, None),
    };
    let (slot, thread, loop_ptr) = start_pipewire(fd, target, serial, "Screen").map_err(|words| {
        // No PipeWire is said in words. A black XWayland grab would hide that.
        if words == NO_PIPEWIRE {
            refused(&words)
        } else {
            failed(&words)
        }
    })?;
    Ok(Live {
        slot,
        thread: Some(thread),
        loop_ptr,
        conn,
        session: Some(session),
    })
}

fn open_camera_live() -> Result<Live, PortalError> {
    let conn = bus()?;
    let camera = proxy(&conn, CAMERA)?;
    // A missing property is not a no. A present `false` is a no, and V4L2's words stand.
    match camera.get_property::<bool>("IsCameraPresent") {
        Ok(false) => return Err(failed(PORTAL_DOWN)),
        Ok(true) => {}
        Err(err) => tracing::debug!(%err, "portal camera presence"),
    }
    let handle = token('h');
    let mut wait = Waiter::arm(&conn, &handle)?;
    let options = dict(&[("handle_token", str_value(&handle))]);
    let _: OwnedObjectPath = camera
        .call("AccessCamera", &(options,))
        .map_err(|err| failed_err(PORTAL_DOWN, err))?;
    let code_results = wait.finish_camera()?;
    let _ = code_results;
    let remote = dict(&[("handle_token", str_value(&token('h')))]);
    let fd: OwnedFd = camera
        .call("OpenPipeWireRemote", &(remote,))
        .map_err(|err| failed_err(PORTAL_DOWN, err))?;
    let (slot, thread, loop_ptr) =
        start_pipewire(fd, PW_ID_ANY, None, "Camera").map_err(|words| failed(&words))?;
    Ok(Live {
        slot,
        thread: Some(thread),
        loop_ptr,
        conn,
        session: None,
    })
}

fn bus() -> Result<Connection, PortalError> {
    // No display, no dialog. Do not connect: the socket at `$XDG_RUNTIME_DIR/bus` is up on a
    // headless runner, and the portal call then sits until the method timeout.
    if super::session::current() == super::session::Kind::None {
        return Err(failed(NO_BUS));
    }
    zbus::blocking::connection::Builder::session()
        .map_err(|err| failed_err(NO_BUS, err))?
        .method_timeout(Duration::from_secs(10))
        .build()
        .map_err(|err| failed_err(NO_BUS, err))
}

fn proxy<'a>(
    conn: &Connection,
    interface: &'a str,
) -> Result<zbus::blocking::Proxy<'a>, PortalError> {
    zbus::blocking::Proxy::new(conn, DEST, PATH, interface)
        .map_err(|err| failed_err(PORTAL_DOWN, err))
}

fn close_session(conn: &Connection, session: &str) {
    let Ok(proxy) = zbus::blocking::Proxy::new(conn, DEST, session, SESSION_IFACE) else {
        return;
    };
    let closed: Result<(), zbus::Error> = proxy.call("Close", &());
    if let Err(err) = closed {
        tracing::debug!(%err, "portal session close");
    }
}

fn token(kind: char) -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    format!(
        "tiny{}_{kind}{}",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

fn str_value(text: &str) -> Value<'static> {
    Value::from(text.to_owned())
}

fn dict(pairs: &[(&str, Value<'static>)]) -> HashMap<String, Value<'static>> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), value.clone()))
        .collect()
}

fn path_of(value: Option<&OwnedValue>) -> Option<String> {
    let value = value?.try_clone().ok()?;
    if let Ok(path) = OwnedObjectPath::try_from(value.try_clone().ok()?) {
        return Some(path.to_string());
    }
    String::try_from(value).ok()
}

fn first_stream(value: &OwnedValue) -> Option<(u32, Option<String>)> {
    let array = Array::try_from(value.try_clone().ok()?).ok()?;
    let first = array.inner().first()?;
    let Value::Structure(structure) = first else {
        return None;
    };
    let fields = structure.fields();
    let node = match fields.first()? {
        Value::U32(n) => *n,
        Value::U64(n) => *n as u32,
        _ => return None,
    };
    let serial = fields.get(1).and_then(serial_of);
    Some((node, serial))
}

fn serial_of(value: &Value) -> Option<String> {
    let Value::Dict(dict) = value else {
        return None;
    };
    for (key, item) in dict.iter() {
        let Value::Str(key) = key else {
            continue;
        };
        if key.as_str() != "pipewire-serial" && key.as_str() != "serial" {
            continue;
        }
        return match item {
            Value::Str(text) => Some(text.as_str().to_owned()),
            Value::U32(n) => Some(n.to_string()),
            Value::U64(n) => Some(n.to_string()),
            _ => None,
        };
    }
    None
}

/// A `Response` signal, subscribed before the call that causes it. `next` on the iterator blocks
/// with no timeout of its own, so a thread waits and a timeout closes the connection to unblock it.
struct Waiter {
    rx: Receiver<Option<zbus::Result<zbus::message::Message>>>,
    reader: Option<JoinHandle<()>>,
    cancel: Connection,
    done: bool,
}

impl Waiter {
    fn arm(conn: &Connection, handle: &str) -> Result<Waiter, PortalError> {
        let path = request_path(conn, handle)?;
        let rule = zbus::MatchRule::builder()
            .msg_type(MessageType::Signal)
            .interface(REQUEST)
            .map_err(|err| failed_err(PORTAL_DOWN, err))?
            .member("Response")
            .map_err(|err| failed_err(PORTAL_DOWN, err))?
            .path(path.as_str())
            .map_err(|err| failed_err(PORTAL_DOWN, err))?
            .build();
        let mut messages = zbus::blocking::MessageIterator::for_match_rule(rule, conn, Some(8))
            .map_err(|err| failed_err(PORTAL_DOWN, err))?;
        let (tx, rx) = mpsc::channel();
        let reader = std::thread::Builder::new()
            .name("portal-request".into())
            .spawn(move || {
                let _ = tx.send(messages.next());
            })
            .map_err(|err| failed_err(PORTAL_DOWN, err))?;
        Ok(Waiter {
            rx,
            reader: Some(reader),
            cancel: conn.clone(),
            done: false,
        })
    }

    fn finish(&mut self) -> Result<HashMap<String, OwnedValue>, PortalError> {
        self.recv(SCREEN_CANCELLED, "The screen share did not start.")
    }

    fn finish_camera(&mut self) -> Result<HashMap<String, OwnedValue>, PortalError> {
        self.recv(CAMERA_CANCELLED, "The camera did not start.")
    }

    fn recv(
        &mut self,
        cancelled: &str,
        timed_out: &str,
    ) -> Result<HashMap<String, OwnedValue>, PortalError> {
        let reader = self.reader.take();
        let arrived = self.rx.recv_timeout(Duration::from_secs(60));
        let outcome = match arrived {
            Ok(Some(Ok(message))) => parse_response(&message, cancelled),
            Ok(Some(Err(err))) => Err(failed_err(PORTAL_DOWN, err)),
            Ok(None) => Err(failed(PORTAL_DOWN)),
            // A dialog that never answers is not a missing portal. Falling through to X11 here
            // would grab a black XWayland root and look like a picture.
            Err(_) => {
                let _ = self.cancel.clone().close();
                Err(refused(timed_out))
            }
        };
        if let Some(reader) = reader {
            let _ = reader.join();
        }
        self.done = true;
        outcome
    }
}

impl Drop for Waiter {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let _ = self.cancel.clone().close();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn request_path(conn: &Connection, handle: &str) -> Result<String, PortalError> {
    let name = conn.unique_name().ok_or_else(|| failed(NO_BUS))?;
    let sender = name.as_str().trim_start_matches(':').replace('.', "_");
    Ok(format!(
        "/org/freedesktop/portal/desktop/request/{sender}/{handle}"
    ))
}

fn parse_response(
    message: &zbus::message::Message,
    cancelled: &str,
) -> Result<HashMap<String, OwnedValue>, PortalError> {
    let (code, results): (u32, HashMap<String, OwnedValue>) = message
        .body()
        .deserialize()
        .map_err(|err| failed_err(PORTAL_DOWN, err))?;
    match code {
        0 => Ok(results),
        1 => Err(refused(cancelled)),
        _ => Err(failed(PORTAL_DOWN)),
    }
}

type PwInit = unsafe extern "C" fn(*mut i32, *mut *mut *mut c_char);
type LoopNew = unsafe extern "C" fn(*const SpaDict) -> *mut c_void;
type LoopGet = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
type LoopRun = unsafe extern "C" fn(*mut c_void) -> i32;
type LoopQuit = unsafe extern "C" fn(*mut c_void);
type LoopDestroy = unsafe extern "C" fn(*mut c_void);
type ContextNew = unsafe extern "C" fn(*mut c_void, *mut c_void, usize) -> *mut c_void;
type ContextConnectFd = unsafe extern "C" fn(*mut c_void, i32, *mut c_void, usize) -> *mut c_void;
type ContextDestroy = unsafe extern "C" fn(*mut c_void);
type PropsNew = unsafe extern "C" fn(*const SpaDict) -> *mut c_void;
type StreamNew = unsafe extern "C" fn(
    *mut c_void,
    *const c_char,
    *mut c_void,
    *const StreamEvents,
    *mut c_void,
) -> *mut c_void;
type StreamConnect = unsafe extern "C" fn(*mut c_void, i32, u32, u32, *mut *const u8, u32) -> i32;
type StreamDequeue = unsafe extern "C" fn(*mut c_void) -> *mut PwBuffer;
type StreamQueue = unsafe extern "C" fn(*mut c_void, *mut PwBuffer) -> i32;
type StreamDestroy = unsafe extern "C" fn(*mut c_void);

#[derive(Clone, Copy)]
struct Api {
    main_loop_new: LoopNew,
    main_loop_get: LoopGet,
    main_loop_run: LoopRun,
    main_loop_quit: LoopQuit,
    main_loop_destroy: LoopDestroy,
    context_new: ContextNew,
    context_connect_fd: ContextConnectFd,
    context_destroy: ContextDestroy,
    properties_new_dict: PropsNew,
    stream_new_simple: StreamNew,
    stream_connect: StreamConnect,
    stream_dequeue_buffer: StreamDequeue,
    stream_queue_buffer: StreamQueue,
    stream_destroy: StreamDestroy,
}

struct Ctx {
    slot: Arc<Slot>,
    stream: *mut c_void,
    api: Api,
    format: Mutex<Option<Fixated>>,
}

fn api() -> Result<&'static Api, String> {
    static API: OnceLock<Result<Api, String>> = OnceLock::new();
    match API.get_or_init(load_api) {
        Ok(api) => Ok(api),
        Err(words) => Err(words.clone()),
    }
}

fn load_api() -> Result<Api, String> {
    let library = open_pipewire().ok_or_else(|| NO_PIPEWIRE.to_owned())?;
    let library: &'static Library = Box::leak(Box::new(library));
    unsafe {
        let init = load::<PwInit>(library, b"pw_init")?;
        init(std::ptr::null_mut(), std::ptr::null_mut());
        Ok(Api {
            main_loop_new: load(library, b"pw_main_loop_new")?,
            main_loop_get: load(library, b"pw_main_loop_get_loop")?,
            main_loop_run: load(library, b"pw_main_loop_run")?,
            main_loop_quit: load(library, b"pw_main_loop_quit")?,
            main_loop_destroy: load(library, b"pw_main_loop_destroy")?,
            context_new: load(library, b"pw_context_new")?,
            context_connect_fd: load(library, b"pw_context_connect_fd")?,
            context_destroy: load(library, b"pw_context_destroy")?,
            properties_new_dict: load(library, b"pw_properties_new_dict")?,
            stream_new_simple: load(library, b"pw_stream_new_simple")?,
            stream_connect: load(library, b"pw_stream_connect")?,
            stream_dequeue_buffer: load(library, b"pw_stream_dequeue_buffer")?,
            stream_queue_buffer: load(library, b"pw_stream_queue_buffer")?,
            stream_destroy: load(library, b"pw_stream_destroy")?,
        })
    }
}

unsafe fn load<T: Copy>(library: &'static Library, name: &[u8]) -> Result<T, String> {
    let symbol: Symbol<T> = library.get(name).map_err(|_| NO_PIPEWIRE.to_owned())?;
    Ok(*symbol)
}

/// Soname first, so the loader's cache covers Debian, Fedora, Arch and Ubuntu. Then the paths
/// a multilib or Nix-style `LD_LIBRARY_PATH` would use. A miss is a sentence, not a crash.
fn open_pipewire() -> Option<Library> {
    let names = ["libpipewire-0.3.so.0", "libpipewire-0.3.so"];
    if let Some(path) = std::env::var_os("LD_LIBRARY_PATH") {
        for dir in std::env::split_paths(&path) {
            for name in names {
                if let Ok(library) = unsafe { Library::new(dir.join(name)) } {
                    return Some(library);
                }
            }
        }
    }
    for name in names {
        if let Ok(library) = unsafe { Library::new(name) } {
            return Some(library);
        }
    }
    let mut roots = vec![
        "/usr/lib".to_owned(),
        "/usr/lib64".to_owned(),
        "/lib".to_owned(),
        "/lib64".to_owned(),
        "/usr/lib/x86_64-linux-gnu".to_owned(),
        "/usr/lib/aarch64-linux-gnu".to_owned(),
        "/usr/lib/arm-linux-gnueabihf".to_owned(),
        "/usr/lib/i386-linux-gnu".to_owned(),
        "/usr/lib/riscv64-linux-gnu".to_owned(),
        // NixOS keeps libraries out of the FHS paths. The linker cache still finds a soname
        // when the profile is linked, and these two are the system and the user profile.
        "/run/current-system/sw/lib".to_owned(),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(std::path::PathBuf::from(home).join(".nix-profile/lib").to_string_lossy().into_owned());
    }
    if let Some(profiles) = std::env::var_os("NIX_PROFILES") {
        let text = profiles.to_string_lossy();
        for profile in text.split_whitespace() {
            roots.push(format!("{profile}/lib"));
        }
    }
    for root in roots {
        for name in names {
            if let Ok(library) = unsafe { Library::new(format!("{root}/{name}")) } {
                return Some(library);
            }
        }
    }
    None
}

fn start_pipewire(
    fd: OwnedFd,
    target: u32,
    serial: Option<String>,
    role: &'static str,
) -> Result<(Arc<Slot>, JoinHandle<()>, usize), String> {
    let api = *api()?;
    let slot = Slot::new();
    let mine = slot.clone();
    let fd = std::os::fd::OwnedFd::from(fd).into_raw_fd();
    let (tx, rx): (
        Sender<Result<usize, String>>,
        Receiver<Result<usize, String>>,
    ) = mpsc::channel();
    let thread = std::thread::Builder::new()
        .name(
            if role == "Screen" {
                "portal-screen"
            } else {
                "portal-camera"
            }
            .into(),
        )
        .spawn(move || pipewire_thread(api, fd, target, serial, role, mine, tx))
        .map_err(|err| err.to_string())?;
    match rx.recv_timeout(Duration::from_secs(8)) {
        Ok(Ok(loop_ptr)) => Ok((slot, thread, loop_ptr)),
        Ok(Err(words)) => {
            let _ = thread.join();
            Err(words)
        }
        Err(_) => Err(NO_PIPEWIRE.to_owned()),
    }
}

fn pipewire_thread(
    api: Api,
    fd: i32,
    target: u32,
    serial: Option<String>,
    role: &str,
    slot: Arc<Slot>,
    tx: Sender<Result<usize, String>>,
) {
    unsafe {
        let main = (api.main_loop_new)(std::ptr::null());
        if main.is_null() {
            libc::close(fd);
            let _ = tx.send(Err(NO_PIPEWIRE.to_owned()));
            return;
        }
        let loop_ = (api.main_loop_get)(main);
        let context = (api.context_new)(loop_, std::ptr::null_mut(), 0);
        if context.is_null() {
            libc::close(fd);
            (api.main_loop_destroy)(main);
            let _ = tx.send(Err(NO_PIPEWIRE.to_owned()));
            return;
        }
        // PipeWire closes `fd` with the context, including when the connect fails.
        let core = (api.context_connect_fd)(context, fd, std::ptr::null_mut(), 0);
        if core.is_null() {
            (api.context_destroy)(context);
            (api.main_loop_destroy)(main);
            let _ = tx.send(Err("PipeWire did not take the portal's stream.".to_owned()));
            return;
        }
        let props = stream_props(&api, role, serial.as_deref());
        let ctx = Box::into_raw(Box::new(Ctx {
            slot: slot.clone(),
            stream: std::ptr::null_mut(),
            api,
            format: Mutex::new(None),
        }));
        let events = StreamEvents {
            version: 2,
            destroy: None,
            state_changed: Some(on_state),
            control_info: None,
            io_changed: None,
            param_changed: Some(on_param),
            add_buffer: None,
            remove_buffer: None,
            process: Some(on_process),
            drained: None,
            command: None,
            trigger_done: None,
        };
        let name = CString::new(format!("tinyclient-{role}"))
            .unwrap_or_else(|_| CString::new("tinyclient").unwrap());
        let stream = (api.stream_new_simple)(loop_, name.as_ptr(), props, &events, ctx.cast());
        if stream.is_null() {
            drop(Box::from_raw(ctx));
            (api.context_destroy)(context);
            (api.main_loop_destroy)(main);
            let _ = tx.send(Err("PipeWire did not open a capture stream.".to_owned()));
            return;
        }
        (*ctx).stream = stream;
        let pod = spa_pod::offer_pod();
        let pod_ptr = pod.as_ptr();
        let mut params = [pod_ptr];
        let flags = PW_STREAM_FLAG_AUTOCONNECT | PW_STREAM_FLAG_MAP_BUFFERS;
        let connected = (api.stream_connect)(stream, 0, target, flags, params.as_mut_ptr(), 1);
        if connected < 0 {
            (api.stream_destroy)(stream);
            drop(Box::from_raw(ctx));
            (api.context_destroy)(context);
            (api.main_loop_destroy)(main);
            let _ = tx.send(Err("PipeWire refused the capture format.".to_owned()));
            return;
        }
        let _ = tx.send(Ok(main as usize));
        (api.main_loop_run)(main);
        (api.stream_destroy)(stream);
        drop(Box::from_raw(ctx));
        (api.context_destroy)(context);
        (api.main_loop_destroy)(main);
    }
}

fn stream_props(api: &Api, role: &str, serial: Option<&str>) -> *mut c_void {
    let mut owned = vec![
        CString::new("media.type").unwrap(),
        CString::new("Video").unwrap(),
        CString::new("media.category").unwrap(),
        CString::new("Capture").unwrap(),
        CString::new("media.role").unwrap(),
        CString::new(role).unwrap_or_else(|_| CString::new("Screen").unwrap()),
    ];
    if let Some(serial) = serial {
        owned.push(CString::new("target.object").unwrap());
        owned.push(CString::new(serial).unwrap_or_else(|_| CString::new("").unwrap()));
    }
    let items: Vec<SpaDictItem> = owned
        .chunks(2)
        .map(|pair| SpaDictItem {
            key: pair[0].as_ptr(),
            value: pair[1].as_ptr(),
        })
        .collect();
    let dict = SpaDict {
        flags: 0,
        n_items: items.len() as u32,
        items: items.as_ptr(),
    };
    // The properties object copies the strings. `owned` has to live until this returns.
    let props = unsafe { (api.properties_new_dict)(&dict) };
    let _ = owned;
    props
}

unsafe extern "C" fn on_state(data: *mut c_void, _old: i32, state: i32, error: *const c_char) {
    if data.is_null() || state != PW_STREAM_STATE_ERROR {
        return;
    }
    let ctx = &*data.cast::<Ctx>();
    let words = if error.is_null() {
        "PipeWire stopped the capture.".to_owned()
    } else {
        CStr::from_ptr(error).to_string_lossy().into_owned()
    };
    ctx.slot.fail(words);
}

unsafe extern "C" fn on_param(data: *mut c_void, id: u32, param: *const u8) {
    if data.is_null() || id != SPA_PARAM_FORMAT || param.is_null() {
        return;
    }
    let ctx = &*data.cast::<Ctx>();
    let size = std::ptr::read_unaligned(param.cast::<u32>()) as usize;
    let total = 8usize.saturating_add(size);
    if total > 1_048_576 {
        return;
    }
    let bytes = std::slice::from_raw_parts(param, total);
    match spa_pod::parse_format(bytes) {
        Some(format) => {
            *ctx.format
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(format)
        }
        None => tracing::debug!("portal format was not a video size this capture can read"),
    }
}

unsafe extern "C" fn on_process(data: *mut c_void) {
    if data.is_null() {
        return;
    }
    let ctx = &*data.cast::<Ctx>();
    if ctx.stream.is_null() {
        return;
    }
    let buffer = (ctx.api.stream_dequeue_buffer)(ctx.stream);
    if buffer.is_null() {
        return;
    }
    if let Some(frame) = copy_frame(&*buffer.cast::<PwBuffer>(), &ctx.format) {
        ctx.slot.store(frame);
    }
    let _ = (ctx.api.stream_queue_buffer)(ctx.stream, buffer);
}

fn copy_frame(buffer: &PwBuffer, format: &Mutex<Option<Fixated>>) -> Option<Rgba> {
    // Postfix `?` binds tighter than `*`, so this has to copy the `Option` out of the guard first.
    let guard = format.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let fixated = (*guard)?;
    if buffer.buffer.is_null() {
        return None;
    }
    let spa = unsafe { &*buffer.buffer };
    if spa.n_datas == 0 || spa.datas.is_null() {
        return None;
    }
    let data0 = unsafe { &*spa.datas };
    if data0.data.is_null() || data0.chunk.is_null() {
        return None;
    }
    let chunk = unsafe { &*data0.chunk };
    if chunk.size == 0 || chunk.flags & 2 != 0 {
        return None;
    }
    let width = fixated.width;
    let height = fixated.height;
    if width == 0 || height == 0 || width > 8192 || height > 8192 {
        return None;
    }
    match fixated.format {
        SPA_VIDEO_FORMAT_BGRX
        | SPA_VIDEO_FORMAT_BGRA
        | SPA_VIDEO_FORMAT_RGBX
        | SPA_VIDEO_FORMAT_RGBA => {
            let bgra = matches!(
                fixated.format,
                SPA_VIDEO_FORMAT_BGRX | SPA_VIDEO_FORMAT_BGRA
            );
            let opaque = matches!(
                fixated.format,
                SPA_VIDEO_FORMAT_BGRX | SPA_VIDEO_FORMAT_RGBX
            );
            let pixels = copy_rows(data0, chunk, width, height, 4)?;
            let mut pixels = pixels;
            if opaque {
                let mut at = 3;
                while at < pixels.len() {
                    pixels[at] = 255;
                    at += 4;
                }
            }
            Some(Rgba {
                width,
                height,
                pixels,
                bgra,
            })
        }
        SPA_VIDEO_FORMAT_NV12 => {
            let pixels = if spa.n_datas >= 2 {
                let data1 = unsafe { &*spa.datas.add(1) };
                if data1.data.is_null() || data1.chunk.is_null() {
                    return None;
                }
                let uv_chunk = unsafe { &*data1.chunk };
                nv12_planes(data0, chunk, data1, uv_chunk, width, height)?
            } else {
                nv12_packed(data0, chunk, width, height)?
            };
            Some(Rgba {
                width,
                height,
                pixels,
                bgra: false,
            })
        }
        _ => None,
    }
}

fn plane<'a>(data: &'a SpaData, chunk: &SpaChunk) -> Option<&'a [u8]> {
    let span = if data.maxsize > 0 {
        data.maxsize as usize
    } else {
        chunk.offset.saturating_add(chunk.size) as usize
    };
    if span == 0 || data.data.is_null() {
        return None;
    }
    Some(unsafe { std::slice::from_raw_parts(data.data, span) })
}

fn copy_rows(
    data: &SpaData,
    chunk: &SpaChunk,
    width: u32,
    height: u32,
    bpp: usize,
) -> Option<Vec<u8>> {
    let bytes = plane(data, chunk)?;
    let stride = if chunk.stride > 0 {
        chunk.stride as usize
    } else {
        width as usize * bpp
    };
    let row_bytes = width as usize * bpp;
    let mut pixels = Vec::with_capacity(row_bytes * height as usize);
    for row in 0..height as usize {
        let start = chunk.offset as usize + row * stride;
        let end = start.checked_add(row_bytes)?;
        pixels.extend_from_slice(bytes.get(start..end)?);
    }
    Some(pixels)
}

fn nv12_planes(
    y: &SpaData,
    y_chunk: &SpaChunk,
    uv: &SpaData,
    uv_chunk: &SpaChunk,
    width: u32,
    height: u32,
) -> Option<Vec<u8>> {
    let y_bytes = plane(y, y_chunk)?;
    let uv_bytes = plane(uv, uv_chunk)?;
    let y_stride = if y_chunk.stride > 0 {
        y_chunk.stride as usize
    } else {
        width as usize
    };
    let uv_stride = if uv_chunk.stride > 0 {
        uv_chunk.stride as usize
    } else {
        width as usize
    };
    nv12_to_rgba(
        y_bytes.get(y_chunk.offset as usize..)?,
        y_stride,
        uv_bytes.get(uv_chunk.offset as usize..)?,
        uv_stride,
        width,
        height,
    )
}

fn nv12_packed(data: &SpaData, chunk: &SpaChunk, width: u32, height: u32) -> Option<Vec<u8>> {
    let bytes = plane(data, chunk)?;
    let stride = if chunk.stride > 0 {
        chunk.stride as usize
    } else {
        width as usize
    };
    let y_at = chunk.offset as usize;
    let uv_at = y_at + stride * height as usize;
    nv12_to_rgba(
        bytes.get(y_at..)?,
        stride,
        bytes.get(uv_at..)?,
        stride,
        width,
        height,
    )
}

/// Full-range BT.601, integer. Screen and camera bytes from PipeWire are full range unless a
/// modifier said otherwise, and we did not ask for modifiers.
fn nv12_to_rgba(
    y: &[u8],
    y_stride: usize,
    uv: &[u8],
    uv_stride: usize,
    width: u32,
    height: u32,
) -> Option<Vec<u8>> {
    let width = width as usize;
    let height = height as usize;
    if width < 2 || height < 2 {
        return None;
    }
    let mut pixels = vec![0u8; width * height * 4];
    for row in 0..height {
        let y_off = row * y_stride;
        let uv_off = (row / 2) * uv_stride;
        for col in 0..width {
            let luma = *y.get(y_off + col)? as i32;
            let chroma = uv_off + (col & !1);
            let u = *uv.get(chroma)? as i32 - 128;
            let v = *uv.get(chroma + 1)? as i32 - 128;
            let r = (luma + ((359 * v) >> 8)).clamp(0, 255) as u8;
            let g = (luma - ((88 * u) >> 8) - ((183 * v) >> 8)).clamp(0, 255) as u8;
            let b = (luma + ((454 * u) >> 8)).clamp(0, 255) as u8;
            let at = (row * width + col) * 4;
            pixels[at] = r;
            pixels[at + 1] = g;
            pixels[at + 2] = b;
            pixels[at + 3] = 255;
        }
    }
    Some(pixels)
}
