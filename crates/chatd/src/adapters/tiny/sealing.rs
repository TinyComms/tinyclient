//! A file on its way into an end-to-end encrypted conversation. The server cannot look inside a
//! sealed file, so whatever it used to do to a picture happens here, before sealing: a picture is
//! decoded with limits on its size and re-encoded, which leaves its EXIF, XMP, IPTC and GPS
//! behind, and is turned the way its EXIF said first. Then the file is sealed with a fresh key
//! (`e2ee::attachment`, AES-256-GCM STREAM) into a private temporary file, and that is what goes up
//! as `sealed=1`. The key, the name, the type and the hash travel only inside the MLS message.
//!
//! Not processed: GIFs and every other kind of file, which go as they are (a GIF has no EXIF, and
//! can carry comments). A file the person marks to send as the original keeps its metadata.

use super::Saved;
use crate::blobs::upload::{Target, Upload};
use e2ee::attachment::{self, AttachmentKey};
use e2ee::AttachmentRef;
use serde_json::Value;
use std::io::{BufReader, Cursor, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Pictures are decoded only up to these, as `security.md` 3.3 says.
const MAX_SIDE: u32 = 16_384;
const MAX_PIXELS: u64 = 100_000_000;
const MAX_DECODE: u64 = 512 * 1024 * 1024;
/// The quality a JPEG is written again at.
const JPEG_QUALITY: u8 = 88;

/// A picture after it was cleaned: its bytes, and what it is now.
pub(crate) struct Cleaned {
    pub bytes: Vec<u8>,
    pub mime: &'static str,
    /// The extension its name ends in now.
    pub extension: &'static str,
    pub width: u32,
    pub height: u32,
}

/// Decodes a PNG, JPEG or WebP picture with limits, turns it upright, and writes it again without
/// any of its metadata: JPEG stays JPEG, the others become PNG (lossless, keeps transparency).
/// `None` for anything else, and for a picture that is too large or does not decode: it is then
/// sent as a file.
pub(crate) fn clean_picture(reader: impl Read + Seek, mime: &str) -> Option<Cleaned> {
    use image::{ImageDecoder, ImageFormat};
    let format = match mime {
        "image/jpeg" => ImageFormat::Jpeg,
        "image/png" => ImageFormat::Png,
        "image/webp" => ImageFormat::WebP,
        _ => return None,
    };
    let mut reader = image::ImageReader::with_format(BufReader::new(reader), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(MAX_DECODE);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().ok()?;
    let (width, height) = decoder.dimensions();
    if u64::from(width) * u64::from(height) > MAX_PIXELS {
        return None;
    }
    let orientation = decoder.orientation().ok();
    let mut picture = image::DynamicImage::from_decoder(decoder).ok()?;
    if let Some(orientation) = orientation {
        picture.apply_orientation(orientation);
    }
    let (width, height) = (picture.width(), picture.height());
    let mut out = Cursor::new(Vec::new());
    if format == ImageFormat::Jpeg {
        let rgb = picture.to_rgb8();
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY);
        rgb.write_with_encoder(encoder).ok()?;
        Some(Cleaned {
            bytes: out.into_inner(),
            mime: "image/jpeg",
            extension: "jpg",
            width,
            height,
        })
    } else {
        picture.write_to(&mut out, ImageFormat::Png).ok()?;
        Some(Cleaned {
            bytes: out.into_inner(),
            mime: "image/png",
            extension: "png",
            width,
            height,
        })
    }
}

/// `name` with its extension replaced.
fn renamed(name: &str, extension: &str) -> String {
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    let stem = if stem.is_empty() { "picture" } else { stem };
    format!("{stem}.{extension}")
}

/// Where a sealed copy waits for its upload: a private directory, swept of what a crash left.
fn outgoing() -> Result<PathBuf, String> {
    let dir = if cfg!(test) {
        std::env::temp_dir().join(format!("chatd-test-sealed-{}", std::process::id()))
    } else {
        protocol::support_dir().join("e2ee").join("outgoing")
    };
    super::e2ee::private_dir(&dir)
        .map_err(|err| format!("The file could not be prepared: {err}"))?;
    let _ = attachment::remove_stale_temps(&dir, std::time::Duration::from_secs(3600));
    Ok(dir)
}

/// One file sealed into a temporary file, with what its message entry says.
pub(crate) struct Sealed {
    pub file: tempfile::NamedTempFile,
    pub sealed_len: u64,
    pub entry: AttachmentRef,
}

/// Cleans (a picture) and seals one file for `conversation_id`.
pub(crate) fn seal(
    path: &Path,
    name: &str,
    mime: &str,
    original: bool,
    conversation_id: &str,
) -> Result<Sealed, String> {
    let unreadable = |err: std::io::Error| format!("{name} could not be read: {err}.");
    let context = super::e2ee::attachment_context(conversation_id).ok_or(super::CHECK)?;
    let key = super::e2ee::attachment_key()?;
    let dir = outgoing()?;
    let mut file = tempfile::Builder::new()
        .prefix(attachment::TEMP_PREFIX)
        .tempfile_in(&dir)
        .map_err(unreadable)?;
    let mut source = std::fs::File::open(path).map_err(unreadable)?;
    let cleaned = if original {
        None
    } else {
        clean_picture(&mut source, mime)
    };
    let (info, name, mime, width, height) = match cleaned {
        Some(cleaned) => {
            let info = attachment::seal(
                &key,
                &context,
                Cursor::new(&cleaned.bytes),
                file.as_file_mut(),
            )
            .map_err(|err| format!("{name} could not be sealed: {err}."))?;
            (
                info,
                renamed(name, cleaned.extension),
                cleaned.mime.to_owned(),
                cleaned.width,
                cleaned.height,
            )
        }
        None => {
            source.seek(SeekFrom::Start(0)).map_err(unreadable)?;
            let info = attachment::seal(&key, &context, BufReader::new(source), file.as_file_mut())
                .map_err(|err| format!("{name} could not be sealed: {err}."))?;
            (info, name.to_owned(), mime.to_owned(), 0, 0)
        }
    };
    file.as_file().sync_all().map_err(unreadable)?;
    Ok(Sealed {
        file,
        sealed_len: info.sealed_len,
        entry: AttachmentRef {
            id: String::new(),
            key: AttachmentKey::from_bytes(*key.as_bytes()),
            name,
            mime,
            bytes: info.plain_len,
            sha256: info.sha256,
            width,
            height,
        },
    })
}

/// Seals every file of a send and uploads each as `sealed=1`. Answers the message's entries, ids
/// filled in, in the order of the files.
pub(crate) fn upload_sealed(
    upload: &mut Upload,
    saved: &Saved,
    conversation_id: &str,
) -> Result<Vec<AttachmentRef>, String> {
    let mut entries = Vec::with_capacity(upload.files().len());
    for at in 0..upload.files().len() {
        upload.check()?;
        let source = &upload.files()[at];
        let (path, name, mime, original) = (
            source.path.clone(),
            source.name.clone(),
            source.mime,
            source.original || source.as_file,
        );
        let sealed = seal(&path, &name, mime, original, conversation_id)?;
        let reopened = sealed
            .file
            .reopen()
            .map_err(|err| format!("{name} could not be read: {err}."))?;
        upload.replace(
            at,
            reopened,
            sealed.sealed_len,
            "sealed",
            "application/octet-stream",
        );
        let target = Target::post(super::endpoint(&saved.base_url, "/v1/media"))
            .header("Authorization", &format!("Bearer {}", saved.token));
        let answer = match upload.send_form(at, &target, "file", &[("sealed", "1")]) {
            Ok(answer) => answer,
            Err(why) => return Err(super::messages::media_failure(&why)),
        };
        let body: Value = serde_json::from_str(&answer.body).unwrap_or(Value::Null);
        let id = body
            .get("id")
            .and_then(Value::as_str)
            .or_else(|| body.pointer("/media/id").and_then(Value::as_str))
            .unwrap_or("");
        if !super::safe_id(id) {
            return Err(super::UNREADABLE.to_owned());
        }
        let mut entry = sealed.entry;
        entry.id = id.to_owned();
        entries.push(entry);
        // The temporary file goes here; the upload holds it open no longer than this send.
        drop(sealed.file);
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A JPEG with an EXIF block that names a place.
    fn jpeg_with_exif() -> Vec<u8> {
        let picture = image::RgbImage::from_pixel(4, 3, image::Rgb([200, 10, 10]));
        let mut plain = Cursor::new(Vec::new());
        picture
            .write_to(&mut plain, image::ImageFormat::Jpeg)
            .unwrap();
        let plain = plain.into_inner();
        // APP1 "Exif\0\0" right after SOI, with a marker string a reader would see.
        let payload = b"Exif\0\0GPS 51.5007N 0.1246W secret-camera";
        let mut out = plain[..2].to_vec();
        out.extend_from_slice(&[0xFF, 0xE1]);
        out.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(payload);
        out.extend_from_slice(&plain[2..]);
        out
    }

    #[test]
    fn a_picture_loses_its_metadata_and_keeps_its_pixels() {
        let dirty = jpeg_with_exif();
        assert!(dirty.windows(6).any(|w| w == b"GPS 51"));
        let cleaned = clean_picture(Cursor::new(&dirty), "image/jpeg").expect("it decodes");
        assert!(
            !cleaned.bytes.windows(6).any(|w| w == b"GPS 51"),
            "the EXIF is gone"
        );
        assert!(!cleaned.bytes.windows(4).any(|w| w == b"Exif"));
        assert_eq!(
            (cleaned.width, cleaned.height, cleaned.mime),
            (4, 3, "image/jpeg")
        );
        assert!(image::load_from_memory(&cleaned.bytes).is_ok());
    }

    #[test]
    fn a_png_text_chunk_does_not_survive_and_webp_becomes_png() {
        let picture = image::RgbaImage::from_pixel(2, 2, image::Rgba([1, 2, 3, 4]));
        let mut png = Cursor::new(Vec::new());
        picture.write_to(&mut png, image::ImageFormat::Png).unwrap();
        let mut png = png.into_inner();
        // A tEXt chunk inserted after IHDR (8 byte signature + 25 byte IHDR).
        let text = b"tEXtComment\0where I live";
        let mut chunk = (text.len() as u32 - 4).to_be_bytes().to_vec();
        chunk.extend_from_slice(text);
        chunk.extend_from_slice(&[0, 0, 0, 0]);
        png.splice(33..33, chunk);
        // A wrong CRC makes the decoder refuse it, which is also the right outcome: sent as a file.
        if let Some(cleaned) = clean_picture(Cursor::new(&png), "image/png") {
            assert!(!cleaned.bytes.windows(10).any(|w| w == b"where I li"));
        }
        assert!(clean_picture(Cursor::new(b"not a picture"), "image/png").is_none());
        assert!(
            clean_picture(Cursor::new(b"GIF89a"), "image/gif").is_none(),
            "a GIF goes as it is"
        );
        assert_eq!(renamed("photo.webp", "png"), "photo.png");
        assert_eq!(renamed("noext", "png"), "noext.png");
    }

    #[test]
    fn a_sealed_file_opens_with_its_entry_only_in_its_conversation() {
        let dir = std::env::temp_dir().join(format!("chatd-seal-src-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("photo.jpg");
        std::fs::write(&path, jpeg_with_exif()).unwrap();
        let conv = "6f0c9c5e-3b1f-4c7a-9d52-1a2b3c4d5e6f";
        let sealed = seal(&path, "photo.jpg", "image/jpeg", false, conv).unwrap();
        let blob = std::fs::read(sealed.file.path()).unwrap();
        assert!(
            !blob.windows(6).any(|w| w == b"GPS 51"),
            "nothing readable goes up"
        );
        assert_eq!(blob.len() as u64, sealed.sealed_len);
        let context = super::super::e2ee::attachment_context(conv).unwrap();
        let mut opened = Vec::new();
        attachment::open_verified(
            &sealed.entry.key,
            &context,
            Cursor::new(&blob),
            &mut opened,
            &sealed.entry.sha256,
            Some(sealed.entry.bytes),
        )
        .unwrap();
        assert!(image::load_from_memory(&opened).is_ok());
        let elsewhere =
            super::super::e2ee::attachment_context("00000000-0000-4000-8000-000000000001").unwrap();
        assert!(attachment::open_verified(
            &sealed.entry.key,
            &elsewhere,
            Cursor::new(&blob),
            std::io::sink(),
            &sealed.entry.sha256,
            None
        )
        .is_err());
        let original = seal(&path, "photo.jpg", "image/jpeg", true, conv).unwrap();
        assert_eq!(
            original.entry.bytes,
            std::fs::metadata(&path).unwrap().len(),
            "the original goes as it is"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
