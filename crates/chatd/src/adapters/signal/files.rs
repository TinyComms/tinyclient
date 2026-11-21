//! A file on its way to Signal. A still picture is decoded with limits and written again, so the
//! copy that leaves has no EXIF, XMP or location; a GIF keeps every frame; a video goes as it is.
//! The phone's own copy of a send we already showed with its file must not replace that row.

use super::wire::{File, Quote};
use image::DynamicImage;
use protocol::{Capabilities, MediaKind, Outgoing};
use std::io::Cursor;

/// Largest file Signal takes from here, in bytes.
pub const MAX_FILE_BYTES: u64 = 100 * 1024 * 1024;
/// Most files in one message.
pub const MAX_FILES: u32 = 32;
/// Pictures are decoded only up to these.
const MAX_SIDE: u32 = 16_384;
const MAX_PIXELS: u64 = 100_000_000;
const MAX_DECODE: u64 = 512 * 1024 * 1024;
/// The quality a JPEG is written again at.
const JPEG_QUALITY: u8 = 88;
/// BlurHash components on a real picture: wide, then tall.
const HASH_X: u32 = 4;
const HASH_Y: u32 = 3;

/// What the composer may offer once Signal is connected. A caption uses the text limit.
pub fn capabilities() -> Capabilities {
    Capabilities {
        max_file_bytes: MAX_FILE_BYTES,
        max_files: MAX_FILES,
        ..Capabilities::default()
    }
}

/// Options Signal does not carry out. `None` when the send can go.
pub fn refused(with: &Outgoing) -> Option<String> {
    if with.schedule.is_some() {
        return Some("Signal can't send a message later.".into());
    }
    if with.voice.is_some() {
        return Some("A voice note can't be sent to Signal from here.".into());
    }
    if let Some(pick) = &with.pick {
        if pick.kind != protocol::CatalogKind::Gifs {
            return Some("Signal can't send a chosen sticker from here.".into());
        }
    }
    if with.files.iter().any(|file| file.spoiler) {
        return Some("Signal can't hide a file behind a spoiler.".into());
    }
    if with.files.len() > MAX_FILES as usize {
        return Some(format!(
            "Signal takes at most {MAX_FILES} files in one message."
        ));
    }
    None
}

/// `Some` when `bytes` is over Signal's limit, naming the file and the limit.
pub fn too_big(name: &str, bytes: u64) -> Option<String> {
    (bytes > MAX_FILE_BYTES).then(|| {
        format!(
            "{name} is too big for Signal (limit {} MB).",
            MAX_FILE_BYTES >> 20
        )
    })
}

/// The phone already showed this send with a file, so its text-only copy must not replace it.
pub fn replace_outgoing_sync(had_attachments: bool) -> bool {
    had_attachments
}

/// What the scripted wire records for a file send. The caption is omitted when empty.
pub fn call_line(
    conversation: &str,
    names: &[String],
    caption: &str,
    quote: Option<&Quote>,
) -> String {
    let mut line = format!("send {conversation} files {}", names.join(","));
    if !caption.is_empty() {
        line.push(' ');
        line.push_str(caption);
    }
    if let Some(quote) = quote {
        line.push_str(&format!(" (answering {}-{})", quote.sent, quote.author));
    }
    line
}

/// What a conversation draws this file as.
pub fn kind(mime: &str) -> MediaKind {
    attachment_kind(mime, 0)
}

/// Maps a Signal attachment's MIME type and `AttachmentPointer.Flags` bitmask to `MediaKind`.
pub fn attachment_kind(mime: &str, flags: u32) -> MediaKind {
    if flags & super::wire::FLAG_GIF != 0 || mime.eq_ignore_ascii_case("image/gif") {
        return MediaKind::Gif;
    }
    if flags & super::wire::FLAG_VOICE_MESSAGE != 0 || mime.starts_with("audio/") {
        return MediaKind::Audio;
    }
    if mime.starts_with("image/") {
        return MediaKind::Image;
    }
    if mime.starts_with("video/") {
        return MediaKind::Video;
    }
    MediaKind::File
}

