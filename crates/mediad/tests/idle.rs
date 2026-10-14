use std::process::Command;

#[test]
fn exits_at_once_when_no_call_is_active() {
    for args in [&[][..], &["0"][..]] {
        let status = Command::new(env!("CARGO_BIN_EXE_mediad"))
            .args(args)
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(0), "mediad {args:?}");
    }
}
