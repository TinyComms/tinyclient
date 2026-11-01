// Puts packaging/windows/tinyclient.ico in tinyclient.exe as icon resource 1: the one Explorer and
// the taskbar show, and the one GPUI's Windows window loads. Nothing on other systems.
//
// Also names the commit this build is from, as TINYCLIENT_COMMIT, so the updater can tell a build
// behind the latest release of the same version (src/update.rs). Empty when it cannot be known.
fn main() {
    #[cfg(windows)]
    {
        let manifest = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
        let ico = manifest.join("../../packaging/windows/tinyclient.ico");
        println!("cargo:rerun-if-changed={}", ico.display());
        let rc = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("tinyclient.rc");
        let escaped = ico.display().to_string().replace('\\', "\\\\");
        std::fs::write(&rc, format!("1 ICON \"{escaped}\"\n")).unwrap();
        embed_resource::compile(&rc, embed_resource::NONE).manifest_optional().unwrap();
    }
    println!("cargo:rustc-env=TINYCLIENT_COMMIT={}", commit());
    println!("cargo:rerun-if-changed=build.rs");
}

/// `TINYCLIENT_COMMIT` when a packager sets it, then Actions' `GITHUB_SHA` (the commit checked
/// out, so a tag's release carries the tag's commit), then `git rev-parse HEAD`. A source tree with
/// no git, or no git at all, is a build without a commit; it never fails the build.
fn commit() -> String {
    println!("cargo:rerun-if-env-changed=TINYCLIENT_COMMIT");
    println!("cargo:rerun-if-env-changed=GITHUB_SHA");
    let sha = ["TINYCLIENT_COMMIT", "GITHUB_SHA"]
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|sha| !sha.trim().is_empty()))
        .or_else(|| {
            let sha = git(&["rev-parse", "HEAD"])?;
            watch_head();
            Some(sha)
        })
        .unwrap_or_default();
    let sha = sha.trim().to_ascii_lowercase();
    if sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit()) {
        sha
    } else {
        String::new()
    }
}

/// Rebuilds when HEAD moves: HEAD itself (a checkout), and the branch it names (a commit). Only
/// files that exist are watched, since Cargo reruns a script whose watched file is missing.
fn watch_head() {
    let mut paths = vec![("HEAD".to_owned(), false), ("packed-refs".to_owned(), false)];
    if let Some(name) = git(&["symbolic-ref", "-q", "HEAD"]) {
        paths.push((name.trim().to_owned(), true));
    }
    let Some(manifest) = std::env::var_os("CARGO_MANIFEST_DIR").map(std::path::PathBuf::from) else {
        return;
    };
    for (path, branch) in paths {
        let Some(found) = git(&["rev-parse", "--git-path", &path]) else {
            continue;
        };
        let found = manifest.join(found.trim());
        // A branch packed into packed-refs gets its own file again at the next commit, in the
        // folder of its siblings.
        let watched = if found.exists() {
            Some(found)
        } else if branch {
            found.parent().filter(|dir| dir.is_dir() && dir.components().any(|part| part.as_os_str() == "refs")).map(|dir| dir.to_owned())
        } else {
            None
        };
        if let Some(watched) = watched {
            println!("cargo:rerun-if-changed={}", watched.display());
        }
    }
}

fn git(args: &[&str]) -> Option<String> {
    let dir = std::env::var_os("CARGO_MANIFEST_DIR")?;
    let out = std::process::Command::new("git").args(args).current_dir(dir).stderr(std::process::Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8(out.stdout).ok()).flatten()
}
