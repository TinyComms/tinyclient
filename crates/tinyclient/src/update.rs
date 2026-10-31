//! tinyclient replacing itself (`docs/design/08-first-run-settings.md` §8,
//! `docs/product/08-sign-in-accounts.md` story 18).
//!
//! While the window is open it asks GitHub's public release about once an hour, and again
//! shortly after launch, whether a newer one exists. A newer release is said in the window
//! until it is dismissed. When that release has this copy's version, a second request asks
//! GitHub to compare the release's tag with the commit this copy was built from: a build from
//! before the tag (a copy made from main ahead of the release) is offered the release too. Each
//! request's user agent is `tinyclient/<version>` and it carries nothing else: no account, no
//! machine id. The commit in the second request's address is the public one this build is from.
//! With automatic install on, and only when this process is the installed app (not a build under
//! `target/`), the archive is downloaded, checked against the release's `SHA256SUMS`, and on
//! macOS the new app is checked with `codesign` before it is kept. It replaces this copy when
//! the app quits. A failure is remembered for a day and said only in About.
//!
//! The window draws the states. This module does the check, the file and the swap.

use sha2::{Digest, Sha256};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const FEED: &str = "https://api.github.com/repos/itz4blitz/tinyclient/releases/latest";
const COMPARE: &str = "https://api.github.com/repos/itz4blitz/tinyclient/compare";
const RELEASES: &str = "https://github.com/itz4blitz/tinyclient/releases/latest";
const ARCHIVE_LIMIT: u64 = 200 * 1024 * 1024;

/// What the window is allowed to say. Quiet is said by saying nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shown {
    Quiet,
    Available { version: String, page: String },
    Ready { version: String },
    Failed { version: String },
}

/// Why a check is running.
#[derive(Clone, Copy, Debug)]
pub enum Policy {
    /// Check for Updates…, and the check while the window is open. Downloads when automatic
    /// install is on.
    Now { automatic: bool },
    /// About's Install, or the switch being turned on. Downloads when this copy can replace itself.
    Install,
}

struct Memory {
    shown: Shown,
    checked: i64,
    busy: bool,
    /// The version this process has already told the user about.
    announced: Option<String>,
}

static MEMORY: Mutex<Memory> = Mutex::new(Memory {
    shown: Shown::Quiet,
    checked: 0,
    busy: false,
    announced: None,
});

fn memory() -> std::sync::MutexGuard<'static, Memory> {
    MEMORY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub fn shown() -> Shown {
    memory().shown.clone()
}

pub fn menu_label() -> &'static str {
    if matches!(shown(), Shown::Ready { .. }) {
        "Restart to Update"
    } else {
        "Check for Updates…"
    }
}

/// The rail's dot: a version is waiting, whether or not it has been downloaded.
pub fn announced() -> bool {
    matches!(shown(), Shown::Available { .. } | Shown::Ready { .. })
}

/// A newer release the window has not told the user about yet, in this process. The same
/// version is returned only once, so the hourly check does not repeat the alert.
pub fn announce() -> Option<String> {
    let mut memory = memory();
    let version = match &memory.shown {
        Shown::Available { version, .. } | Shown::Ready { version } => version.clone(),
        _ => return None,
    };
    if memory.announced.as_deref() == Some(version.as_str()) {
        return None;
    }
    memory.announced = Some(version.clone());
    Some(version)
}

pub fn current() -> &'static str {
    // `TINYCLIENT_UPDATE_AS=0.2.2` pretends this build is older, so the real GitHub feed can
    // be watched without publishing a release. A shipped app never sets it.
    static PINNED: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    PINNED
        .get_or_init(|| std::env::var("TINYCLIENT_UPDATE_AS").unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_owned()))
        .as_str()
}

/// The commit this build is from (`build.rs`), or empty when it was built without git.
pub fn commit() -> &'static str {
    env!("TINYCLIENT_COMMIT")
}

/// The commit as people read it next to the version: its first seven characters.
pub fn short_commit() -> Option<&'static str> {
    commit().get(..7)
}

/// This build's archive, as `.github/workflows/release.yml` names the unversioned copy.
pub fn archive_name() -> Option<&'static str> {
    if cfg!(target_os = "macos") && cfg!(target_arch = "aarch64") {
        Some("tinyclient-macos.zip")
    } else if cfg!(target_os = "linux") && cfg!(target_arch = "x86_64") {
        Some("tinyclient-linux-x86_64.tar.gz")
    } else if cfg!(target_os = "windows") && cfg!(target_arch = "x86_64") {
        Some("tinyclient-windows-x86_64.zip")
    } else {
        None
    }
}

