//! Open at login (G1). The switch in Settings › General writes the entry the system reads at
//! sign-in and removes it again, so it takes effect without reinstalling. Each system has its
//! own place for it: the per-user Run key on Windows, a LaunchAgent on macOS, an XDG autostart
//! entry on Linux. The entry always names the program that is running now, and launch
//! rewrites it while the switch is on (`Shell::repair_sign_in_entry`), so a copy that moved or
//! was replaced — a portable .zip, an AppImage, a Nix store path — keeps starting. Task
//! Manager's Startup tab can still disable the entry: its word lives beside ours and is never
//! touched here. A phone starts nothing at sign-in, so the row is absent there
//! (`rows::absent`) and nothing is written.
//!
//! The second launch at sign-in is held off the first window by the same handoff a link uses
//! (`invite_ipc`), so switching on cannot start tinyclient twice.
//!
//! What the entry says is built by pure functions and tested here. Applying touches the
//! registry or a file in the home directory, so it runs off the main thread, and the real
//! registration is driven by the lab's scenario (`TINYCLIENT_PROVE=open-at-login`), never by
//! a unit test.

use std::path::{Path, PathBuf};

/// Whether this system keeps a sign-in list tinyclient can join.
pub(crate) fn supported() -> bool {
    cfg!(any(windows, target_os = "macos", target_os = "linux"))
}

/// The program the entry names: what is running now. On Linux an AppImage runs from a private
/// mount that is gone once it quits, and the `APPIMAGE` variable names the real file.
fn current_program() -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    if let Some(appimage) = std::env::var_os("APPIMAGE") {
        let path = PathBuf::from(appimage);
        if path.is_absolute() {
            return Some(path);
        }
    }
    std::env::current_exe().ok()
}

/// Writes or removes the entry now. Called off the main thread: it touches the registry on
/// Windows and a file in the home directory elsewhere. Why it failed goes to the log; what
/// the person sees is the row's own line (`failure_words`).
pub(crate) fn apply(on: bool) -> Result<(), &'static str> {
    if !supported() {
        return Ok(());
    }
    let Some(exe) = current_program() else {
        return Err("the running program's path is not known, so no sign-in entry was written");
    };
    if let Err(err) = apply_entry(on, &exe) {
        tracing::warn!("tinyclient: the sign-in entry could not be {}: {err}", if on { "written" } else { "removed" });
        return Err(if on { "couldn't write the sign-in entry" } else { "couldn't remove the sign-in entry" });
    }
    Ok(())
}

/// Whether an entry is there now, whatever it names.
pub(crate) fn entry_present() -> bool {
    supported() && entry_present_os()
}

/// Whether the entry names the program that is running now.
pub(crate) fn registered() -> bool {
    supported() && current_program().is_some_and(|exe| entry_matches(&exe))
}

#[cfg(windows)]
fn apply_entry(on: bool, exe: &Path) -> Result<(), String> {
    windows_run::apply(on, exe)
}

#[cfg(target_os = "macos")]
fn apply_entry(on: bool, exe: &Path) -> Result<(), String> {
    apply_file(on, exe, plist_text)
}

#[cfg(target_os = "linux")]
fn apply_entry(on: bool, exe: &Path) -> Result<(), String> {
    apply_file(on, exe, desktop_text)
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn apply_entry(_on: bool, _exe: &Path) -> Result<(), String> {
    Ok(())
}

/// Writes or removes the entry file. Nothing is started or stopped now: the switch is about
/// signing in, and launchd and the desktop read the file at the next sign-in.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn apply_file(on: bool, exe: &Path, text: fn(&Path) -> String) -> Result<(), String> {
    let Some(path) = entry_path() else {
        return Err("the home directory is not known, so no sign-in entry was written".to_owned());
    };
    let done = if on {
        std::fs::create_dir_all(path.parent().unwrap_or(&path)).and_then(|()| std::fs::write(&path, text(exe)))
    } else {
        // Already gone: switching off twice still switched off.
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err),
        }
    };
    done.map_err(|err| err.to_string())
}

