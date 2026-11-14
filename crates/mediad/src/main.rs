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
    // The lab's short-hole driver. A build without this opens the audio engine below and stays up.
    if env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("short-hole") {
        if let Err(err) = mediad::jitter::prove_short_hole() {
            eprintln!("short-hole: {err}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }
    // The lab's empty-ring driver. A build without this opens the audio engine below and stays up.
    if env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("drift-empty") {
        if let Err(err) = mediad::audio::prove_drift_empty() {
            eprintln!("drift-empty: {err}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }
    // The lab's missed-frame driver. A build without this opens the audio engine below and stays up.
    // The lab's voice-playout driver. A build without this opens the audio engine below and
    // stays up.
    if env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("voice-playout") {
        if let Err(err) = mediad::media::prove_voice_playout() {
            eprintln!("voice-playout: {err}");
            return ExitCode::from(1);
        }
        // What the driver looks for: any mediad exits 0 once its input closes.
        eprintln!("voice-playout: proved");
        return ExitCode::SUCCESS;
    }
    // The lab's voice-stall driver. A build without this opens the audio engine below and stays up.
    if env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("voice-stall") {
        if let Err(err) = mediad::media::prove_voice_stall() {
            eprintln!("voice-stall: {err}");
            return ExitCode::from(1);
        }
        // What the driver looks for: any mediad exits 0 once its input closes.
        eprintln!("voice-stall: proved");
        return ExitCode::SUCCESS;
    }
    // The lab's voice-freeze-held driver. A build without this opens the audio engine below and
    // stays up.
    if env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("voice-freeze-held") {
        if let Err(err) = mediad::media::prove_voice_freeze() {
            eprintln!("voice-freeze-held: {err}");
            return ExitCode::from(1);
        }
        // What the driver looks for: any mediad exits 0 once its input closes.
        eprintln!("voice-freeze-held: proved");
        return ExitCode::SUCCESS;
    }
    if env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("missed-frame") {
        if let Err(err) = mediad::media::prove_missed_frame() {
            eprintln!("missed-frame: {err}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }
    // The lab's bridge-image driver. A build without this opens the audio engine below and stays up.
    if env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("bridge-image") {
        if let Err(err) = mediad::process::prove_bridge_image() {
            eprintln!("bridge-image: {err}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }
    // The lab's high-band driver. A build without this opens the audio engine below and stays up.
    if env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("high-band") {
        if let Err(err) = mediad::process::prove_high_band() {
            eprintln!("high-band: {err}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }
    // The lab's isolation-bounded driver.
    if env::var("TINYCLIENT_PROVE").ok().as_deref() == Some("isolation-bounded") {
        if let Err(err) = mediad::process::prove_isolation_limit() {
            eprintln!("isolation-bounded: {err}");
            return ExitCode::from(1);
        }
        // What the driver looks for: any mediad exits 0 once its input closes.
        eprintln!("isolation-bounded: proved");
        return ExitCode::SUCCESS;
    }
    let _log = protocol::log_policy::install("mediad", protocol::log_policy::LogPolicy::from_env());
    protocol::crash::install_panic_hook("mediad");
    let code = mediad::serve::run(&args);
    // What is queued for chatd goes first (`out`).
    mediad::out::flush();
    ExitCode::from(code)
}