fn archive_suffix() -> Option<&'static str> {
    archive_name().map(|name| match name {
        name if name.ends_with("-macos.zip") => "-macos.zip",
        name if name.ends_with("-linux-x86_64.tar.gz") => "-linux-x86_64.tar.gz",
        _ => "-windows-x86_64.zip",
    })
}

/// Where a quit may write the new copy. See [`replaceable`].
pub fn install_into() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    if !replaceable(&exe) {
        return None;
    }
    if cfg!(target_os = "macos") {
        return mac_bundle(&exe);
    }
    let dir = exe.parent()?.to_owned();
    let sibling = |name: &str| dir.join(program(name)).is_file();
    (sibling("chatd") && sibling("mediad") && sibling("tinyclient-login") && writable(&dir)).then_some(dir)
}

/// Whether this user can put files in `dir`. A copy installed for everyone (Program Files, a
/// system package) is not: it is updated by downloading the installer, not in place.
fn writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".tinyclient-write-probe-{}", std::process::id()));
    let made = fs::OpenOptions::new().write(true).create_new(true).open(&probe).is_ok();
    let _ = fs::remove_file(&probe);
    made
}

/// A tree under Cargo's `target/` is a development build and is never replaced. Neither is a copy
/// in the Nix store, which is read-only and changes only when Nix builds a new one
/// (`packaging/nix/package.nix`).
fn replaceable(exe: &Path) -> bool {
    !exe.components().any(|part| part.as_os_str() == "target") && !exe.starts_with("/nix/store")
}

fn program(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    }
}

/// `…/tinyclient.app/Contents/MacOS/<exe>` → the `.app`.
fn mac_bundle(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    if macos.file_name()? != "MacOS" {
        return None;
    }
    let contents = macos.parent()?;
    if contents.file_name()? != "Contents" {
        return None;
    }
    let app = contents.parent()?;
    (app.extension()? == "app").then(|| app.to_owned())
}

fn work_dir() -> PathBuf {
    protocol::support_dir().join("update")
}

/// Reads a download left from the last run, so the dot is there before the check answers.
pub fn recall() {
    let saved = read_state(&work_dir().join("state"));
    // What the last swap did, left by the script that ran after we quit.
    let swapped = fs::read_to_string(work_dir().join(SWAP_RESULT)).unwrap_or_default();
    let _ = fs::remove_file(work_dir().join(SWAP_RESULT));
    if let Some(version) = failed_swap(&swapped) {
        let mut memory = memory();
        memory.checked = saved.checked;
        memory.shown = Shown::Failed { version };
        let _ = write_state(&work_dir(), &memory.shown, &saved.page, memory.checked);
        return;
    }
    let mut memory = memory();
    memory.checked = saved.checked;
    memory.shown = match saved.shown {
        Shown::Ready { version } if payload_ready(&work_dir()).is_some() => {
            Shown::Ready { version }
        }
        Shown::Ready { version } => Shown::Available {
            version,
            page: saved.page,
        },
        other => other,
    };
}

/// Asks, and downloads when the policy says to. Safe to call while another check is running:
/// the second call leaves the state as it was.
pub fn pursue(policy: Policy) -> Shown {
    let automatic = match policy {
        Policy::Now { automatic } => automatic,
        Policy::Install => true,
    };
    {
        let mut memory = memory();
        if memory.busy || archive_name().is_none() {
            return memory.shown.clone();
        }
        memory.busy = true;
    }
    let outcome = fetch(automatic && install_into().is_some());
    let mut memory = memory();
    memory.busy = false;
    match outcome {
        Outcome::Unchanged => {}
        Outcome::Set {
            shown,
            checked,
            page,
        } => {
            memory.shown = shown;
            memory.checked = checked;
            let _ = write_state(&work_dir(), &memory.shown, &page, memory.checked);
        }
    }
    memory.shown.clone()
}

/// Forgets a downloaded copy and goes back to "available", so quitting does not install it.
/// The version we already found stays, so About can still offer Install.
pub fn discard_download() {
    let dir = work_dir();
    let _ = fs::remove_dir_all(dir.join("staged"));
    let _ = fs::remove_file(dir.join("incoming.part"));
    let mut memory = memory();
    if let Shown::Ready { version } = memory.shown.clone() {
        let page = read_state(&dir.join("state")).page;
        memory.shown = Shown::Available {
            version,
            page: page.clone(),
        };
        let _ = write_state(&dir, &memory.shown, &page, memory.checked);
    }
}

/// Starts the swap, which waits until this process has exited. Does nothing when no download
/// is waiting. Called at the start of quit, before chatd is asked to stop.
pub fn apply_on_quit() {
    let Shown::Ready { version } = shown() else {
        return;
    };
    let Some(dest) = install_into() else {
        return;
    };
    let Some(payload) = payload_ready(&work_dir()) else {
        return;
    };
    if cfg!(target_os = "macos") && dest.extension().and_then(|ext| ext.to_str()) != Some("app") {
        return;
    }
    let _ = spawn_swap(std::process::id(), &payload, &dest, &version);
}