#[cfg(windows)]
fn entry_present_os() -> bool {
    windows_run::value().is_ok_and(|value| value.is_some())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn entry_present_os() -> bool {
    entry_path().is_some_and(|path| path.exists())
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn entry_present_os() -> bool {
    false
}

#[cfg(windows)]
fn entry_matches(exe: &Path) -> bool {
    windows_run::value().is_ok_and(|value| value.as_deref() == Some(&run_value(exe)))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn entry_matches(exe: &Path) -> bool {
    entry_path().and_then(|path| std::fs::read_to_string(path).ok()).is_some_and(|text| text == entry_text(exe))
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn entry_matches(_exe: &Path) -> bool {
    false
}

/// The home directory: `$HOME`, absolute. A GUI session sets it on macOS and Linux; without
/// it there is no place the entry could go, so the switch fails rather than guessing.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from).filter(|home| home.is_absolute())
}

/// The entry's file, when the system keeps one: the LaunchAgent, or the XDG autostart entry.
#[cfg(target_os = "macos")]
fn entry_path() -> Option<PathBuf> {
    home().map(|home| home.join("Library").join("LaunchAgents").join("dev.tinyclient.app.plist"))
}

#[cfg(target_os = "linux")]
fn entry_path() -> Option<PathBuf> {
    autostart_dir(std::env::var_os("XDG_CONFIG_HOME").as_deref(), home().as_deref()).map(|dir| dir.join("dev.tinyclient.app.desktop"))
}

#[cfg(target_os = "macos")]
fn entry_text(exe: &Path) -> String {
    plist_text(exe)
}

#[cfg(target_os = "linux")]
fn entry_text(exe: &Path) -> String {
    desktop_text(exe)
}

/// The Run key's data: the quoted program. Quotes because a path may hold spaces.
#[cfg(any(windows, test))]
fn run_value(exe: &Path) -> String {
    format!("\"{}\"", exe.display())
}

/// The LaunchAgent. Its file name is its label, and `RunAtLoad` starts it at sign-in.
/// `KeepAlive` is off, so quitting the window stays quitting.
#[cfg(any(target_os = "macos", test))]
fn plist_text(exe: &Path) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n\t<key>Label</key>\n\t<string>dev.tinyclient.app</string>\n\t<key>ProgramArguments</key>\n\t<array>\n\t\t<string>{}</string>\n\t</array>\n\t<key>RunAtLoad</key>\n\t<true/>\n</dict>\n</plist>\n",
        xml(&exe.display().to_string())
    )
}

/// `&`, `<` and `>` escaped, as a plist is XML.
#[cfg(any(target_os = "macos", test))]
fn xml(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// The XDG autostart folder: `$XDG_CONFIG_HOME/autostart` when that is an absolute path, else
/// the home's `.config/autostart` (freedesktop autostart spec).
#[cfg(any(target_os = "linux", test))]
fn autostart_dir(xdg: Option<&std::ffi::OsStr>, home: Option<&Path>) -> Option<PathBuf> {
    let xdg = xdg.and_then(|dir| {
        let dir = PathBuf::from(dir);
        dir.is_absolute().then_some(dir)
    });
    match xdg {
        Some(dir) => Some(dir.join("autostart")),
        None => home.map(|home| home.join(".config").join("autostart")),
    }
}

/// The autostart entry, with the fields the shipped .desktop carries
/// (`packaging/linux/dev.tinyclient.app.desktop`) and `Exec` naming the running file. No
/// field code: an autostart entry starts nothing but the app.
#[cfg(any(target_os = "linux", test))]
fn desktop_text(exe: &Path) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=tinyclient\nGenericName=Chat Client\nComment=Slack, Discord, Telegram, Teams and Signal in one small native window\nExec={}\nIcon=dev.tinyclient.app\nTerminal=false\nStartupNotify=true\nStartupWMClass=dev.tinyclient.app\nCategories=Network;InstantMessaging;Chat;\n",
        shell_quote(&exe.display().to_string())
    )
}

/// One word of a freedesktop `Exec`: quoted, with the characters the line treats specially
/// escaped, so a path with a space or a `$` stays one word.
#[cfg(any(target_os = "linux", test))]
fn shell_quote(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    for c in text.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    quoted.push('"');
    quoted
}

