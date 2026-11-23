//! Native pickers and inbound shares deliver app-owned copies. Only outbox files may be
//! handed to the composer, whose existing staging rules decide what the conversation can send.

use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
pub(crate) struct Items {
    pub(crate) text: String,
    pub(crate) files: Vec<PathBuf>,
    pub(crate) destination: Option<String>,
}

impl Drop for Items {
    fn drop(&mut self) {
        for file in &self.files {
            let _ = std::fs::remove_file(file);
        }
    }
}

impl Items {
    pub(crate) fn decode(json: &str, outbox: &Path) -> Result<Self, &'static str> {
        let data: serde_json::Value = serde_json::from_str(json).map_err(|_| "Couldn't read the shared items.")?;
        if data.get("error").and_then(serde_json::Value::as_bool) == Some(true) {
            return Err("Couldn't copy the shared items.");
        }
        let mut items = Self::default();
        if let Some(files) = data.get("files").and_then(serde_json::Value::as_array).filter(|files| !files.is_empty()) {
            let root = outbox.canonicalize().map_err(|_| "Couldn't open the attachment staging folder.")?;
            for path in files.iter().take(32).filter_map(serde_json::Value::as_str).map(Path::new) {
                let Ok(metadata) = path.symlink_metadata() else { continue };
                let Ok(real) = path.canonicalize() else { continue };
                if metadata.is_file() && real.starts_with(&root) && !items.files.contains(&real) {
                    items.files.push(real);
                }
            }
            if items.files.len() != files.len() {
                return Err("Some shared files couldn't be read.");
            }
        }
        items.text = data.get("text").and_then(serde_json::Value::as_str).unwrap_or_default().trim().to_owned();
        items.destination = data.get("destination").and_then(serde_json::Value::as_str).filter(|id| !id.is_empty()).map(str::to_owned);
        Ok(items)
    }

    pub(crate) fn empty(&self) -> bool {
        self.text.is_empty() && self.files.is_empty()
    }
}

pub(crate) fn draft_with_share(draft: &str, shared: &str) -> String {
    if shared.is_empty() {
        draft.to_owned()
    } else if draft.trim().is_empty() {
        shared.to_owned()
    } else {
        format!("{draft}\n{shared}")
    }
}

pub(crate) fn destination_id(key: &protocol::Key) -> Option<String> {
    use sha2::{Digest, Sha256};
    let data = serde_json::to_vec(key).ok()?;
    // Android shortcut ids also travel in intents. Hash the tuple to keep ids short and avoid
    // exposing account ids in notifications or shortcut metadata.
    Some(format!("{:x}", Sha256::digest(data)))
}

pub(crate) fn direct_destination<'a>(rows: &'a [protocol::Conversation], id: Option<&str>) -> Option<&'a protocol::Conversation> {
    let id = id?;
    rows.iter().find(|row| destination_id(&row.key).as_deref() == Some(id))
}

#[cfg(any(target_os = "android", target_os = "ios"))]
mod native {
    use super::*;
    use futures_channel::oneshot;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    type ResultItems = Result<Items, &'static str>;
    static INCOMING: Mutex<VecDeque<ResultItems>> = Mutex::new(VecDeque::new());
    static LINKS: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
    static PICK: Mutex<Option<oneshot::Sender<ResultItems>>> = Mutex::new(None);

    pub(crate) fn receive(kind: i32, data: &str) {
        if kind == 2 {
            if protocol::parse_invite_link(data).is_some() || protocol::parse_room_link(data).is_some() || protocol::parse_message_link(data).is_some() {
                let mut links = LINKS.lock().unwrap_or_else(|err| err.into_inner());
                if links.len() < 8 && !links.iter().any(|link| link == data) {
                    links.push_back(data.to_owned());
                }
            }
            return;
        }
        let items = Items::decode(data, &protocol::support_dir().join("outbox"));
        if kind == 1 {
            let mut queue = INCOMING.lock().unwrap_or_else(|err| err.into_inner());
            if queue.len() < 8 && !items.as_ref().is_ok_and(Items::empty) {
                queue.push_back(items);
            }
        } else if let Some(waiter) = PICK.lock().unwrap_or_else(|err| err.into_inner()).take() {
            let _ = waiter.send(items);
        }
    }

    pub(crate) fn next_share() -> Option<ResultItems> {
        INCOMING.lock().unwrap_or_else(|err| err.into_inner()).pop_front()
    }

    pub(crate) fn next_link() -> Option<String> {
        LINKS.lock().unwrap_or_else(|err| err.into_inner()).pop_front()
    }

    pub(crate) fn pick() -> Option<oneshot::Receiver<ResultItems>> {
        let (send, receive) = oneshot::channel();
        {
            let mut pending = PICK.lock().unwrap_or_else(|err| err.into_inner());
            if pending.is_some() { return None; }
            *pending = Some(send);
        }
        if !open_picker() {
            PICK.lock().unwrap_or_else(|err| err.into_inner()).take();
            return None;
        }
        Some(receive)
    }