enum Outcome {
    Unchanged,
    Set {
        shown: Shown,
        checked: i64,
        page: String,
    },
}

fn fetch(download: bool) -> Outcome {
    let now = unix_now();
    let Some(body) = http_text(FEED, true) else {
        tracing::warn!(phase = "check", "updater failed");
        return Outcome::Unchanged;
    };
    let Some(release) = parse_release(&body) else {
        return Outcome::Unchanged;
    };
    // The compare is asked only for the same version: a newer one is offered without it.
    let compared = if cmp_version(&release.version, current()).is_eq() {
        compare(&release.tag, commit())
    } else {
        None
    };
    if !offered(&release.version, current(), compared.as_deref()) {
        let _ = fs::remove_dir_all(work_dir().join("staged"));
        return Outcome::Set {
            shown: Shown::Quiet,
            checked: now,
            page: release.page,
        };
    }
    if !download {
        return Outcome::Set {
            shown: Shown::Available {
                version: release.version.clone(),
                page: release.page.clone(),
            },
            checked: now,
            page: release.page,
        };
    }
    if let Shown::Ready { version } = shown() {
        if version == release.version && payload_ready(&work_dir()).is_some() {
            return Outcome::Set {
                shown: Shown::Ready { version },
                checked: now,
                page: release.page,
            };
        }
    }
    let page = release.page.clone();
    match stage(&release) {
        Ok(()) => Outcome::Set {
            shown: Shown::Ready {
                version: release.version,
            },
            checked: now,
            page,
        },
        Err(()) => Outcome::Set {
            shown: Shown::Failed {
                version: release.version,
            },
            checked: now,
            page,
        },
    }
}

struct Release {
    tag: String,
    version: String,
    page: String,
    archive: String,
    archive_url: String,
    sums_url: String,
}

fn stage(release: &Release) -> Result<(), ()> {
    let dir = work_dir();
    fs::create_dir_all(&dir).map_err(|_| ())?;
    let incoming = dir.join("incoming.part");
    let _ = fs::remove_file(&incoming);
    if http_file(&release.archive_url, &incoming).is_err() {
        tracing::warn!(phase = "download", "updater failed");
        return Err(());
    }
    let len = fs::metadata(&incoming).map_err(|_| ())?.len();
    if len == 0 || len > ARCHIVE_LIMIT {
        let _ = fs::remove_file(&incoming);
        return Err(());
    }
    let sums = http_text(&release.sums_url, false).ok_or(())?;
    let want = sum_for(&sums, &release.archive).ok_or(())?;
    let got = sha256_file(&incoming).map_err(|_| ())?;
    if !got.eq_ignore_ascii_case(want) {
        let _ = fs::remove_file(&incoming);
        tracing::warn!(phase = "verify", "updater failed");
        return Err(());
    }
    let staged = dir.join("staged");
    let _ = fs::remove_dir_all(&staged);
    fs::create_dir_all(&staged).map_err(|_| ())?;
    extract(&incoming, &staged)?;
    let _ = fs::remove_file(&incoming);
    if payload_ready(&dir).is_none() {
        let _ = fs::remove_dir_all(&staged);
        return Err(());
    }
    if cfg!(target_os = "macos") {
        let app = staged.join("tinyclient.app");
        if !codesign_ok(&app) {
            let _ = fs::remove_dir_all(&staged);
            return Err(());
        }
    }
    Ok(())
}

fn payload_ready(dir: &Path) -> Option<PathBuf> {
    let staged = dir.join("staged");
    if cfg!(target_os = "macos") {
        let app = staged.join("tinyclient.app");
        return (app.join("Contents/MacOS/tinyclient").is_file()
            && app.join("Contents/MacOS/chatd").is_file())
        .then_some(app);
    }
    let tinyclient = staged.join(program("tinyclient"));
    let chatd = staged.join(program("chatd"));
    (tinyclient.is_file() && chatd.is_file()).then_some(staged)
}

fn extract(archive: &Path, into: &Path) -> Result<(), ()> {
    let status =
        if cfg!(target_os = "macos") && archive_name().is_some_and(|name| name.ends_with(".zip")) {
            Command::new("/usr/bin/ditto")
                .args(["-x", "-k"])
                .arg(archive)
                .arg(into)
                .status()
        } else {
            Command::new("tar")
                .args(["-xf"])
                .arg(archive)
                .arg("-C")
                .arg(into)
                .status()
        };
    status.map_err(|_| ())?.success().then_some(()).ok_or(())
}

