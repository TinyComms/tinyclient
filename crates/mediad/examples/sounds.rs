//! Renders the seven bundled notification sounds to 16-bit WAV, 22.05 kHz mono, from the code in
//! `mediad::cue`. The committed CAF files in `packaging/sounds` are these, encoded:
//!
//!     cargo run -p mediad --example sounds -- /tmp/sounds
//!     for f in /tmp/sounds/*.wav; do
//!         afconvert -f caff -d ima4 -c 1 "$f" "packaging/sounds/$(basename "$f" .wav).caf"
//!     done
//!
//! `packaging/sounds/SHA256SUMS` holds the WAVs' hashes; a test renders them again and compares,
//! so the files cannot drift from the code.

use mediad::cue::{sound, wav, Sound, FILE_RATE};

fn main() {
    let dir = std::env::args().nth(1).unwrap_or_else(|| "sounds".to_owned());
    std::fs::create_dir_all(&dir).expect("the directory is made");
    for which in Sound::ALL {
        let path = std::path::Path::new(&dir).join(format!("{}.wav", which.file()));
        std::fs::write(&path, wav(&sound(which, FILE_RATE), FILE_RATE)).expect("the file is written");
        println!("{}", path.display());
    }
}
