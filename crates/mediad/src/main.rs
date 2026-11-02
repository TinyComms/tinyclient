//! The mediad process. Its body is `mediad::serve::run`, which a phone runs in process instead
//! (`mediad::inproc`).

use std::env;
use std::process::ExitCode;

/// macOS reads the reason for the microphone prompt from the Info.plist of the process it holds
/// responsible. Inside `tinyclient.app` that is the app, whose Info.plist says it. When mediad is
/// responsible for itself (its parents were started by hand and are gone), this copy, in the
/// section a single-file tool keeps its Info.plist in, is what the system reads; without one
/// the system kills the process the moment it opens the microphone.
#[used]
#[link_section = "__TEXT,__info_plist"]
static INFO_PLIST: [u8; include_bytes!("../Info.plist").len()] = *include_bytes!("../Info.plist");

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--version") {
        println!("mediad {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    let _log = protocol::log_policy::install("mediad", protocol::log_policy::LogPolicy::from_env());
    protocol::crash::install_panic_hook("mediad");
    let code = mediad::serve::run(&args);
    // What is queued for chatd goes first (`out`).
    mediad::out::flush();
    ExitCode::from(code)
}