fn codesign_ok(app: &Path) -> bool {
    Command::new("/usr/bin/codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(app)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn spawn_swap(pid: u32, payload: &Path, dest: &Path, version: &str) -> io::Result<()> {
    let dir = work_dir();
    fs::create_dir_all(&dir)?;
    if cfg!(windows) {
        let script = dir.join("apply.ps1");
        let result = dir.join(SWAP_RESULT);
        let _ = fs::remove_file(&result);
        fs::write(&script, windows_script(pid, payload, dest, version, &result))?;
        Command::new("cmd")
            .args([
                "/C",
                "start",
                "",
                "/MIN",
                "powershell",
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
            ])
            .arg(&script)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        return Ok(());
    }
    let script = dir.join("apply.sh");
    let body = if cfg!(target_os = "macos") {
        mac_script(pid, payload, dest)
    } else {
        unix_script(pid, payload, dest)
    };
    fs::write(&script, body)?;
    // The inner shell is backgrounded at once, so it is reparented before we exit and is not
    // taken down with the window.
    let launch = format!(
        "/bin/sh {} >/dev/null 2>&1 &",
        shell_quote(&script.display().to_string())
    );
    Command::new("/bin/sh")
        .args(["-c", &launch])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    Ok(())
}

fn mac_script(pid: u32, payload: &Path, dest: &Path) -> String {
    format!(
        "#!/bin/sh\nset -eu\nwhile kill -0 {pid} 2>/dev/null; do sleep 0.2; done\nsleep 0.5\nrm -rf {dest}\nditto {src} {dest}\nopen {dest}\n",
        dest = shell_quote(&dest.display().to_string()),
        src = shell_quote(&payload.display().to_string()),
    )
}

fn unix_script(pid: u32, payload: &Path, dest: &Path) -> String {
    let mut moves = String::new();
    for name in ["tinyclient", "chatd", "mediad", "tinyclient-login"] {
        let from = shell_quote(&payload.join(name).display().to_string());
        let to = shell_quote(&dest.join(name).display().to_string());
        moves.push_str(&format!("mv -f {from} {to}\nchmod +x {to}\n"));
    }
    let relaunch = shell_quote(&dest.join("tinyclient").display().to_string());
    format!("#!/bin/sh\nset -eu\nwhile kill -0 {pid} 2>/dev/null; do sleep 0.2; done\nsleep 0.5\n{moves}{relaunch} >/dev/null 2>&1 &\n")
}

/// The file the Windows swap leaves: `updated <version>`, or `failed <version> <why>`.
const SWAP_RESULT: &str = "swap-result";

/// The uninstall entry of the Inno Setup AppId in `packaging/windows/tinyclient.iss`. Windows
/// shows its version under Installed apps.
const WINDOWS_APP_KEY: &str =
    r"HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\{8C1B5E0A-4F3D-4C1E-9B7A-2D6E5F1A7C3B}_is1";

/// The version a failed swap was for, from [`SWAP_RESULT`].
fn failed_swap(result: &str) -> Option<String> {
    let mut words = result.split_whitespace();
    (words.next() == Some("failed")).then(|| words.next().map(str::to_owned)).flatten()
}

/// Runs after the window has quit. chatd, mediad and a sign-in window outlive it by a moment, and
/// Windows will not replace a running program, so it waits for everything started from this
/// folder and stops what is still there after 15 s. The old programs are moved aside first and
/// put back if any step fails, so a failure never leaves two versions side by side. Whatever
/// happened, tinyclient opens again.
fn windows_script(pid: u32, payload: &Path, dest: &Path, version: &str, result: &Path) -> String {
    let quoted = |text: &str| format!("'{}'", text.replace('\'', "''"));
    r#"$ErrorActionPreference = 'Stop'
$dest = @DEST@
$src = @SRC@
$result = @RESULT@
$names = 'tinyclient','chatd','mediad','tinyclient-login'
function Running { Get-Process -ErrorAction SilentlyContinue | Where-Object { $_.Path -and ((Split-Path $_.Path) -eq $dest) } }
Wait-Process -Id @PID@ -Timeout 30 -ErrorAction SilentlyContinue
$until = (Get-Date).AddSeconds(15)
while ((Running) -and ((Get-Date) -lt $until)) { Start-Sleep -Milliseconds 200 }
Running | Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 300
try {
  foreach ($n in $names) { $d = Join-Path $dest "$n.exe"; if (Test-Path $d) { Move-Item -Force $d "$d.old" } }
  foreach ($n in $names) { Move-Item -Force (Join-Path $src "$n.exe") (Join-Path $dest "$n.exe") }
  foreach ($n in $names) { Remove-Item -Force -ErrorAction SilentlyContinue (Join-Path $dest "$n.exe.old") }
  if (Test-Path @KEY@) { Set-ItemProperty -Path @KEY@ -Name DisplayVersion -Value @VERSION@ -ErrorAction SilentlyContinue }
  Set-Content -Path $result -Value ('updated ' + @VERSION@)
} catch {
  foreach ($n in $names) {
    $d = Join-Path $dest "$n.exe"
    if (Test-Path "$d.old") { Remove-Item -Force -ErrorAction SilentlyContinue $d; Move-Item -Force "$d.old" $d -ErrorAction SilentlyContinue }
  }
  Set-Content -Path $result -Value ('failed ' + @VERSION@ + ' ' + $_.Exception.Message)
}
Start-Process (Join-Path $dest 'tinyclient.exe')
"#
    .replace("@DEST@", &quoted(&dest.display().to_string()))
    .replace("@SRC@", &quoted(&payload.display().to_string()))
    .replace("@RESULT@", &quoted(&result.display().to_string()))
    .replace("@KEY@", &quoted(WINDOWS_APP_KEY))
    .replace("@VERSION@", &quoted(version))
    .replace("@PID@", &pid.to_string())
}

fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

fn http_text(url: &str, api: bool) -> Option<String> {
    let bytes = curl(url, None, if api { 20 } else { 60 }, api).ok()?;
    if bytes.len() > 2 * 1024 * 1024 {
        return None;
    }
    String::from_utf8(bytes).ok()
}

fn http_file(url: &str, path: &Path) -> Result<(), ()> {
    curl(url, Some(path), 180, false)
        .map(|_| ())
        .map_err(|_| ())
}

fn curl(url: &str, out: Option<&Path>, seconds: u64, api: bool) -> io::Result<Vec<u8>> {
    let mut cmd = Command::new(if cfg!(windows) { "curl.exe" } else { "curl" });
    cmd.args([
        "--fail",
        "--silent",
        "--show-error",
        "--location",
        "--max-time",
        &seconds.to_string(),
    ]);
    cmd.arg("--user-agent")
        .arg(format!("tinyclient/{}", current()));
    if api {
        cmd.arg("--header")
            .arg("Accept: application/vnd.github+json");
    }
    if let Some(path) = out {
        cmd.arg("--output").arg(path);
    }
    cmd.arg(url);
    cmd.stdin(std::process::Stdio::null());
    let output = cmd.output()?;
    if !output.status.success() {
        return Err(io::Error::other("the update could not be fetched"));
    }
    Ok(output.stdout)
}

fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0xf) as usize] as char);
    }
    out
}