    #[cfg(target_os = "android")]
    fn open_picker() -> bool {
        use crate::mobile::android::jvm;
        jvm::call("open the attachment picker", |env| {
            jvm::call_static(env, "dev.tinyclient.app.ShareItems", "pick", "()Z", &[])?.z()
        }).unwrap_or(false)
    }

    #[cfg(target_os = "ios")]
    fn open_picker() -> bool {
        extern "C" { fn tinyclient_native_pick_attachments(); }
        // SAFETY: the Swift entry point only schedules UIKit work on the main queue.
        unsafe { tinyclient_native_pick_attachments() };
        true
    }

    #[cfg(target_os = "ios")]
    #[no_mangle]
    pub unsafe extern "C" fn tinyclient_native_share_received(kind: i32, json: *const std::ffi::c_char) {
        if json.is_null() { return; }
        // SAFETY: Swift provides a NUL-terminated UTF-8 string valid for this call.
        if let Ok(data) = unsafe { std::ffi::CStr::from_ptr(json) }.to_str() {
            receive(kind, data);
        }
    }

    #[cfg(target_os = "android")]
    #[no_mangle]
    pub extern "system" fn Java_dev_tinyclient_app_ShareItems_nativeReceived(
        mut env: jni::JNIEnv,
        _class: jni::objects::JClass,
        kind: jni::sys::jint,
        json: jni::objects::JString,
    ) {
        if let Ok(data) = env.get_string(&json) {
            receive(kind, &String::from(data));
        }
    }
}

#[cfg(any(target_os = "android", target_os = "ios"))]
pub(crate) use native::{next_link, next_share, pick};

#[cfg(target_os = "android")]
pub(crate) fn share_file(path: &str, name: &str) {
    use crate::mobile::android::jvm;
    use jni::objects::JValue;
    let _ = jvm::call("share the attachment", |env| {
        let path = env.new_string(path)?;
        let name = env.new_string(name)?;
        jvm::call_static(env, "dev.tinyclient.app.ShareItems", "share",
            "(Ljava/lang/String;Ljava/lang/String;)V", &[JValue::Object(&path), JValue::Object(&name)])?;
        Ok(())
    });
}

#[cfg(target_os = "ios")]
pub(crate) fn share_file(path: &str, name: &str) {
    use std::ffi::{c_char, CString};
    extern "C" { fn tinyclient_native_share_file(path: *const c_char, name: *const c_char); }
    if let (Ok(path), Ok(name)) = (CString::new(path), CString::new(name)) {
        // SAFETY: Swift copies these two NUL-terminated strings before it schedules the share.
        unsafe { tinyclient_native_share_file(path.as_ptr(), name.as_ptr()) };
    }
}

#[cfg(target_os = "android")]
pub(crate) fn note_recent(key: &protocol::Key, title: &str) {
    use crate::mobile::android::jvm;
    use jni::objects::JValue;
    let Some(id) = destination_id(key) else { return };
    if title.trim().is_empty() { return; }
    let _ = jvm::call("publish recent share destination", |env| {
        let id = env.new_string(id)?;
        let title = env.new_string(title)?;
        jvm::call_static(env, "dev.tinyclient.app.ShareItems", "recent",
            "(Ljava/lang/String;Ljava/lang/String;)V", &[JValue::Object(&id), JValue::Object(&title)])?;
        Ok(())
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_text_and_rejects_paths_outside_the_staging_folder() {
        let directory = std::env::temp_dir().join(format!("tinyclient-share-test-{}", std::process::id()));
        let staging = directory.join("outbox");
        std::fs::create_dir_all(&staging).unwrap();
        let path = staging.join("photo.jpg");
        let private = directory.join("private");
        std::fs::write(&path, b"photo").unwrap();
        std::fs::write(&private, b"private").unwrap();
        let valid = serde_json::json!({ "text": " https://example.com ", "files": [&path] });
        let items = Items::decode(&valid.to_string(), &staging).unwrap();
        assert!(!items.empty());
        assert_eq!(items.text, "https://example.com");
        assert_eq!(items.files, vec![path.canonicalize().unwrap()]);
        drop(items);
        assert!(!path.exists());
        let invalid = serde_json::json!({ "files": [&private] });
        assert!(Items::decode(&invalid.to_string(), &staging).is_err());
        assert!(private.is_file());
        std::fs::remove_file(private).unwrap();
        std::fs::remove_dir(staging).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn share_text_appends_to_an_existing_draft() {
        assert_eq!(draft_with_share("My draft", "shared link"), "My draft\nshared link");
        assert_eq!(draft_with_share("", "shared link"), "shared link");
        assert_eq!(draft_with_share("My draft", ""), "My draft");
    }

    #[test]
    fn direct_share_targets_include_the_account() {
        let key = |account: &str| protocol::Key { network: protocol::Network::Tiny, account: account.into(), id: "same".into() };
        let row = |account| protocol::Conversation::new(key(account), protocol::ConversationKind::Direct, "Same conversation title");
        let rows = [row("one"), row("two")];
        let id = destination_id(&key("two"));
        assert_eq!(direct_destination(&rows, id.as_deref()).map(|row| &row.key), Some(&key("two")));
        assert!(direct_destination(&rows[..1], id.as_deref()).is_none());
        assert!(direct_destination(&rows, None).is_none());
    }
}
