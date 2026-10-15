//! The demo's photographs, portraits and logos, so a screenshot of it looks like a chat people
//! use. They are files in `photos/` beside this module, credited in `photos/CREDITS.md`, and are
//! read from the source tree when the demo is seeded: none of them is compiled into `chatd`. A
//! `chatd` run away from its source tree finds none and the demo draws its own pictures instead.

use std::path::Path;

const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/demo/photos");

/// The bytes of `name`, a plain file name in `photos/`.
pub fn bytes(name: &str) -> Option<Vec<u8>> {
    let plain = !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.') && !name.starts_with('.');
    plain.then(|| std::fs::read(Path::new(DIR).join(name)).ok()).flatten()
}

/// The content type a file in `photos/` is served with.
pub fn mime(name: &str) -> &'static str {
    match name.rsplit('.').next() {
        Some("jpg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        Some("opus") => "audio/ogg",
        Some("m4a") => "audio/mp4",
        Some("mp4") => "video/mp4",
        _ => "image/png",
    }
}

/// The portrait of the person called `name`, when there is one: "Chloé Martin" is
/// `avatar-chloe-martin.jpg` (or `.png`, or a moving `.webp`, as a Discord `a_` avatar is).
pub fn portrait(name: &str) -> Option<String> {
    let mut slug = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        let c = match c {
            'é' | 'è' | 'ê' => 'e',
            'á' | 'à' | 'ä' => 'a',
            'ö' | 'ó' => 'o',
            c => c,
        };
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_end_matches('-');
    ["webp", "jpg", "png"].into_iter().map(|ext| format!("avatar-{slug}.{ext}")).find(|file| Path::new(DIR).join(file).is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_finds_its_portrait_and_nothing_leaves_the_directory() {
        assert_eq!(portrait("Chloé Martin").as_deref(), Some("avatar-chloe-martin.jpg"));
        assert_eq!(portrait("Haruto Senda").as_deref(), Some("avatar-haruto-senda.png"));
        assert_eq!(portrait("Nobody In Particular"), None);
        assert!(bytes("avatar-ines-marchetti.jpg").is_some());
        assert_eq!(bytes("../photos.rs"), None);
        assert_eq!(bytes(".."), None);
    }
}
