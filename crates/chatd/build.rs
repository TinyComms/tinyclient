use std::path::Path;

fn main() {
    println!("cargo:rerun-if-env-changed=TINYCLIENT_TELEGRAM_API_ID");
    println!("cargo:rerun-if-env-changed=TINYCLIENT_TELEGRAM_API_HASH");
    // Telegram's app id is read at build time and shipped in the binary. The calling shell
    // does not always carry it (and nothing else may stand in the repo), so a local
    // `telegram.env` next to the workspace holds it instead: `KEY = VALUE`, one a line. The
    // env keeps its say; CI sets it there and never looks at a file.
    println!("cargo:rerun-if-changed=telegram.env");
    let mut id = std::env::var("TINYCLIENT_TELEGRAM_API_ID").unwrap_or_default();
    let mut hash = std::env::var("TINYCLIENT_TELEGRAM_API_HASH").unwrap_or_default();
    if id.is_empty() && hash.is_empty() {
        match read_local() {
            Ok((found_id, found_hash)) => {
                id = found_id;
                hash = found_hash;
            }
            // No file is a yes: the tests assert the "without an app id" wording, and a build
            // made without either input must not read credentials in from anywhere.
            Err(why) => eprintln!("cargo warning: telegram.env: {why}"),
        }
    }
    // Debug and test builds may omit the id: the Telegram tests assert the "without an app id"
    // message. A release is what gets installed, and it cannot sign in without these.
    let id_ok = id.trim().parse::<i32>().ok().is_some_and(|id| id > 0);
    if !id_ok || hash.trim().is_empty() {
        if std::env::var("PROFILE").as_deref() == Ok("release") {
            panic!(
                "refusing to build a release of chatd without TINYCLIENT_TELEGRAM_API_ID and TINYCLIENT_TELEGRAM_API_HASH"
            );
        }
        return;
    }
    // `rustc-env` is what `option_env!` reads, so the build script is what brings a file's
    // credentials in reach for the adapter, no matter what the shell had.
    println!("cargo:rustc-env=TINYCLIENT_TELEGRAM_API_ID={}", id.trim());
    println!("cargo:rustc-env=TINYCLIENT_TELEGRAM_API_HASH={}", hash.trim());
}

/// The two credentials a local build needs, from the `KEY = VALUE` lines of `telegram.env`
/// next to the workspace, or the error that says why there is nothing: the file is absent, or
/// it does not hold both keys complete.
fn read_local() -> Result<(String, String), String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("telegram.env");
    let file = std::fs::read_to_string(path).map_err(|err| err.to_string())?;
    let mut id = String::new();
    let mut hash = String::new();
    for line in file.lines() {
        let Some((key, value)) = line.split_once('=') else { continue };
        let value = value.trim().trim_matches('"').to_owned();
        match key.trim() {
            "TINYCLIENT_TELEGRAM_API_ID" => id = value,
            "TINYCLIENT_TELEGRAM_API_HASH" => hash = value,
            _ => {}
        }
    }
    if id.is_empty() || hash.is_empty() {
        return Err("the file holds no complete pair of the two keys".to_owned());
    }
    Ok((id, hash))
}
