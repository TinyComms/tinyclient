//! What Java calls back into Rust (`TinyPlatform.native*`): camera and screen frames, a sign-in
//! page's navigation decisions, the Tiny sign-in's redirect. Java holds only numbers; the Rust
//! side of each (a sink, a navigation policy, a redirect) waits in a table here under that number.
//!
//! Each native is quick and never calls back into Java synchronously (docs/mobile-platform.md).
//! A panic is caught here: unwinding across the JNI boundary would abort the app.

use jni::objects::{JByteArray, JClass, JString};
use jni::sys::{jint, jlong, jstring};
use jni::JNIEnv;
use mediad::platform::{FrameSink, PixelFormat, VideoFrame};
use protocol::web_sign_in::Navigation;
use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

static NEXT: AtomicI64 = AtomicI64::new(1);

/// A fresh number for something Java will name back.
pub fn next_id() -> i64 {
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// A table of Rust callbacks Java names by number.
pub struct Table<T: Clone>(Mutex<Option<HashMap<i64, T>>>);

impl<T: Clone> Table<T> {
    pub const fn new() -> Self {
        Table(Mutex::new(None))
    }

    pub fn put(&self, id: i64, value: T) {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).get_or_insert_with(HashMap::new).insert(id, value);
    }

    pub fn get(&self, id: i64) -> Option<T> {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).as_ref().and_then(|all| all.get(&id).cloned())
    }

    pub fn take(&self, id: i64) {
        if let Some(all) = self.0.lock().unwrap_or_else(|p| p.into_inner()).as_mut() {
            all.remove(&id);
        }
    }
}

pub type Navigate = Arc<dyn Fn(&str) -> Navigation + Send + Sync>;
pub type Redirected = Arc<dyn Fn(String) + Send + Sync>;

pub static SINKS: Table<FrameSink> = Table::new();
pub static PAGES: Table<Navigate> = Table::new();
pub static AUTHS: Table<Redirected> = Table::new();

#[no_mangle]
pub extern "system" fn Java_dev_tinyclient_app_TinyPlatform_nativeFrame(env: JNIEnv, _class: JClass, sink: jlong, width: jint, height: jint, format: jint, data: JByteArray, timestamp_us: jlong) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let Some(sink) = SINKS.get(sink) else {
            return;
        };
        let Ok(bytes) = env.convert_byte_array(&data) else {
            return;
        };
        let format = match format {
            1 => PixelFormat::Nv12,
            2 => PixelFormat::Rgba,
            3 => PixelFormat::I420,
            _ => PixelFormat::Bgra,
        };
        let frame = VideoFrame { width: width.max(0) as u32, height: height.max(0) as u32, format, data: bytes, timestamp_us: timestamp_us.max(0) as u64 };
        if frame.is_whole() {
            sink(frame);
        } else {
            log::warn!("tinyclient: a {format:?} frame {width}x{height} arrived with the wrong length");
        }
    }));
}

#[no_mangle]
pub extern "system" fn Java_dev_tinyclient_app_TinyPlatform_nativeNavigate(mut env: JNIEnv, _class: JClass, page: jlong, url: JString) -> jstring {
    let answer = catch_unwind(AssertUnwindSafe(|| {
        let url: String = match env.get_string(&url) {
            Ok(text) => text.into(),
            Err(_) => return "A".to_owned(),
        };
        let Some(navigate) = PAGES.get(page) else {
            return "A".to_owned();
        };
        super::wire::navigation_word(navigate(&url))
    }))
    .unwrap_or_else(|_| "A".to_owned());
    env.new_string(answer).map(|s| s.into_raw()).unwrap_or(std::ptr::null_mut())
}

#[no_mangle]
pub extern "system" fn Java_dev_tinyclient_app_TinyPlatform_nativeSelfTest(mut env: JNIEnv, _class: JClass, what: JString) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let Ok(text) = env.get_string(&what) else {
            return;
        };
        let what: String = text.into();
        std::thread::spawn(move || super::selftest::run(&what));
    }));
}

#[no_mangle]
pub extern "system" fn Java_dev_tinyclient_app_TinyPlatform_nativeRedirected(mut env: JNIEnv, _class: JClass, auth: jlong, url: JString) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let Ok(text) = env.get_string(&url) else {
            return;
        };
        let url: String = text.into();
        if let Some(redirected) = AUTHS.get(auth) {
            redirected(url);
        }
    }));
}
