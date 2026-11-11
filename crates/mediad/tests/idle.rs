use std::process::Command;

#[test]
fn exits_at_once_when_no_call_is_active() {
    // A scratch `TINYCLIENT_DIR`: without one mediad logs into the installed app's own directory.
    let dir = std::env::temp_dir().join(format!("tc-idle-{}", std::process::id()));
    for args in [&[][..], &["0"][..]] {
        let status = Command::new(env!("CARGO_BIN_EXE_mediad"))
            .args(args)
            .env("TINYCLIENT_DIR", &dir)
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(0), "mediad {args:?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
