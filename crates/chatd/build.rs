fn main() {
    println!("cargo:rerun-if-env-changed=TINYCLIENT_TELEGRAM_API_ID");
    println!("cargo:rerun-if-env-changed=TINYCLIENT_TELEGRAM_API_HASH");
    // Debug and test builds may omit the id: the Telegram tests assert the "without an app id"
    // message. A release is what gets installed, and it cannot sign in without these.
    if std::env::var("PROFILE").as_deref() != Ok("release") {
        return;
    }
    let id = std::env::var("TINYCLIENT_TELEGRAM_API_ID").unwrap_or_default();
    let hash = std::env::var("TINYCLIENT_TELEGRAM_API_HASH").unwrap_or_default();
    let id_ok = id.trim().parse::<i32>().ok().is_some_and(|id| id > 0);
    if !id_ok || hash.trim().is_empty() {
        panic!(
            "refusing to build a release of chatd without TINYCLIENT_TELEGRAM_API_ID and TINYCLIENT_TELEGRAM_API_HASH"
        );
    }
}