/// Fallback display filename when a Signal attachment pointer omits `file_name`.
pub fn default_filename(mime: &str, flags: u32) -> &'static str {
    if flags & super::wire::FLAG_VOICE_MESSAGE != 0 {
        return "voice-message.m4a";
    }
    if flags & super::wire::FLAG_GIF != 0 || mime.eq_ignore_ascii_case("image/gif") {
        return "animation.gif";
    }
    match mime.to_ascii_lowercase().as_str() {
        "image/jpeg" | "image/jpg" => "photo.jpg",
        "image/png" => "photo.png",
        "image/webp" => "photo.webp",
        "video/mp4" => "video.mp4",
        "video/quicktime" => "video.mov",
        "video/webm" => "video.webm",
        "audio/mp4" | "audio/aac" | "audio/m4a" => "audio.m4a",
        "audio/ogg" => "audio.ogg",
        "audio/mpeg" => "audio.mp3",
        "application/pdf" => "document.pdf",
        _ if mime.starts_with("image/") => "photo.jpg",
        _ if mime.starts_with("video/") => "video.mp4",
        _ if mime.starts_with("audio/") => "audio.m4a",
        _ => "file",
    }
}

/// Human-readable short label for a quoted attachment when the quoted message had no text.
pub fn attachment_label(mime: &str, file_name: &str, flags: u32) -> String {
    if !file_name.trim().is_empty() && matches!(attachment_kind(mime, flags), MediaKind::File) {
        return file_name.trim().to_owned();
    }
    if flags & super::wire::FLAG_VOICE_MESSAGE != 0 {
        return "Voice message".to_owned();
    }
    match attachment_kind(mime, flags) {
        MediaKind::Image => "Photo".to_owned(),
        MediaKind::Video => "Video".to_owned(),
        MediaKind::Gif => "GIF".to_owned(),
        MediaKind::Audio => "Audio".to_owned(),
        _ => {
            if !file_name.trim().is_empty() {
                file_name.trim().to_owned()
            } else {
                "File".to_owned()
            }
        }
    }
}

/// The bytes Signal is given. `keep` sends a picture as it is (as a file, or the original).
pub fn prepare(name: &str, mime: &str, bytes: Vec<u8>, keep: bool) -> File {
    let picture = opened(&bytes, mime);
    let cleaned = if keep {
        None
    } else {
        picture.as_ref().and_then(|picture| encode(picture, mime))
    };
    let (payload, sent_mime, shown) = match cleaned {
        Some((payload, sent_mime)) => (payload, sent_mime.to_owned(), renamed(name, sent_mime)),
        None => (bytes, mime.to_owned(), name.to_owned()),
    };
    let (width, height, thumb, blurhash) = match picture.as_ref() {
        Some(picture) if picture.width() > 0 && picture.height() > 0 => {
            let thumb = jpeg(&scaled(picture, 200));
            (
                picture.width(),
                picture.height(),
                thumb,
                hash_picture(picture),
            )
        }
        _ => (0, 0, Vec::new(), None),
    };
    let gif = sent_mime == "image/gif";
    File {
        name: shown,
        mime: sent_mime,
        bytes: payload,
        width,
        height,
        thumb,
        blurhash,
        gif,
    }
}

/// BlurHash of `pixels` in row order. A solid red pixel at one component is `00TI:j`.
pub fn blurhash(pixels: &[(u8, u8, u8)], width: u32, height: u32, nx: u32, ny: u32) -> String {
    let (nx, ny) = (nx.clamp(1, 9), ny.clamp(1, 9));
    if width == 0 || height == 0 || pixels.len() < width as usize * height as usize {
        return String::new();
    }
    let mut factors = Vec::with_capacity((nx * ny) as usize);
    for cy in 0..ny {
        for cx in 0..nx {
            factors.push(factor(pixels, width, height, cx, cy));
        }
    }
    let mut maximum = 0.0f32;
    for &(r, g, b) in factors.iter().skip(1) {
        maximum = maximum.max(r.abs()).max(g.abs()).max(b.abs());
    }
    let quantised = (maximum * 166.0 - 0.5).clamp(0.0, 82.0) as u32;
    let scale = (quantised as f32 + 1.0) / 166.0;
    let mut out = String::new();
    push83(&mut out, (nx - 1) + (ny - 1) * 9, 1);
    push83(&mut out, quantised, 1);
    push83(&mut out, encode_dc(factors[0]), 4);
    for &factor in factors.iter().skip(1) {
        push83(&mut out, encode_ac(factor, scale), 2);
    }
    out
}

