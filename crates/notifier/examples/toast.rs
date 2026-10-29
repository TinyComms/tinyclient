//! Posts one real notification and says whether the system lists it, then waits a while for a
//! click or a button and prints what came back.
//!
//!     cargo run -p notifier --example toast -- [seconds] [app id]
//!
//! On Windows the default id is the one debug builds of chatd register, so it never changes how
//! an installed tinyclient's notifications look.

use notifier::{Activation, App, Kind, Note};
use std::sync::mpsc;
use std::time::Duration;

fn main() {
    let mut args = std::env::args().skip(1);
    let wait: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let id = args.next().unwrap_or_else(|| "dev.tinyclient.app.debug".to_owned());
    let icon = std::env::current_dir().ok().map(|dir| dir.join("packaging/windows/tinyclient.ico"));
    let app = App { id, name: "tinyclient (dev)".to_owned(), icon };
    match app.register(true) {
        Ok(()) => println!("registered {}", app.id),
        Err(err) => println!("not registered: {err}"),
    }
    let (told, heard) = mpsc::channel::<Activation>();
    let told = std::sync::Mutex::new(told);
    let mut sink = notifier::system(&app, std::sync::Arc::new(move |done| {
        let _ = told.lock().map(|told| told.send(done));
    }));
    let note = Note {
        id: "test".to_owned(),
        title: "tinyclient".to_owned(),
        subtitle: String::new(),
        body: "This is how messages will look and sound.".to_owned(),
        kind: Kind::Message,
        reply_hint: "Reply".to_owned(),
        sound: None,
        locked_body: "New message".to_owned(),
    };
    match sink.post(&note) {
        Ok(()) => println!("posted"),
        Err(err) => println!("refused: {err}"),
    }
    println!("listed: {}", sink.listed("test"));
    println!("locked: {}", notifier::screen_locked());
    if let Ok(done) = heard.recv_timeout(Duration::from_secs(wait)) {
        println!("activated: {done:?}");
    }
    sink.withdraw("test");
    println!("listed after withdraw: {}", sink.listed("test"));
}
