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
    match mime {
        "image/png" | "image/jpeg" | "image/webp" => MediaKind::Image,
        "image/gif" => MediaKind::Gif,
        "video/mp4" | "video/quicktime" | "video/webm" => MediaKind::Video,
        "audio/mpeg" | "audio/mp4" | "audio/ogg" | "audio/wav" => MediaKind::Audio,
        _ => MediaKind::File,
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
}