fn hash_picture(picture: &DynamicImage) -> Option<String> {
    let small = scaled(picture, 32);
    let pixels: Vec<(u8, u8, u8)> = small
        .to_rgb8()
        .pixels()
        .map(|pixel| (pixel[0], pixel[1], pixel[2]))
        .collect();
    let hash = blurhash(&pixels, small.width(), small.height(), HASH_X, HASH_Y);
    (!hash.is_empty()).then_some(hash)
}

fn opened(bytes: &[u8], mime: &str) -> Option<DynamicImage> {
    use image::{ImageDecoder, ImageFormat};
    let format = match mime {
        "image/jpeg" => ImageFormat::Jpeg,
        "image/png" => ImageFormat::Png,
        "image/webp" => ImageFormat::WebP,
        "image/gif" => ImageFormat::Gif,
        _ => return None,
    };
    let mut reader =
        image::ImageReader::with_format(std::io::BufReader::new(Cursor::new(bytes)), format);
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
    // A GIF's frames stay in the file. Its first frame is only for the preview.
    let orientation = match format {
        ImageFormat::Gif => None,
        _ => decoder.orientation().ok(),
    };
    let mut picture = DynamicImage::from_decoder(decoder).ok()?;
    if let Some(orientation) = orientation {
        picture.apply_orientation(orientation);
    }
    Some(picture)
}

/// JPEG stays JPEG. PNG and WebP become PNG, which keeps transparency and drops metadata.
fn encode(picture: &DynamicImage, mime: &str) -> Option<(Vec<u8>, &'static str)> {
    let mut out = Cursor::new(Vec::new());
    match mime {
        "image/jpeg" => {
            let rgb = picture.to_rgb8();
            let encoder =
                image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY);
            rgb.write_with_encoder(encoder).ok()?;
            Some((out.into_inner(), "image/jpeg"))
        }
        "image/png" | "image/webp" => {
            picture.write_to(&mut out, image::ImageFormat::Png).ok()?;
            Some((out.into_inner(), "image/png"))
        }
        _ => None,
    }
}

fn jpeg(picture: &DynamicImage) -> Vec<u8> {
    let rgb = picture.to_rgb8();
    let mut out = Cursor::new(Vec::new());
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 60);
    if rgb.write_with_encoder(encoder).is_err() {
        return Vec::new();
    }
    out.into_inner()
}

fn scaled(picture: &DynamicImage, longest: u32) -> DynamicImage {
    let (width, height) = (picture.width().max(1), picture.height().max(1));
    let long = width.max(height);
    if long <= longest {
        return picture.clone();
    }
    let width = (u64::from(width) * u64::from(longest) / u64::from(long)).max(1) as u32;
    let height = (u64::from(height) * u64::from(longest) / u64::from(long)).max(1) as u32;
    picture.resize(width, height, image::imageops::FilterType::Triangle)
}

fn renamed(name: &str, mime: &str) -> String {
    let extension = match mime {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        _ => return name.to_owned(),
    };
    let Some((stem, current)) = name.rsplit_once('.') else {
        return format!("{name}.{extension}");
    };
    if current.eq_ignore_ascii_case(extension)
        || (extension == "jpg" && current.eq_ignore_ascii_case("jpeg"))
    {
        return name.to_owned();
    }
    let stem = if stem.is_empty() { "picture" } else { stem };
    format!("{stem}.{extension}")
}

