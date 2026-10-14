//! Whether macOS lets this process hear the microphone. CoreAudio does not say: a denied process
//! is simply handed silence, so the answer comes from AVFoundation, loaded only when a call
//! starts.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Microphone {
    Allowed,
    /// Denied by the user, or by a profile.
    Denied,
    /// Never asked. Opening the microphone makes the system ask.
    Unasked,
}

/// The executable macOS holds responsible for this process: the one whose name is in the
/// microphone prompt and whose Info.plist must say why. The app when we run inside its bundle,
/// the terminal when started from one, ourselves when our parents are gone. `None` when the
/// system would not say.
pub fn responsible() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        return macos_responsible();
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::env::current_exe().ok().map(|path| path.display().to_string())
    }
}

#[cfg(target_os = "macos")]
fn macos_responsible() -> Option<String> {
    use std::ffi::{c_char, c_int, c_void};

    extern "C" {
        fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    }

    const RTLD_LAZY: c_int = 1;

    extern "C" {
        fn getpid() -> c_int;
        fn proc_pidpath(pid: c_int, buffer: *mut c_void, size: u32) -> c_int;
    }
    // SAFETY: the symbol, when libSystem has it, is `pid_t (*)(pid_t)`; the buffer is ours and
    // `proc_pidpath` writes at most `size` bytes into it.
    unsafe {
        let handle = dlopen(std::ptr::null(), RTLD_LAZY);
        let symbol = dlsym(handle, c"responsibility_get_pid_responsible_for_pid".as_ptr());
        if symbol.is_null() {
            return None;
        }
        let ask: extern "C" fn(c_int) -> c_int = std::mem::transmute(symbol);
        let pid = ask(getpid());
        let mut path = [0u8; 4096];
        let len = proc_pidpath(pid, path.as_mut_ptr().cast(), path.len() as u32);
        let len = usize::try_from(len).ok().filter(|&len| len > 0)?;
        Some(String::from_utf8_lossy(&path[..len]).into_owned())
    }
}

/// `None` when AVFoundation cannot be asked, which no supported macOS should produce.
pub fn microphone() -> Option<Microphone> {
    #[cfg(target_os = "macos")]
    {
        return macos_microphone();
    }
    #[cfg(not(target_os = "macos"))]
    {
        Some(Microphone::Unasked)
    }
}

#[cfg(target_os = "macos")]
fn macos_microphone() -> Option<Microphone> {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};
    use std::ffi::{c_char, c_int, c_void};

    extern "C" {
        fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    }

    const RTLD_LAZY: c_int = 1;

    // SAFETY: plain C calls with NUL-terminated names; a framework that loads stays loaded.
    let media_type = unsafe {
        let framework = dlopen(c"/System/Library/Frameworks/AVFoundation.framework/AVFoundation".as_ptr(), RTLD_LAZY);
        if framework.is_null() {
            return None;
        }
        // `AVMediaTypeAudio` is an `NSString *` constant: the symbol is the address of the pointer.
        let symbol = dlsym(framework, c"AVMediaTypeAudio".as_ptr()) as *const *const AnyObject;
        if symbol.is_null() {
            return None;
        }
        *symbol
    };
    let device = AnyClass::get(c"AVCaptureDevice")?;
    // SAFETY: `+[AVCaptureDevice authorizationStatusForMediaType:]` takes an `NSString *` and
    // returns an `NSInteger`.
    let status: isize = unsafe { msg_send![device, authorizationStatusForMediaType: media_type] };
    Some(match status {
        0 => Microphone::Unasked,
        3 => Microphone::Allowed,
        // 1 is restricted, 2 is denied.
        _ => Microphone::Denied,
    })
}
