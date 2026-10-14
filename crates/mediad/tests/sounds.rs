//! The bundled sounds in `packaging/sounds` are what `mediad::cue` renders: the WAVs it makes
//! now hash to the committed manifest, and the encoded files fit their budget.
//!
//! macOS only: the files are made there with afconvert, and another platform's maths library
//! may round a sample the other way.
#![cfg(target_os = "macos")]

use mediad::cue::{sound, wav, Sound, FILE_RATE};
use sha2::{Digest, Sha256};
use std::path::Path;

fn sounds() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../packaging/sounds"))
}

#[test]
fn the_committed_sounds_are_the_ones_the_code_renders() {
    let manifest = std::fs::read_to_string(sounds().join("SHA256SUMS")).expect("the manifest is committed");
    for which in Sound::ALL {
        let bytes = wav(&sound(which, FILE_RATE), FILE_RATE);
        let hash: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
        let line = format!("{hash}  {}.wav", which.file());
        assert!(manifest.lines().any(|l| l == line), "{which:?} renders to {hash}; run packaging/sounds/render.sh");
        assert!(sounds().join(format!("{}.caf", which.file())).is_file(), "{which:?} has no CAF");
    }
}

#[test]
fn the_seven_files_fit_in_160_kb() {
    let total: u64 = Sound::ALL.iter().map(|which| std::fs::metadata(sounds().join(format!("{}.caf", which.file()))).map_or(0, |m| m.len())).sum();
    assert!(total < 160 * 1024, "{total} bytes");
}