fn factor(pixels: &[(u8, u8, u8)], width: u32, height: u32, cx: u32, cy: u32) -> (f32, f32, f32) {
    let mut r = 0.0f32;
    let mut g = 0.0f32;
    let mut b = 0.0f32;
    let norm = if cx == 0 && cy == 0 { 1.0 } else { 2.0 };
    let (w, h) = (width as f32, height as f32);
    for y in 0..height {
        for x in 0..width {
            let basis = (std::f32::consts::PI * x as f32 * cx as f32 / w).cos()
                * (std::f32::consts::PI * y as f32 * cy as f32 / h).cos();
            let (pr, pg, pb) = pixels[(y * width + x) as usize];
            r += basis * linear(pr);
            g += basis * linear(pg);
            b += basis * linear(pb);
        }
    }
    let scale = norm / (w * h);
    (r * scale, g * scale, b * scale)
}

fn linear(value: u8) -> f32 {
    let v = f32::from(value) / 255.0;
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

fn encode_dc((r, g, b): (f32, f32, f32)) -> u32 {
    (srgb(r) << 16) + (srgb(g) << 8) + srgb(b)
}

fn srgb(value: f32) -> u32 {
    let v = value.clamp(0.0, 1.0);
    let s = if v <= 0.0031308 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    };
    (s * 255.0 + 0.5) as u32
}

fn encode_ac((r, g, b): (f32, f32, f32), maximum: f32) -> u32 {
    let quantise = |value: f32| {
        let scaled = (value / maximum).clamp(-1.0, 1.0);
        (scaled.signum() * scaled.abs().powf(0.5) * 9.0 + 9.5)
            .floor()
            .clamp(0.0, 18.0) as u32
    };
    quantise(r) * 19 * 19 + quantise(g) * 19 + quantise(b)
}

const DIGITS: &[u8] =
    b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz#$%*+,-.:;=?@[]^_{|}~";

fn push83(out: &mut String, mut value: u32, length: usize) {
    let mut chars = vec![b'0'; length];
    for slot in chars.iter_mut().rev() {
        *slot = DIGITS[(value as usize) % 83];
        value /= 83;
    }
    out.extend(chars.iter().map(|byte| char::from(*byte)));
}

fn decode83(text: &str) -> Option<u32> {
    let mut value = 0u32;
    for byte in text.bytes() {
        let digit = DIGITS.iter().position(|&d| d == byte)? as u32;
        value = value.checked_mul(83)?.checked_add(digit)?;
    }
    Some(value)
}

fn decode_dc(value: u32) -> (f32, f32, f32) {
    (
        linear(((value >> 16) & 0xff) as u8),
        linear(((value >> 8) & 0xff) as u8),
        linear((value & 0xff) as u8),
    )
}

fn decode_ac(value: u32, maximum: f32) -> (f32, f32, f32) {
    let quant_r = (value / (19 * 19)) as i32;
    let quant_g = ((value / 19) % 19) as i32;
    let quant_b = (value % 19) as i32;
    let unquant = |q: i32| -> f32 {
        let s = (q as f32 - 9.0) / 9.0;
        s.signum() * s * s * maximum
    };
    (unquant(quant_r), unquant(quant_g), unquant(quant_b))
}

