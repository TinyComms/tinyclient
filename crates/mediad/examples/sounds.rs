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
//!
//! `sounds strip <file.caf>...` then drops what afconvert adds that nothing plays: the 'free'
//! padding (3.9 KB a file) and the 'info' strings.

use mediad::cue::{sound, wav, Sound, FILE_RATE};

/// A CAF file without its 'free' and 'info' chunks. The header is 8 bytes; each chunk is a
/// four-letter type, a big-endian 64-bit size, then its bytes. A size of -1 (only 'data', last)
/// runs to the end of the file.
fn strip(bytes: &[u8]) -> Vec<u8> {
    let mut out = bytes[..8].to_vec();
    let mut at = 8;
    while at + 12 <= bytes.len() {
        let kind = &bytes[at..at + 4];
        let size = i64::from_be_bytes(bytes[at + 4..at + 12].try_into().expect("8 bytes"));
        let end = if size < 0 { bytes.len() } else { (at + 12 + size as usize).min(bytes.len()) };
        if kind != b"free" && kind != b"info" {
            out.extend_from_slice(&bytes[at..end]);
        }
        at = end;
    }
    out
}

fn main() {
    let mut args = std::env::args().skip(1);
    if let Some("strip") = args.next().as_deref() {
        for path in args {
            let bytes = std::fs::read(&path).expect("the file is read");
            assert_eq!(&bytes[..4], b"caff", "{path} is not a CAF file");
            std::fs::write(&path, strip(&bytes)).expect("the file is written");
        }
        return;
    }
    let dir = std::env::args().nth(1).unwrap_or_else(|| "sounds".to_owned());
    std::fs::create_dir_all(&dir).expect("the directory is made");
    for which in Sound::ALL {
        let path = std::path::Path::new(&dir).join(format!("{}.wav", which.file()));
        std::fs::write(&path, wav(&sound(which, FILE_RATE), FILE_RATE)).expect("the file is written");
        println!("{}", path.display());
    }
}
