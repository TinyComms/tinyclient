//! What `build.rs` tells cargo to watch around Telegram's app id. A file of its own so that a
//! unit test can include the same code (`src/lib.rs` does); `build.rs` includes it too.

use std::path::{Path, PathBuf};

/// `telegram.env`, next to the workspace's `Cargo.toml`: two levels above this crate.
pub fn local_file(manifest_dir: &Path) -> PathBuf {
    manifest_dir.join("..").join("..").join("telegram.env")
}

/// The `cargo:rerun-if-` lines `build.rs` prints, in the order it prints them.
///
/// The two environment variables are always watched, set or not. That matters: a script that
/// prints no `rerun-if` line at all is run again by cargo whenever any file of its package
/// changes.
///
/// `telegram.env` is watched only while it is there (`Some`), and by its full path: cargo takes a
/// relative one from this crate's folder, and the file is read from the workspace root. A missing
/// file is left out because cargo runs a build script again on every single command for as long
/// as a file it watches is missing, and most checkouts have no `telegram.env` (CI sets the two
/// variables in the environment). In those, `chatd` and everything compiled after it was built
/// again on every `cargo` command.
///
/// What that costs: a `telegram.env` created after a first build is not noticed by cargo until
/// `build.rs` is touched or `cargo clean -p chatd` is run.
pub fn rerun_instructions(local_file: Option<&Path>) -> Vec<String> {
    let mut lines = vec![
        "cargo:rerun-if-env-changed=TINYCLIENT_TELEGRAM_API_ID".to_owned(),
        "cargo:rerun-if-env-changed=TINYCLIENT_TELEGRAM_API_HASH".to_owned(),
    ];
    if let Some(file) = local_file {
        lines.push(format!("cargo:rerun-if-changed={}", file.display()));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENV: [&str; 2] = [
        "cargo:rerun-if-env-changed=TINYCLIENT_TELEGRAM_API_ID",
        "cargo:rerun-if-env-changed=TINYCLIENT_TELEGRAM_API_HASH",
    ];

    /// No `telegram.env` in the checkout: cargo must not be told to watch one, or it runs the
    /// script and recompiles `chatd` on every single command. The environment is still watched.
    #[test]
    fn a_missing_telegram_env_is_not_watched() {
        assert_eq!(rerun_instructions(None), ENV);
    }

    /// `telegram.env` is there: cargo watches it, by the path it is read from.
    #[test]
    fn a_telegram_env_that_is_there_is_watched() {
        let file = local_file(Path::new("/checkout/crates/chatd"));
        let expected: Vec<String> = ENV
            .into_iter()
            .map(String::from)
            .chain([format!("cargo:rerun-if-changed={}", file.display())])
            .collect();
        assert_eq!(rerun_instructions(Some(&file)), expected);
    }
}