/// Decodes a BlurHash string into a tiny JPEG preview image (at most 32 px on the longest side)
/// for immediate display before the full encrypted attachment finishes downloading.
pub fn decode_blurhash_jpeg(hash: &str, width: u32, height: u32) -> Option<Vec<u8>> {
    if hash.len() < 6 || !hash.is_ascii() {
        return None;
    }
    let size_flag = decode83(&hash[0..1])?;
    let ny = (size_flag / 9) + 1;
    let nx = (size_flag % 9) + 1;
    let expected_len = 4 + 2 * (nx * ny) as usize;
    if hash.len() != expected_len {
        return None;
    }
    let quant_max = decode83(&hash[1..2])?;
    let max_val = (quant_max as f32 + 1.0) / 166.0;
    let mut colors = Vec::with_capacity((nx * ny) as usize);
    colors.push(decode_dc(decode83(&hash[2..6])?));
    for i in 1..(nx * ny) as usize {
        let off = 4 + i * 2;
        colors.push(decode_ac(decode83(&hash[off..off + 2])?, max_val));
    }
    let (pw, ph) = if width > 0 && height > 0 {
        let long = width.max(height);
        let pw = ((u64::from(width) * 32) / u64::from(long)).clamp(4, 32) as u32;
        let ph = ((u64::from(height) * 32) / u64::from(long)).clamp(4, 32) as u32;
        (pw, ph)
    } else {
        (32, 24)
    };
    let mut img = image::RgbImage::new(pw, ph);
    for y in 0..ph {
        for x in 0..pw {
            let mut r = 0.0f32;
            let mut g = 0.0f32;
            let mut b = 0.0f32;
            for j in 0..ny {
                let cos_y = (std::f32::consts::PI * y as f32 * j as f32 / ph as f32).cos();
                for i in 0..nx {
                    let basis = (std::f32::consts::PI * x as f32 * i as f32 / pw as f32).cos() * cos_y;
                    let (cr, cg, cb) = colors[(j * nx + i) as usize];
                    r += cr * basis;
                    g += cg * basis;
                    b += cb * basis;
                }
            }
            img.put_pixel(x, y, image::Rgb([srgb(r) as u8, srgb(g) as u8, srgb(b) as u8]));
        }
    }
    let bytes = jpeg(&DynamicImage::ImageRgb8(img));
    (!bytes.is_empty()).then_some(bytes)
}

/// Encrypts `plaintext` with Signal's attachment cipher:
/// `IV (16) || AES-256-CBC-PKCS7(key[..32], IV, plaintext) || HMAC-SHA256(key[32..], IV || ct)`.
/// Returns `(ciphertext, sha256_digest_of_ciphertext)`.
pub fn encrypt_attachment(iv: [u8; 16], key: [u8; 64], plaintext: &[u8]) -> (Vec<u8>, [u8; 32]) {
    use aes::cipher::{BlockEncrypt, KeyInit};
    use aes::Aes256;
    use hmac::{Hmac, Mac};
    use sha2::{Digest, Sha256};

    let aes = Aes256::new_from_slice(&key[..32]).expect("32-byte AES key");
    let pad = 16 - (plaintext.len() % 16);
    let mut out = Vec::with_capacity(16 + plaintext.len() + pad + 32);
    out.extend_from_slice(&iv);
    let mut prev = iv;
    let mut chunks = plaintext.chunks_exact(16);
    for chunk in &mut chunks {
        let mut block = [0u8; 16];
        for i in 0..16 {
            block[i] = chunk[i] ^ prev[i];
        }
        aes.encrypt_block((&mut block).into());
        out.extend_from_slice(&block);
        prev = block;
    }
    let rem = chunks.remainder();
    let mut last = [pad as u8; 16];
    last[..rem.len()].copy_from_slice(rem);
    for i in 0..16 {
        last[i] ^= prev[i];
    }
    aes.encrypt_block((&mut last).into());
    out.extend_from_slice(&last);

    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&key[32..64]).expect("32-byte HMAC key");
    mac.update(&out);
    out.extend_from_slice(&mac.finalize().into_bytes());
    let digest: [u8; 32] = Sha256::digest(&out).into();
    (out, digest)
}

