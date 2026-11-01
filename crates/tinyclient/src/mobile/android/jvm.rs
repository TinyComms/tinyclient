//! The Java side, from Rust: the JavaVM and activity android-activity hands `ndk-context`, the
//! app's own classes (`mobile/android/app/src/main/java/dev/tinyclient/app`) found through the
//! activity's class loader, and one way to call them.
//!
//! Threads. mediad and chatd call from their own threads, which are attached to the VM once and
//! stay attached (`attach_current_thread_permanently`; detached when the thread ends). Every call
//! runs in its own local-reference frame, so a call loop that encodes 30 frames a second leaks
//! nothing. A pending Java exception is logged and cleared, and becomes an `Err`.

use jni::objects::{GlobalRef, JClass, JObject, JString, JValue, JValueOwned};
use jni::{JNIEnv, JavaVM};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// The VM, made once from the pointer android-activity left in `ndk-context`.
fn vm() -> Result<&'static JavaVM, String> {
    static VM: OnceLock<Option<JavaVM>> = OnceLock::new();
    VM.get_or_init(|| {
        let context = ndk_context::android_context();
        // SAFETY: android-activity stores the process's JavaVM pointer in ndk-context before
        // `android_main` runs, and it lives as long as the process.
        unsafe { JavaVM::from_raw(context.vm().cast()) }.ok()
    })
    .as_ref()
    .ok_or_else(|| "The phone's Java runtime isn't reachable.".to_owned())
}

/// Runs `work` with a JNI environment for this thread, in a local frame of its own.
pub fn call<T>(what: &str, work: impl FnOnce(&mut JNIEnv) -> jni::errors::Result<T>) -> Result<T, String> {
    let vm = vm()?;
    let mut env = vm.attach_current_thread_permanently().map_err(|err| format!("The phone's Java runtime refused this thread: {err}"))?;
    let out = env.with_local_frame(32, |env| work(env));
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_describe();
        let _ = env.exception_clear();
    }
    out.map_err(|err| {
        log::warn!("tinyclient: {what} failed in Java: {err}");
        format!("The phone couldn't {what}.")
    })
}

/// One of the app's classes by binary name (`dev.tinyclient.app.TinyPlatform`), loaded through
/// the activity's class loader: a thread attached from Rust only sees the system's classes.
pub fn class(env: &mut JNIEnv, name: &'static str) -> jni::errors::Result<GlobalRef> {
    static CLASSES: Mutex<Option<HashMap<&'static str, GlobalRef>>> = Mutex::new(None);
    if let Some(found) = CLASSES.lock().unwrap_or_else(|p| p.into_inner()).as_ref().and_then(|all| all.get(name)) {
        return Ok(found.clone());
    }
    let context = ndk_context::android_context().context();
    // SAFETY: the activity is a global reference android-activity keeps for the process's life.
    let activity = unsafe { JObject::from_raw(context.cast()) };
    let loader = env.call_method(&activity, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])?.l()?;
    let binary = env.new_string(name)?;
    let found = env.call_method(&loader, "loadClass", "(Ljava/lang/String;)Ljava/lang/Class;", &[JValue::Object(&binary)])?.l()?;
    let global = env.new_global_ref(found)?;
    // Not ours to free: from_raw over a global reference must not be deleted as a local.
    std::mem::forget(activity);
    CLASSES.lock().unwrap_or_else(|p| p.into_inner()).get_or_insert_with(HashMap::new).insert(name, global.clone());
    Ok(global)
}

/// A static method of one of the app's classes.
pub fn call_static<'local>(env: &mut JNIEnv<'local>, class_name: &'static str, method: &str, sig: &str, args: &[JValue]) -> jni::errors::Result<JValueOwned<'local>> {
    let class = class(env, class_name)?;
    let class: &JClass = class.as_obj().into();
    env.call_static_method(class, method, sig, args)
}

/// A Java string, or `None` for null.
pub fn string(env: &mut JNIEnv, value: JObject) -> jni::errors::Result<Option<String>> {
    if value.is_null() {
        return Ok(None);
    }
    let text = JString::from(value);
    let out: String = env.get_string(&text)?.into();
    Ok(Some(out))
}

/// The class every platform call goes through.
pub const PLATFORM: &str = "dev.tinyclient.app.TinyPlatform";
pub const AUDIO: &str = "dev.tinyclient.app.CallAudio";
pub const CAMERA: &str = "dev.tinyclient.app.CameraCapture";
pub const SCREEN: &str = "dev.tinyclient.app.ScreenCapture";
pub const ENCODER: &str = "dev.tinyclient.app.AvcEncoder";
pub const DECODER: &str = "dev.tinyclient.app.AvcDecoder";
pub const WEB: &str = "dev.tinyclient.app.WebSession";
pub const AUTH: &str = "dev.tinyclient.app.AuthSession";
