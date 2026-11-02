//! The microphone permission, asked by the window before a call starts.
//!
//! macOS asks on behalf of the app a process belongs to and reads the reason from that app's
//! Info.plist. Inside `tinyclient.app` that app is this process, and `mediad`, two processes down,
//! is counted as part of it. So the question is put here, before chatd is asked for the call:
//! the prompt names tinyclient, comes up over tinyclient's own window, and nobody is rung or joined
//! by someone who then turns out to be mute. A denial is said in the dock with the way to
//! System Settings, and no call is started.
//!
//! Nothing is opened: this asks AVFoundation for the authorization and nothing else. It is a
//! handful of Objective-C messages sent through the runtime's C entry points, so the shell
//! takes on no dependency for it.

use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
pub(super) const DENIED: &str = "Allow it in System Settings › Privacy & Security › Microphone, then call again.";
#[cfg(target_os = "linux")]
pub(super) const DENIED: &str = "Allow the microphone in your system privacy settings, then call again.";
#[cfg(windows)]
pub(super) const DENIED: &str = "Allow the microphone in Windows Settings › Privacy › Microphone, then call again.";

/// Android and iOS: the reason names no settings path there.
#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
pub(super) const DENIED: &str = "Allow the microphone in the system settings, then call again.";

/// How long the system's prompt may stay unanswered before the call is started anyway. mediad
/// then finds the permission still unasked and watches for the answer itself.
const PATIENCE: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Microphone {
    Allowed,
    Denied,
    Unasked,
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use std::ffi::{c_char, c_int, c_void, CStr};

    type Id = *mut c_void;

    #[link(name = "objc")]
    extern "C" {
        fn objc_getClass(name: *const c_char) -> Id;
        fn sel_registerName(name: *const c_char) -> Id;
        fn objc_msgSend();
    }

    extern "C" {
        fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
        /// The class of a block that captures nothing and lives for the whole process.
        static _NSConcreteGlobalBlock: c_void;
    }

    const RTLD_LAZY: c_int = 1;
    const BLOCK_IS_GLOBAL: c_int = 1 << 28;

    /// A block as the Objective-C runtime lays one out (the blocks ABI). Ours captures nothing.
    #[repr(C)]
    struct Block {
        isa: *const c_void,
        flags: c_int,
        reserved: c_int,
        invoke: extern "C" fn(*const Block, i8),
        descriptor: *const Descriptor,
    }

    #[repr(C)]
    struct Descriptor {
        reserved: usize,
        size: usize,
    }

    // SAFETY: both are immutable for the life of the process and hold no thread's state.
    unsafe impl Sync for Block {}
    unsafe impl Sync for Descriptor {}

    static DESCRIPTOR: Descriptor = Descriptor { reserved: 0, size: std::mem::size_of::<Block>() };

    /// The answer is read from the authorization itself, so the block has nothing to do.
    extern "C" fn answered(_: *const Block, _granted: i8) {}

    /// `AVCaptureDevice` and the `AVMediaTypeAudio` string, with AVFoundation loaded.
    fn capture() -> Option<(Id, Id)> {
        // SAFETY: plain C calls with NUL-terminated names; a framework that loads stays loaded.
        unsafe {
            let framework = dlopen(c"/System/Library/Frameworks/AVFoundation.framework/AVFoundation".as_ptr(), RTLD_LAZY);
            if framework.is_null() {
                return None;
            }
            // An `NSString *` constant: the symbol is the address of the pointer.
            let audio = dlsym(framework, c"AVMediaTypeAudio".as_ptr()) as *const Id;
            let device = objc_getClass(c"AVCaptureDevice".as_ptr());
            (!audio.is_null() && !device.is_null()).then(|| (device, *audio))
        }
    }

    fn selector(name: &CStr) -> Id {
        // SAFETY: a NUL-terminated name; selectors are interned and never freed.
        unsafe { sel_registerName(name.as_ptr()) }
    }

    /// `None` when AVFoundation cannot be asked, which no supported macOS should produce.
    pub(super) fn status() -> Option<Microphone> {
        let (device, audio) = capture()?;
        // SAFETY: `+[AVCaptureDevice authorizationStatusForMediaType:]` takes an `NSString *` and
        // returns an `NSInteger`; `objc_msgSend` is called through that exact signature.
        let status = unsafe {
            let send: extern "C" fn(Id, Id, Id) -> isize = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            send(device, selector(c"authorizationStatusForMediaType:"), audio)
        };
        Some(match status {
            0 => Microphone::Unasked,
            3 => Microphone::Allowed,
            // 1 is restricted (a profile), 2 is denied.
            _ => Microphone::Denied,
        })
    }

    /// True when this process is an app whose Info.plist says why it wants the microphone. Asking
    /// without one makes macOS end the process, so nothing is asked then: started from a terminal,
    /// the terminal is asked when mediad opens the microphone.
    fn may_ask() -> bool {
        // SAFETY: `+[NSBundle mainBundle]`, `+[NSString stringWithUTF8String:]` and
        // `-[NSBundle objectForInfoDictionaryKey:]`, each through its own signature.
        unsafe {
            let (bundle, string) = (objc_getClass(c"NSBundle".as_ptr()), objc_getClass(c"NSString".as_ptr()));
            if bundle.is_null() || string.is_null() {
                return false;
            }
            let plain: extern "C" fn(Id, Id) -> Id = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let text: extern "C" fn(Id, Id, *const c_char) -> Id = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let lookup: extern "C" fn(Id, Id, Id) -> Id = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let main = plain(bundle, selector(c"mainBundle"));
            let key = text(string, selector(c"stringWithUTF8String:"), c"NSMicrophoneUsageDescription".as_ptr());
            !main.is_null() && !key.is_null() && !lookup(main, selector(c"objectForInfoDictionaryKey:"), key).is_null()
        }
    }

    /// Whether a call may start. Asks the user when they were never asked, and waits for the
    /// answer. Blocks, possibly for as long as a prompt stays up: never on the main thread.
    pub(super) fn ensure() -> Microphone {
        // Devices that tests and the demo replace need nobody's permission.
        if std::env::var_os("TINYCLIENT_MEDIAD_AUDIO").is_some() {
            return Microphone::Allowed;
        }
        match status() {
            Some(Microphone::Unasked) if may_ask() => {}
            // Unknown, or not ours to ask: mediad deals with it as it did before there was a bundle.
            Some(Microphone::Unasked) | None => return Microphone::Allowed,
            Some(known) => return known,
        }
        let Some((device, audio)) = capture() else {
            return Microphone::Allowed;
        };
        static BLOCK: Block = Block {
            // SAFETY of the address: a static's address is a constant.
            isa: unsafe { &_NSConcreteGlobalBlock as *const c_void },
            flags: BLOCK_IS_GLOBAL,
            reserved: 0,
            invoke: answered,
            descriptor: &DESCRIPTOR,
        };
        // SAFETY: `+[AVCaptureDevice requestAccessForMediaType:completionHandler:]` takes an
        // `NSString *` and a block taking a `BOOL`; a global block needs no copying or releasing.
        unsafe {
            let send: extern "C" fn(Id, Id, Id, *const Block) = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            send(device, selector(c"requestAccessForMediaType:completionHandler:"), audio, &BLOCK);
        }
        let asked = Instant::now();
        loop {
            std::thread::sleep(Duration::from_millis(200));
            match status() {
                Some(Microphone::Unasked) if asked.elapsed() < PATIENCE => {}
                Some(Microphone::Unasked) | None => return Microphone::Allowed,
                Some(answer) => return answer,
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod portable {
    use super::*;

    pub(super) fn status() -> Option<Microphone> {
        Some(Microphone::Unasked)
    }

    pub(super) fn ensure() -> Microphone {
        if std::env::var_os("TINYCLIENT_MEDIAD_AUDIO").is_some() {
            return Microphone::Allowed;
        }
        Microphone::Allowed
    }
}

#[cfg(target_os = "macos")]
pub(super) fn status() -> Option<Microphone> {
    macos::status()
}

#[cfg(target_os = "macos")]
pub(super) fn ensure() -> Microphone {
    macos::ensure()
}

#[cfg(not(target_os = "macos"))]
pub(super) fn status() -> Option<Microphone> {
    portable::status()
}

#[cfg(not(target_os = "macos"))]
pub(super) fn ensure() -> Microphone {
    portable::ensure()
}