/// Streams an encrypted Signal attachment from `reader` onto disk at `dest` without holding the
/// whole file in memory:
/// 1. Streams ciphertext to a temporary `.enc` file beside `dest`, computing SHA-256 and
///    HMAC-SHA256 on the fly (keeping a 32-byte trailing MAC window).
/// 2. Verifies SHA-256 digest (when non-empty) and HMAC-SHA256.
/// 3. Streams AES-256-CBC decryption in 64 KiB chunks from `.enc` to `dest`, removes PKCS#7
///    padding, truncates to `plaintext_size` (when > 0), and deletes `.enc`.
pub fn decrypt_attachment_stream<R: std::io::Read>(
    mut reader: R,
    key: &[u8],
    expected_digest: &[u8],
    plaintext_size: u32,
    dest: &std::path::Path,
) -> Result<u64, String> {
    use hmac::{Hmac, Mac};
    use sha2::{Digest, Sha256};
    use std::io::Write;

    const STREAM_CAP: u64 = MAX_FILE_BYTES + 1024 * 1024;
    if key.len() != 64 {
        return Err("Invalid attachment key.".to_owned());
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    let enc_path = dest.with_extension("sigenc");
    let cleanup = || {
        let _ = std::fs::remove_file(&enc_path);
    };

    // Pass 1: stream encrypted bytes to disk while computing SHA-256 and HMAC-SHA256.
    let mut sha = Sha256::new();
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&key[32..64]).map_err(|_| "Invalid HMAC key.".to_owned())?;
    let mut total_enc = 0u64;
    let mut tail = Vec::<u8>::with_capacity(64);
    {
        let mut enc_file = protocol::private::create(&enc_path).map_err(|err| err.to_string())?;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(err) => {
                    drop(enc_file);
                    cleanup();
                    return Err(format!("Attachment download failed: {err}"));
                }
            };
            total_enc = total_enc.saturating_add(n as u64);
            if total_enc > STREAM_CAP {
                drop(enc_file);
                cleanup();
                return Err("Attachment exceeds Signal's size limit.".to_owned());
            }
            let slice = &buf[..n];
            sha.update(slice);
            if let Err(err) = enc_file.write_all(slice) {
                drop(enc_file);
                cleanup();
                return Err(err.to_string());
            }
            // Feed everything except the final 32 bytes (the trailing MAC) into HMAC.
            if slice.len() >= 32 {
                if !tail.is_empty() {
                    mac.update(&tail);
                    tail.clear();
                }
                let cut = slice.len() - 32;
                if cut > 0 {
                    mac.update(&slice[..cut]);
                }
                tail.extend_from_slice(&slice[cut..]);
            } else {
                tail.extend_from_slice(slice);
                if tail.len() > 32 {
                    let flush = tail.len() - 32;
                    mac.update(&tail[..flush]);
                    tail.drain(..flush);
                }
            }
        }
    }

    if total_enc < 16 + 16 + 32 || tail.len() != 32 {
        cleanup();
        return Err("Encrypted attachment is truncated.".to_owned());
    }
    let body_len = total_enc - 16 - 32;
    if !body_len.is_multiple_of(16) {
        cleanup();
        return Err("Encrypted attachment is not block-aligned.".to_owned());
    }
    if !expected_digest.is_empty() {
        let actual_digest = sha.finalize();
        if actual_digest.as_slice() != expected_digest {
            cleanup();
            return Err("Attachment digest mismatch.".to_owned());
        }
    }
    if mac.verify_slice(&tail).is_err() {
        cleanup();
        return Err("Attachment MAC verification failed.".to_owned());
    }

    // Pass 2: stream AES-256-CBC decryption in 64 KiB chunks to `dest`.
    let res = decrypt_verified_enc_file(&enc_path, body_len, key, plaintext_size, dest);
    cleanup();
    res
}

