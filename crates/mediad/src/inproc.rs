//! mediad inside another process: what a phone runs instead of the binary.
//!
//! [`start`] is a `protocol::media_host::MediaStarter`: the mobile entry registers it
//! (`protocol::media_host::register(mediad::inproc::start)`), and every place chatd or the shell
//! would spawn mediad then gets these threads instead, talking the same frames over an in-memory
//! pipe. The body is the process's own (`serve::run`). What the process would have done to
//! itself is done to this one mediad only (`out`): `exit` ends it and not the app, a panic on
//! any of its threads ends it and is logged, and its environment is the command's.
//!
//! Not done here, because the host process already did it: logging, the panic hook, the
//! Info.plist and signal handling.

use crate::out;
use protocol::media_host::Launch;
use std::io;

/// Starts one mediad on a thread of this process and returns at once.
pub fn start(launch: Launch) -> io::Result<()> {
    let args = launch.args.clone();
    let instance = out::Instance::new(launch)?;
    let name = format!("mediad {}", args.first().map(String::as_str).unwrap_or(""));
    let held = instance.clone();
    std::thread::Builder::new().name(name).spawn(move || {
        out::enter(Some(held.clone()));
        let code = out::guarded(&held, || crate::serve::run(&args)).map_or(101, i32::from);
        // As `main` returning: whatever threads remain see the end through `out::over`.
        held.finish(code);
        out::enter(None);
    })?;
    Ok(())
}
