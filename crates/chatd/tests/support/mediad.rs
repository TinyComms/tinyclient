use std::path::PathBuf;

pub fn path() -> PathBuf {
    let executable = std::env::current_exe().expect("test executable");
    resolve(
        std::env::var_os("TINYCLIENT_TEST_MEDIAD"),
        executable,
        || {
            let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
            let mut command = std::process::Command::new(cargo);
            command.args(["build", "-p", "mediad", "--bin", "mediad"]);
            if std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .ends_with("release")
            {
                command.arg("--release");
            }
            assert!(
                command.status().expect("cargo runs").success(),
                "mediad builds"
            );
        },
    )
}

fn resolve(
    prebuilt: Option<std::ffi::OsString>,
    executable: PathBuf,
    build: impl FnOnce(),
) -> PathBuf {
    if let Some(prebuilt) = prebuilt {
        let path = PathBuf::from(prebuilt);
        assert!(
            path.is_absolute() && path.is_file(),
            "TINYCLIENT_TEST_MEDIAD must name an existing absolute file: {}",
            path.display()
        );
        return path;
    }
    build();
    executable
        .parent()
        .and_then(|deps| deps.parent())
        .expect("target profile directory")
        .join(format!("mediad{}", std::env::consts::EXE_SUFFIX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prebuilt_engine_never_invokes_cargo() {
        let executable = std::env::current_exe().unwrap();
        assert_eq!(
            resolve(
                Some(executable.clone().into_os_string()),
                executable.clone(),
                || panic!("must not build")
            ),
            executable
        );
    }

    #[test]
    fn local_run_builds_the_engine_beside_the_test_binaries() {
        let executable = std::env::current_dir()
            .unwrap()
            .join("target/release/deps/test");
        let mut built = false;
        let result = resolve(None, executable, || built = true);
        assert!(built);
        assert_eq!(
            result,
            std::env::current_dir().unwrap().join(format!(
                "target/release/mediad{}",
                std::env::consts::EXE_SUFFIX
            ))
        );
    }

    #[test]
    #[should_panic(expected = "must name an existing absolute file")]
    fn missing_prebuilt_engine_fails_instead_of_building() {
        resolve(
            Some(
                std::env::current_dir()
                    .unwrap()
                    .join("does-not-exist/mediad")
                    .into_os_string(),
            ),
            PathBuf::new(),
            || panic!("must not build"),
        );
    }
}