/// Verifies SHA-256 digest and HMAC-SHA256 of an already-downloaded encrypted blob at `enc_path`,
/// then streams AES-256-CBC decryption in 64 KiB chunks into `dest`. Removes `enc_path` before returning.
pub fn decrypt_attachment_file(
    enc_path: &std::path::Path,
    key: &[u8],
    expected_digest: &[u8],
    plaintext_size: u32,
    dest: &std::path::Path,
) -> Result<u64, String> {
    use hmac::{Hmac, Mac};
    use sha2::{Digest, Sha256};
    use std::io::Read;

    let cleanup = || {
        let _ = std::fs::remove_file(enc_path);
    };
    if key.len() != 64 {
        cleanup();
        return Err("Invalid attachment key.".to_owned());
    }
    let meta = match std::fs::metadata(enc_path) {
        Ok(m) => m,
        Err(err) => {
            cleanup();
            return Err(err.to_string());
        }
    };
    let total_enc = meta.len();
    if total_enc < 16 + 16 + 32 {
        cleanup();
        return Err("Encrypted attachment is truncated.".to_owned());
    }
    let body_len = total_enc - 16 - 32;
    if !body_len.is_multiple_of(16) {
        cleanup();
        return Err("Encrypted attachment is not block-aligned.".to_owned());
    }
    let mut sha = Sha256::new();
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&key[32..64]).map_err(|_| {
        cleanup();
        "Invalid HMAC key.".to_owned()
    })?;
    let mut tail = Vec::<u8>::with_capacity(64);
    {
        let mut file = match std::fs::File::open(enc_path) {
            Ok(f) => f,
            Err(err) => {
                cleanup();
                return Err(err.to_string());
            }
        };
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = match file.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(err) => {
                    drop(file);
                    cleanup();
                    return Err(err.to_string());
                }
            };
            let slice = &buf[..n];
            sha.update(slice);
            if slice.len() >= 32 {
                if !tail.is_empty() {
                    mac.update(&tail);
                    tail.clear();
                }
                let cut = slice.len() - 32;
                if cut > 0 {
                    mac.update(&slice[..cut]);
                }
                tail.extend_from_slice(&slice[cut..]);
            } else {
                tail.extend_from_slice(slice);
                if tail.len() > 32 {
                    let flush = tail.len() - 32;
                    mac.update(&tail[..flush]);
                    tail.drain(..flush);
                }
            }
        }
    }
    if !expected_digest.is_empty() && sha.finalize().as_slice() != expected_digest {
        cleanup();
        return Err("Attachment digest mismatch.".to_owned());
    }
    if tail.len() != 32 || mac.verify_slice(&tail).is_err() {
        cleanup();
        return Err("Attachment MAC verification failed.".to_owned());
    }
    let res = decrypt_verified_enc_file(enc_path, body_len, key, plaintext_size, dest);
    cleanup();
    res
}

