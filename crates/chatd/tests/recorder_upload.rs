//! The real recorder's output must pass the same upload checks as a Telegram send.
#[path = "support/mediad.rs"]
mod test_mediad;

use chatd::{blobs::upload::{self, Upload}, hub::Hub, store::Store};
use protocol::{Key, Network, OutgoingFile, PlayRequest, PlayState, RecordRequest, RecordState};
use std::{path::PathBuf, process::Command, sync::Arc, thread::sleep, time::{Duration, Instant}};

#[test]
fn a_recorded_opus_note_can_be_uploaded_and_private_cache_files_stay_blocked() {
    // Isolate support_dir and the simulated microphone from every other test's environment.
    if std::env::var_os("TINY_RECORDER_UPLOAD_CHILD").is_none() {
        let dir = std::env::temp_dir().join(format!("tiny-recorder-upload-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "a_recorded_opus_note_can_be_uploaded_and_private_cache_files_stay_blocked", "--nocapture"])
            .env("TINY_RECORDER_UPLOAD_CHILD", "1")
            .env("TINYCLIENT_DIR", &dir)
            .env("TINYCLIENT_MEDIAD_AUDIO", "tone")
            .env("TINYCLIENT_MEDIAD", test_mediad::path())
            .status().unwrap();
        std::fs::remove_dir_all(dir).unwrap();
        assert!(status.success(), "recording/upload child failed");
        return;
    }

    let dir = protocol::support_dir();
    let hub = Arc::new(Hub::new(Store::open(&dir.join("chat.sqlite")).unwrap()));
    let key = Key { network: Network::Telegram, account: "1".into(), id: "7002".into() };
    hub.recorder.request(&hub, RecordRequest::Start { conversation: key.clone() });
    let shown = hub.recorder.shown().unwrap();
    assert!(matches!(shown.state, RecordState::Active), "{shown:?}");
    sleep(Duration::from_millis(500));
    hub.recorder.request(&hub, RecordRequest::Stop);
    let deadline = Instant::now() + Duration::from_secs(10);
    let path = loop {
        match hub.recorder.shown().unwrap().state {
            RecordState::Recorded { path, duration, waveform } => {
                assert!(duration > 0);
                assert!(!waveform.is_empty());
                break PathBuf::from(path);
            }
            RecordState::Failed { detail } => panic!("recording failed: {detail}"),
            _ => assert!(Instant::now() < deadline, "recording did not stop"),
        }
        sleep(Duration::from_millis(10));
    };
    let bytes = std::fs::read(&path).unwrap();
    assert!(bytes.starts_with(b"OggS"), "recording is an Ogg container");
    assert!(bytes.windows(8).any(|chunk| chunk == b"OpusHead"));
    let media = dir.join("media");
    let play = |path: &std::path::Path, conversation: Key| PlayRequest::Start {
        account: conversation.account(), file: path.to_string_lossy().into_owned(),
        path: path.to_string_lossy().into_owned(), conversation, message: String::new(),
        title: "Voice message preview".into(), at: None, rate: 100, output: None,
    };
    // An outbox file is playable only while it is the current finished recording, and
    // only for that recording's account and conversation.
    let other = upload::outbox().join("other.opus");
    std::fs::write(&other, &bytes).unwrap();
    assert!(hub.player.request(&hub, &media, play(&other, key.clone())).is_err());
    assert!(hub.player.request(&hub, &media, play(&path, Key { id: "other".into(), ..key.clone() })).is_err());
    assert!(hub.player.request(&hub, &media, play(&path, Key { account: "other".into(), ..key.clone() })).is_err());
    hub.player.request(&hub, &media, play(&path, key.clone())).unwrap();
    wait_playing(&hub);
    hub.player.request(&hub, &media, PlayRequest::Pause).unwrap();
    assert_eq!(hub.player.shown().unwrap().state, PlayState::Paused);
    hub.player.request(&hub, &media, play(&path, key.clone())).unwrap();
    wait_playing(&hub);
    let deadline = Instant::now() + Duration::from_secs(10);
    while hub.player.shown().unwrap().state != PlayState::Ended {
        assert!(Instant::now() < deadline, "preview never ended: {:?}", hub.player.shown());
        sleep(Duration::from_millis(5));
    }
    hub.player.request(&hub, &media, play(&path, key.clone())).unwrap();
    wait_playing(&hub);
    hub.recorder.request(&hub, RecordRequest::Forget);
    assert!(hub.player.shown().is_none(), "removing the note stops its preview");
    assert!(hub.player.request(&hub, &media, play(&path, key.clone())).is_err());
    let file = OutgoingFile {
        path: path.to_string_lossy().into_owned(),
        name: path.file_name().unwrap().to_string_lossy().into_owned(),
        spoiler: false, as_file: false, original: false,
    };
    let up = Upload::with_progress(&key.account(), "voice-upload", &[file.clone()], |_, _| {}).unwrap();
    assert_eq!(up.files()[0].mime, "audio/ogg");
    assert_eq!(up.files()[0].bytes, bytes.len() as u64);
    drop(up);
    upload::discard(&[file]);
    assert!(!path.exists(), "sent outbox note is cleaned up");

    let private = dir.join("media").join("private.opus");
    std::fs::create_dir_all(private.parent().unwrap()).unwrap();
    std::fs::write(&private, bytes).unwrap();
    assert!(upload::allowed(&private.to_string_lossy()).is_err());
}

fn wait_playing(hub: &Hub) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let shown = hub.player.shown().unwrap();
        if shown.state == PlayState::Playing { return; }
        assert!(matches!(shown.state, PlayState::Loading), "preview failed: {shown:?}");
        assert!(Instant::now() < deadline, "preview did not start");
        sleep(Duration::from_millis(5));
    }
}
