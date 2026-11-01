//! Scheduling for the threads audio depends on. On Windows a device callback runs on a thread
//! cpal makes at normal priority, which anything busy can delay past WASAPI's deadline: that is
//! the "buffer underrun or overrun" a call logged by the thousand. MMCSS ("Pro Audio") gives it
//! the scheduling the system's own audio engine gets. The call's tick is woken by the system
//! timer, which on Windows ticks every 15.6 ms unless asked for finer; [`FineTimer`] asks for
//! 1 ms while a call runs. Elsewhere these do nothing: CoreAudio's threads are already real-time.

/// Puts the calling thread in MMCSS's "Pro Audio" class, once per thread: a device callback.
pub fn device_thread() {
    #[cfg(windows)]
    windows::promote("Pro Audio");
}

/// Puts the calling thread in MMCSS's "Audio" class: the call's 20 ms tick, which does the
/// processing and codecs between the device rings and the network.
pub fn tick_thread() {
    #[cfg(windows)]
    windows::promote("Audio");
}

/// Puts the calling thread below the tick: video's capture, codecs and files, which may run late
/// and may not make the tick do so.
pub fn worker_thread() {
    #[cfg(windows)]
    windows::below_normal();
    // SAFETY: plain calls about this thread, which take no pointers. On Linux a nice value is
    // the thread's own, and `0` means this one.
    #[cfg(target_os = "linux")]
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 10);
    }
    #[cfg(target_os = "macos")]
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_UTILITY, 0);
    }
}

/// The system timer at 1 ms for as long as this is held.
pub struct FineTimer {
    #[cfg(windows)]
    held: bool,
}

impl FineTimer {
    pub fn hold() -> FineTimer {
        #[cfg(windows)]
        {
            FineTimer { held: windows::fine_timer(true) }
        }
        #[cfg(not(windows))]
        FineTimer {}
    }
}

impl Drop for FineTimer {
    fn drop(&mut self) {
        #[cfg(windows)]
        if self.held {
            windows::fine_timer(false);
        }
    }
}

#[cfg(windows)]
mod windows {
    use std::cell::Cell;
    use windows_sys::Win32::Media::{timeBeginPeriod, timeEndPeriod};
    use windows_sys::Win32::System::Threading::{AvSetMmThreadCharacteristicsW, GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL};

    thread_local! {
        static PROMOTED: Cell<bool> = const { Cell::new(false) };
    }

    pub fn promote(class: &str) {
        if PROMOTED.with(Cell::get) {
            return;
        }
        PROMOTED.with(|promoted| promoted.set(true));
        // A short fixed buffer: this runs on a device thread, once, and must not allocate.
        let mut name = [0u16; 16];
        for (slot, unit) in name.iter_mut().zip(class.encode_utf16()) {
            *slot = unit;
        }
        let mut task = 0u32;
        // SAFETY: `name` is NUL-terminated UTF-16 and `task` is ours to write. The handle is
        // kept for the thread's life: MMCSS drops the registration when the thread ends.
        let handle = unsafe { AvSetMmThreadCharacteristicsW(name.as_ptr(), &mut task) };
        if handle.is_null() {
            // Not fatal: the call plays on at normal priority, and the log says why it may glitch.
            tracing::warn!("mediad: the {class} thread could not get MMCSS scheduling");
        }
    }

    pub fn below_normal() {
        // SAFETY: the pseudo-handle of this thread needs no closing.
        unsafe {
            SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
        }
    }

    pub fn fine_timer(on: bool) -> bool {
        // SAFETY: plain calls; each begin is matched by one end in `FineTimer::drop`.
        unsafe { if on { timeBeginPeriod(1) == 0 } else { timeEndPeriod(1) == 0 } }
    }
}