fn decrypt_verified_enc_file(
    enc_path: &std::path::Path,
    body_len: u64,
    key: &[u8],
    plaintext_size: u32,
    dest: &std::path::Path,
) -> Result<u64, String> {
    use aes::cipher::{BlockDecrypt, KeyInit};
    use aes::Aes256;
    use std::io::{Read, Write};

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    let aes = Aes256::new_from_slice(&key[..32]).map_err(|_| "Invalid AES key.".to_owned())?;
    let mut enc_in = std::fs::File::open(enc_path).map_err(|err| err.to_string())?;
    let mut iv = [0u8; 16];
    enc_in.read_exact(&mut iv).map_err(|err| err.to_string())?;
    let mut out_file = protocol::private::create(dest).map_err(|err| err.to_string())?;

    let mut remaining = body_len;
    let mut prev = iv;
    let mut written = 0u64;
    let limit = if plaintext_size > 0 { Some(u64::from(plaintext_size)) } else { None };
    let mut chunk_buf = vec![0u8; 64 * 1024];

    while remaining > 0 {
        let to_read = (remaining as usize).min(chunk_buf.len());
        let to_read = (to_read / 16) * 16;
        if let Err(err) = enc_in.read_exact(&mut chunk_buf[..to_read]) {
            drop(out_file);
            let _ = std::fs::remove_file(dest);
            return Err(err.to_string());
        }
        remaining -= to_read as u64;
        for block_slice in chunk_buf[..to_read].chunks_exact_mut(16) {
            let ct_copy: [u8; 16] = block_slice.try_into().expect("16 bytes");
            aes.decrypt_block(block_slice.into());
            for i in 0..16 {
                block_slice[i] ^= prev[i];
            }
            prev = ct_copy;
        }
        let plain_slice = if remaining == 0 {
            let pad = chunk_buf[to_read - 1] as usize;
            if pad == 0 || pad > 16 || !chunk_buf[to_read - pad..to_read].iter().all(|&b| b as usize == pad) {
                drop(out_file);
                let _ = std::fs::remove_file(dest);
                return Err("Attachment padding is invalid.".to_owned());
            }
            &chunk_buf[..to_read - pad]
        } else {
            &chunk_buf[..to_read]
        };
        let to_write = match limit {
            Some(max) => {
                let left = max.saturating_sub(written) as usize;
                &plain_slice[..plain_slice.len().min(left)]
            }
            None => plain_slice,
        };
        if !to_write.is_empty() {
            if let Err(err) = out_file.write_all(to_write) {
                drop(out_file);
                let _ = std::fs::remove_file(dest);
                return Err(err.to_string());
            }
            written += to_write.len() as u64;
        }
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_solid_red_pixel_hashes_to_the_known_value() {
        assert_eq!(blurhash(&[(255, 0, 0)], 1, 1, 1, 1), "00TI:j");
    }

    #[test]
    fn a_file_send_is_recorded_and_a_phone_copy_does_not_wipe_it() {
        assert!(replace_outgoing_sync(true));
        assert!(!replace_outgoing_sync(false));
        assert_eq!(capabilities().max_file_bytes, MAX_FILE_BYTES);
        assert_eq!(capabilities().max_files, MAX_FILES);
        let quote = Quote {
            sent: 5,
            author: "a1a1a1a1-0000-4000-8000-000000000001".into(),
            text: String::new(),
            spans: Vec::new(),
            attachments: Vec::new(),
        };
        assert_eq!(
            call_line(
                "b2b2b2b2-0000-4000-8000-000000000002",
                &["card.png".into()],
                "the hand",
                None
            ),
            "send b2b2b2b2-0000-4000-8000-000000000002 files card.png the hand"
        );
        assert_eq!(
            call_line("c", &["a.png".into(), "b.png".into()], "", Some(&quote)),
            "send c files a.png,b.png (answering 5-a1a1a1a1-0000-4000-8000-000000000001)"
        );
    }

    #[test]
    fn streaming_attachment_cipher_round_trips_and_rejects_tampering() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("plain.bin");
        let iv = [0x42u8; 16];
        let mut key = [0u8; 64];
        for (i, b) in key.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(7).wrapping_add(3);
        }
        // Multi-chunk payload (> 64 KiB) to prove streaming across chunk boundaries.
        let plaintext: Vec<u8> = (0..140_000).map(|i| (i % 251) as u8).collect();
        let (ct, digest) = encrypt_attachment(iv, key, &plaintext);

        let len = decrypt_attachment_stream(std::io::Cursor::new(&ct), &key, &digest, plaintext.len() as u32, &dest)
            .expect("valid encrypted attachment");
        assert_eq!(len, plaintext.len() as u64);
        assert_eq!(std::fs::read(&dest).unwrap(), plaintext);
        assert!(!dest.with_extension("sigenc").exists(), "temp ciphertext file removed");

        // Tampered digest is rejected and leaves no partial plaintext.
        let mut bad_digest = digest;
        bad_digest[0] ^= 0xff;
        let bad_dest = dir.path().join("bad.bin");
        assert!(decrypt_attachment_stream(std::io::Cursor::new(&ct), &key, &bad_digest, 0, &bad_dest).is_err());
        assert!(!bad_dest.exists());

        // Tampered ciphertext fails MAC check.
        let mut bad_ct = ct.clone();
        bad_ct[20] ^= 1;
        assert!(decrypt_attachment_stream(std::io::Cursor::new(&bad_ct), &key, &[], 0, &bad_dest).is_err());
    }

    #[test]
    fn blurhash_decodes_to_a_valid_jpeg_preview() {
        let jpeg = decode_blurhash_jpeg("00TI:j", 640, 480).expect("valid blurhash jpeg");
        assert!(jpeg.starts_with(&[0xff, 0xd8, 0xff]), "JPEG SOI marker");
        assert!(decode_blurhash_jpeg("bad", 10, 10).is_none());
    }
}