/// The per-user Run key: `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, one value per
/// program that starts at sign-in. The uninstaller clears this value too
/// (`packaging/windows/tinyclient.iss`). A disable in Task Manager's Startup tab is a word in
/// `StartupApproved` beside it and is never touched here, so the user's turn-off wins.
#[cfg(windows)]
mod windows_run {
    use std::ffi::OsStr;
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, HANDLE};
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ,
    };

    const SUBKEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
    const VALUE: &str = "tinyclient";

    fn wide(text: &str) -> Vec<u16> {
        OsStr::new(text).encode_wide().chain(std::iter::once(0)).collect()
    }

    /// Opens the Run key for `access`. The key is the caller's to close, and every caller does.
    fn open(access: u32) -> io::Result<HANDLE> {
        let (subkey, mut key) = (wide(SUBKEY), std::ptr::null_mut());
        // SAFETY: the subkey lives to the end of the call; the handle it fills is closed by
        // the caller on every path.
        let opened = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, access, &mut key) };
        if opened != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(opened as i32));
        }
        Ok(key)
    }

    pub(super) fn apply(on: bool, exe: &Path) -> Result<(), String> {
        let key = open(KEY_SET_VALUE).map_err(|err| err.to_string())?;
        let name = wide(VALUE);
        let done = if on {
            let data = wide(&super::run_value(exe));
            // SAFETY: name and data live to the end of the call; the length counts the bytes,
            // the terminating nul included, as a REG_SZ wants.
            let written = unsafe { RegSetValueExW(key, name.as_ptr(), 0, REG_SZ, data.as_ptr().cast(), (data.len() * 2) as u32) };
            if written != ERROR_SUCCESS {
                Err(written)
            } else {
                Ok(())
            }
        } else {
            // SAFETY: the name lives to the end of the call.
            let deleted = unsafe { RegDeleteValueW(key, name.as_ptr()) };
            if deleted == ERROR_SUCCESS || deleted == ERROR_FILE_NOT_FOUND {
                Ok(())
            } else {
                Err(deleted)
            }
        };
        // SAFETY: the key is ours from `open`, and nothing uses it afterwards.
        let closed = unsafe { RegCloseKey(key) };
        done.map_err(|code| io::Error::from_raw_os_error(code as i32).to_string())
            .and_then(|()| if closed == ERROR_SUCCESS { Ok(()) } else { Err(io::Error::from_raw_os_error(closed as i32).to_string()) })
    }

    /// The value as it is: `None` when it is not there.
    pub(super) fn value() -> io::Result<Option<String>> {
        let key = open(KEY_QUERY_VALUE)?;
        let name = wide(VALUE);
        let mut size = 0u32;
        // SAFETY: this call only asks how many bytes the value holds; the data pointers are
        // null and the type is not read.
        let asked = unsafe { RegQueryValueExW(key, name.as_ptr(), std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut(), &mut size) };
        if asked != ERROR_SUCCESS {
            // SAFETY: the key is ours from `open`.
            let closed = unsafe { RegCloseKey(key) };
            if closed != ERROR_SUCCESS {
                return Err(io::Error::from_raw_os_error(closed as i32));
            }
            if asked == ERROR_FILE_NOT_FOUND {
                return Ok(None);
            }
            return Err(io::Error::from_raw_os_error(asked as i32));
        }
        let mut data = vec![0u8; size as usize];
        let mut read = size;
        // SAFETY: `data` lives to the end of the call and `read` says how many bytes fit.
        let read_back = unsafe { RegQueryValueExW(key, name.as_ptr(), std::ptr::null(), std::ptr::null_mut(), data.as_mut_ptr(), &mut read) };
        // SAFETY: the key is ours from `open`, and nothing uses it afterwards.
        let closed = unsafe { RegCloseKey(key) };
        if closed != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(closed as i32));
        }
        if read_back != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(read_back as i32));
        }
        data.truncate((read as usize).min(data.len()));
        let units: Vec<u16> = data.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])).collect();
        let text: Vec<u16> = units.iter().take_while(|&&unit| unit != 0).copied().collect();
        // A value this program never wrote (lone surrogates) reads as no match.
        Ok(String::from_utf16(&text).ok())
    }
}