struct Saved {
    shown: Shown,
    page: String,
    checked: i64,
}

fn read_state(path: &Path) -> Saved {
    let text = fs::read_to_string(path).unwrap_or_default();
    parse_state(&text)
}

fn write_state(dir: &Path, shown: &Shown, page: &str, checked: i64) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let (kind, version) = match shown {
        Shown::Quiet => ("quiet", String::new()),
        Shown::Available { version, .. } => ("available", version.clone()),
        Shown::Ready { version } => ("ready", version.clone()),
        Shown::Failed { version } => ("failed", version.clone()),
    };
    let text = format!("checked\t{checked}\nshown\t{kind}\nversion\t{version}\npage\t{page}\n");
    fs::write(dir.join("state"), text)
}

fn parse_state(text: &str) -> Saved {
    let mut checked = 0i64;
    let mut kind = String::new();
    let mut version = String::new();
    let mut page = RELEASES.to_owned();
    for line in text.lines() {
        let mut fields = line.split('\t');
        match (fields.next(), fields.next()) {
            (Some("checked"), Some(value)) => checked = value.parse().unwrap_or(0),
            (Some("shown"), Some(value)) => kind = value.to_owned(),
            (Some("version"), Some(value)) => version = value.to_owned(),
            (Some("page"), Some(value)) if value.starts_with("https://") => page = value.to_owned(),
            _ => {}
        }
    }
    let shown = match kind.as_str() {
        "available" if !version.is_empty() => Shown::Available {
            version,
            page: page.clone(),
        },
        "ready" if !version.is_empty() => Shown::Ready { version },
        "failed" if !version.is_empty() => Shown::Failed { version },
        _ => Shown::Quiet,
    };
    Saved {
        shown,
        page,
        checked,
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// `latest` is a newer release than `current`. A pre-release is older than the same numbers
/// with no suffix. The leading `v` of a tag is ignored.
pub fn newer(latest: &str, current: &str) -> bool {
    cmp_version(latest, current).is_gt()
}

/// Whether `latest` is offered to this copy. A newer version always is. The same version is only
/// when GitHub's compare of the release's tag with this build's commit said `behind`: this build
/// is from before the tag. Anything else (the same commit, a build past the tag or off to the
/// side of it, a commit GitHub does not have, no answer) says nothing, as an older release does.
fn offered(latest: &str, current: &str, compared: Option<&str>) -> bool {
    newer(latest, current) || (cmp_version(latest, current).is_eq() && compared == Some("behind"))
}

/// GitHub's word for how `commit` stands to `tag`: `identical`, `ahead`, `behind` or `diverged`.
/// None without a commit, or when GitHub does not answer.
fn compare(tag: &str, commit: &str) -> Option<String> {
    let plain = |text: &str| {
        !text.is_empty() && text.len() <= 64 && text.chars().all(|c| c.is_ascii_alphanumeric() || "._-+".contains(c))
    };
    if !plain(tag) || !plain(commit) {
        return None;
    }
    // One commit per page: a build behind the tag has none of its own to list.
    let body = http_text(&format!("{COMPARE}/{tag}...{commit}?per_page=1"), true)?;
    compare_status(&body).map(str::to_owned)
}

/// The compare's top-level `status`. Each changed file has a `status` of its own, but never one
/// of these four words.
fn compare_status(json: &str) -> Option<&str> {
    let mut rest = json;
    while let Some(at) = rest.find("\"status\"") {
        rest = &rest[at..];
        if let Some(status) = json_string(rest, "status") {
            if ["identical", "ahead", "behind", "diverged"].contains(&status) {
                return Some(status);
            }
        }
        rest = &rest["\"status\"".len()..];
    }
    None
}

fn cmp_version(left: &str, right: &str) -> std::cmp::Ordering {
    let (left, right) = (split_version(left), split_version(right));
    let len = left.numbers.len().max(right.numbers.len());
    for index in 0..len {
        let order = left
            .numbers
            .get(index)
            .copied()
            .unwrap_or(0)
            .cmp(&right.numbers.get(index).copied().unwrap_or(0));
        if order.is_ne() {
            return order;
        }
    }
    match (&left.pre, &right.pre) {
        (None, None) => std::cmp::Ordering::Equal,
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (Some(left), Some(right)) => left.cmp(right),
    }
}

struct Version {
    numbers: Vec<u64>,
    pre: Option<String>,
}

fn split_version(text: &str) -> Version {
    let text = text.trim().trim_start_matches('v');
    let (numbers, pre) = text
        .split_once('-')
        .map(|(numbers, pre)| (numbers, Some(pre.to_owned())))
        .unwrap_or((text, None));
    Version {
        numbers: numbers
            .split('.')
            .map(|part| part.parse().unwrap_or(0))
            .collect(),
        pre,
    }
}

fn parse_release(json: &str) -> Option<Release> {
    if json.len() > 2 * 1024 * 1024 {
        return None;
    }
    let tag = json_string(json, "tag_name")?.to_owned();
    let version = tag.trim_start_matches('v').to_owned();
    if version.is_empty() || version.len() > 40 {
        return None;
    }
    let page = json_string(json, "html_url")
        .filter(|url| url.starts_with("https://"))
        .unwrap_or(RELEASES)
        .to_owned();
    let assets = asset_pairs(json);
    let suffix = archive_suffix()?;
    let exact = archive_name()?;
    let archive = assets
        .iter()
        .find(|(name, _)| name == exact)
        .or_else(|| assets.iter().find(|(name, _)| name.ends_with(suffix)))?;
    let sums = assets.iter().find(|(name, _)| name == "SHA256SUMS")?;
    Some(Release {
        tag,
        version,
        page,
        archive: archive.0.clone(),
        archive_url: archive.1.clone(),
        sums_url: sums.1.clone(),
    })
}

/// Asset name is the first `"name"` after `"assets"`, and after each download url: the
/// uploader object later in the same asset also has a `"name"`, and it is not the file.
fn asset_pairs(json: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    let mut rest = json.split("\"assets\"").nth(1).unwrap_or("");
    while let Some(at) = rest.find("\"browser_download_url\"") {
        let url = json_string(&rest[at..], "browser_download_url").unwrap_or_default();
        let name = json_string(&rest[..at], "name").unwrap_or("").to_owned();
        if !name.is_empty() && url.starts_with("https://") {
            pairs.push((name, url.to_owned()));
        }
        rest = &rest[at + "\"browser_download_url\"".len()..];
    }
    pairs
}

fn sum_for<'a>(sums: &'a str, name: &str) -> Option<&'a str> {
    for line in sums.lines() {
        let mut parts = line.split_whitespace();
        let (Some(hash), Some(file)) = (parts.next(), parts.next()) else {
            continue;
        };
        let file = file.trim_start_matches('*');
        if file == name && hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()) {
            return Some(hash);
        }
    }
    None
}

