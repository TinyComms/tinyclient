//! Run the same headless daemon scenario used by the lab, in its own process.

#[test]
fn a_new_direct_reaches_the_recipient_over_live_events_and_daemon_ipc() {
    let scratch = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_chatd"))
        .env("TINYCLIENT_DIR", scratch.path())
        .env("TINYCLIENT_PROVE", "tiny-direct-created")
        .output()
        .expect("run isolated daemon scenario");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(String::from_utf8_lossy(&output.stderr).contains("direct-created: history fetched through daemon IPC and adapter"));
}