/// The lab's open-at-login driver. The row is there and is off by default; the words people
/// use for it find it and the honest line is gone; switching on writes the entry this system
/// reads at sign-in, and switching off removes it again. It writes the real entry, so it
/// starts by switching off: a run interrupted before leaves nothing behind.
pub(crate) fn prove() -> Result<(), &'static str> {
    use super::app::prefs::Choices;
    use super::settings::rows;
    if !supported() {
        return Err("this system keeps no sign-in list");
    }
    if rows::find("G1").is_none() {
        return Err("G1 is not in the settings table");
    }
    if rows::absent("G1") {
        return Err("G1 is absent on a system that keeps a sign-in list");
    }
    if !rows::search("open at login").iter().any(|row| row.id == "G1") {
        return Err("search does not find Open at login");
    }
    for words in ["login", "startup", "launch", "autostart", "sign in"] {
        if rows::not_yet(words).is_some() {
            return Err("search still says there is no open-at-login setting");
        }
        if !rows::search(words).iter().any(|row| row.id == "G1") {
            return Err("the words people use for it do not find Open at login");
        }
    }
    if Choices::default().open_at_login {
        return Err("open at login is on by default");
    }
    apply(false).map_err(|_| "an entry that was already there could not be removed")?;
    if entry_present() {
        return Err("an entry was already there and switching off did not remove it");
    }
    apply(true).map_err(|_| "switching on could not write the entry")?;
    if !registered() {
        return Err("the entry does not name the program that is running");
    }
    apply(false).map_err(|_| "switching off could not remove the entry")?;
    if entry_present() {
        return Err("switching off left the entry behind");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_run_value_quotes_the_program() {
        assert_eq!(run_value(Path::new("C:\\Program Files\\tinyclient\\tinyclient.exe")), "\"C:\\Program Files\\tinyclient\\tinyclient.exe\"");
        assert_eq!(run_value(Path::new("/usr/local/bin/tinyclient")), "\"/usr/local/bin/tinyclient\"");
    }

    #[test]
    fn the_plist_names_this_app_and_starts_at_sign_in() {
        let text = plist_text(Path::new("/Applications/tinyclient.app/Contents/MacOS/tinyclient"));
        assert!(text.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist"), "{text}");
        assert!(text.contains("<key>Label</key>\n\t<string>dev.tinyclient.app</string>"), "{text}");
        assert!(text.contains("<key>RunAtLoad</key>\n\t<true/>"), "{text}");
        // A path with an ampersand stays one XML string.
        assert!(plist_text(Path::new("/tmp/a&b/tinyclient")).contains("<string>/tmp/a&amp;b/tinyclient</string>"));
    }

    #[test]
    fn the_desktop_entry_is_the_shipped_one_with_an_absolute_exec() {
        let text = desktop_text(Path::new("/opt/tinyclient folder/tinyclient"));
        assert!(text.starts_with("[Desktop Entry]\nType=Application\nName=tinyclient\n"), "{text}");
        assert!(text.contains("Exec=\"/opt/tinyclient folder/tinyclient\"\n"), "{text}");
        assert!(text.contains("Terminal=false\n") && text.contains("StartupWMClass=dev.tinyclient.app\n"), "{text}");
        assert!(!text.contains("%u"), "an autostart entry starts nothing but the app");
    }

    #[test]
    fn the_desktop_entry_escapes_what_an_exec_line_treats_specially() {
        let text = desktop_text(Path::new("/opt/a\"b`c$d\\e/tinyclient"));
        assert!(text.contains("Exec=\"/opt/a\\\"b\\`c\\$d\\\\e/tinyclient\"\n"), "{text}");
    }

    #[test]
    fn the_autostart_folder_follows_xdg() {
        // A root that is absolute here too: `/tmp` and `/home` are not absolute on Windows.
        let root = Path::new(if cfg!(windows) { r"C:\" } else { "/" });
        let xdg = root.join("tmp").join("config");
        let home = root.join("home").join("user");
        assert_eq!(autostart_dir(Some(xdg.as_os_str()), Some(&home)), Some(xdg.join("autostart")));
        // A relative XDG_CONFIG_HOME is ignored, as the spec says.
        assert_eq!(autostart_dir(Some(std::ffi::OsStr::new("config")), Some(&home)), Some(home.join(".config").join("autostart")));
        assert_eq!(autostart_dir(None, Some(&home)), Some(home.join(".config").join("autostart")));
        assert_eq!(autostart_dir(None, None), None);
    }
}