fn json_string<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let pattern = format!("\"{key}\"");
    let at = json.find(&pattern)?;
    let after = json[at + pattern.len()..].trim_start();
    let after = after.strip_prefix(':')?.trim_start();
    let raw = after.strip_prefix('"')?;
    let end = raw.find('"')?;
    Some(&raw[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_newer_release_is_newer_and_a_pre_release_is_not() {
        assert!(newer("0.3.0", "0.2.1"));
        assert!(newer("v0.3.0", "0.2.1"));
        assert!(!newer("0.2.1", "0.2.1"));
        assert!(!newer("0.2.0", "0.2.1"));
        assert!(!newer("0.3.0-beta", "0.3.0"));
        assert!(newer("0.3.0", "0.3.0-beta"));
        assert!(newer("0.3.0-beta.2", "0.3.0-beta.1"));
        assert!(newer("1.0", "0.9.9"));
    }

    #[test]
    fn the_same_version_is_offered_only_to_a_build_behind_the_tag() {
        assert!(offered("0.2.15", "0.2.15", Some("behind")));
        assert!(offered("v0.2.15", "0.2.15", Some("behind")));
        for compared in [None, Some("identical"), Some("ahead"), Some("diverged"), Some("nonsense")] {
            assert!(!offered("0.2.15", "0.2.15", compared));
        }
        // A newer release does not need the compare, and an older one is never offered by it.
        assert!(offered("0.2.16", "0.2.15", None));
        assert!(offered("0.2.16", "0.2.15", Some("ahead")));
        assert!(!offered("0.2.14", "0.2.15", Some("behind")));
        assert!(!offered("0.2.15-beta", "0.2.15", Some("behind")));
    }

    #[test]
    fn the_compare_status_is_the_top_level_one() {
        let json = r#"{
            "base_commit": {"commit": {"message": "Say \"status\": \"behind\" in a message"}},
            "status": "ahead",
            "ahead_by": 1,
            "files": [{"filename": "a.rs", "status": "modified"}]
        }"#;
        assert_eq!(compare_status(json), Some("ahead"));
        assert_eq!(compare_status(r#"{"status":"behind","behind_by":3}"#), Some("behind"));
        assert_eq!(compare_status(r#"{"files":[{"status":"modified"}]}"#), None);
        assert_eq!(compare_status(r#"{"message":"Not Found","status":"404"}"#), None);
    }

    #[test]
    fn a_compare_is_not_asked_without_a_plain_tag_and_commit() {
        assert_eq!(compare("v0.2.15", ""), None);
        assert_eq!(compare("v0.2.15/../x", "5d854b4"), None);
        assert_eq!(compare("", "5d854b4"), None);
    }

    #[test]
    fn the_build_commit_is_a_full_sha_or_nothing() {
        let sha = commit();
        assert!(sha.is_empty() || (sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit())));
        assert_eq!(short_commit().map(str::len), (!sha.is_empty()).then_some(7));
    }

    #[test]
    fn the_release_json_picks_this_machines_archive_and_the_sums() {
        let json = r#"{
            "tag_name": "v0.9.0",
            "html_url": "https://github.com/itz4blitz/tinyclient/releases/tag/v0.9.0",
            "author": {"html_url": "https://github.com/itz4blitz"},
            "assets": [
                {"name": "tinyclient-v0.9.0-macos.zip", "uploader": {"name": "not-the-file"}, "browser_download_url": "https://github.com/itz4blitz/tinyclient/releases/download/v0.9.0/tinyclient-v0.9.0-macos.zip"},
                {"name": "tinyclient-macos.zip", "uploader": {"name": "not-the-file"}, "browser_download_url": "https://github.com/itz4blitz/tinyclient/releases/download/v0.9.0/tinyclient-macos.zip"},
                {"name": "tinyclient-linux-x86_64.tar.gz", "browser_download_url": "https://github.com/itz4blitz/tinyclient/releases/download/v0.9.0/tinyclient-linux-x86_64.tar.gz"},
                {"name": "tinyclient-windows-x86_64.zip", "browser_download_url": "https://github.com/itz4blitz/tinyclient/releases/download/v0.9.0/tinyclient-windows-x86_64.zip"},
                {"name": "SHA256SUMS", "browser_download_url": "https://example.invalid/SHA256SUMS"}
            ]
        }"#;
        let release = parse_release(json).expect("parsed");
        assert_eq!(release.tag, "v0.9.0");
        assert_eq!(release.version, "0.9.0");
        assert!(release.page.contains("/releases/tag/v0.9.0"));
        let name = archive_name().expect("this test machine has a release archive");
        assert_eq!(release.archive, name);
        assert!(release.archive_url.contains(name));
        assert!(release.sums_url.ends_with("SHA256SUMS"));
    }

    #[test]
    fn a_versioned_archive_is_used_when_the_stable_name_is_absent() {
        let suffix = archive_suffix().unwrap();
        let json = format!(
            r#"{{"tag_name":"v2.0.0","html_url":"https://github.com/itz4blitz/tinyclient/releases/tag/v2.0.0","assets":[
            {{"name":"tinyclient-v2.0.0{suffix}","browser_download_url":"https://example.invalid/archive"}},
            {{"name":"SHA256SUMS","browser_download_url":"https://example.invalid/SHA256SUMS"}}]}}"#
        );
        let release = parse_release(&json).expect("parsed");
        assert!(release.archive.ends_with(suffix));
    }

    #[test]
    fn the_checksum_line_names_the_archive() {
        let sums = "abc\n0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef  tinyclient-macos.zip\n";
        assert_eq!(
            sum_for(sums, "tinyclient-macos.zip"),
            Some("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
        );
        assert_eq!(sum_for(sums, "other"), None);
        assert_eq!(sum_for("dead beef", "tinyclient-macos.zip"), None);
    }

    #[test]
    fn state_round_trips_and_a_broken_line_is_quiet() {
        let saved = parse_state(
            "checked\t100\nshown\tready\nversion\t0.9.0\npage\thttps://example.invalid/notes\n",
        );
        assert_eq!(saved.checked, 100);
        assert_eq!(
            saved.shown,
            Shown::Ready {
                version: "0.9.0".into()
            }
        );
        assert_eq!(saved.page, "https://example.invalid/notes");
        assert_eq!(parse_state("shown\tnope\n").shown, Shown::Quiet);
    }

    #[test]
    fn the_bundle_is_recognised_and_a_target_tree_or_the_nix_store_is_not_an_install() {
        let app = Path::new("/Applications/tinyclient.app/Contents/MacOS/tinyclient");
        assert_eq!(
            mac_bundle(app),
            Some(PathBuf::from("/Applications/tinyclient.app"))
        );
        assert_eq!(mac_bundle(Path::new("/usr/local/bin/tinyclient")), None);
        assert!(!replaceable(Path::new("/src/target/release/tinyclient")));
        assert!(!replaceable(Path::new(
            "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-tinyclient-0.2.5/lib/tinyclient/tinyclient"
        )));
        assert!(replaceable(Path::new(
            "/home/me/.local/lib/tinyclient/tinyclient"
        )));
    }

    #[test]
    fn the_windows_swap_waits_for_the_folder_rolls_back_and_always_reopens() {
        let script = windows_script(
            42,
            Path::new(r"C:\Users\o'brien\AppData\Roaming\tinyclient\update\staged"),
            Path::new(r"C:\Users\o'brien\AppData\Local\Programs\tinyclient"),
            "0.9.0",
            Path::new(r"C:\Users\o'brien\AppData\Roaming\tinyclient\update\swap-result"),
        );
        assert!(script.contains("Wait-Process -Id 42 "));
        assert!(script.contains(r"$dest = 'C:\Users\o''brien\AppData\Local\Programs\tinyclient'"));
        // What still runs from the folder is stopped before anything is moved.
        assert!(script.find("Stop-Process").unwrap() < script.find("Move-Item").unwrap());
        assert!(script.contains(r#""$d.old""#));
        assert!(script.contains("'failed ' + '0.9.0'"));
        assert!(script.contains("{8C1B5E0A-4F3D-4C1E-9B7A-2D6E5F1A7C3B}_is1"));
        // Reopening is outside the try, so it happens after a failure too.
        assert!(script.trim_end().ends_with("Start-Process (Join-Path $dest 'tinyclient.exe')"));
        assert!(!script.contains('@'));
    }

    #[test]
    fn a_failed_swap_is_read_back_and_a_good_one_is_not() {
        assert_eq!(failed_swap("failed 0.9.0 Access is denied."), Some("0.9.0".into()));
        assert_eq!(failed_swap("updated 0.9.0"), None);
        assert_eq!(failed_swap(""), None);
    }

    #[test]
    fn a_folder_this_user_can_write_is_writable_and_the_probe_is_gone() {
        let dir = std::env::temp_dir().join(format!("tinyclient-writable-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        assert!(writable(&dir));
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        let _ = fs::remove_dir_all(&dir);
        assert!(!writable(&dir));
    }

    #[test]
    fn a_sha256_of_known_bytes_matches() {
        let dir =
            std::env::temp_dir().join(format!("tinyclient-update-test-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("bytes");
        fs::write(&path, b"abc").unwrap();
        // The well-known SHA-256 of the three bytes `abc`.
        assert_eq!(
            sha256_file(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
