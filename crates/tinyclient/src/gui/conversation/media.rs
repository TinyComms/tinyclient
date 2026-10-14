//! Where a message's attachments go and how big each box is. Everything is decided from the
//! sizes the network reported, before a byte of any picture has arrived, so a row never changes
//! height when one lands.

use super::icons::Icon;
use protocol::{Attachment, MediaKind};

pub const MAX_HEIGHT: f32 = 300.;
const GAP: f32 = 4.;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Fit {
    /// Fill the box, cropping evenly.
    Cover,
    /// Fill the box, keeping the top: a very tall picture shows where it starts.
    CoverTop,
    /// The whole picture inside the box.
    Contain,
}

/// The box of a picture shown alone.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Single {
    pub width: f32,
    pub height: f32,
    pub fit: Fit,
}

/// `max` is the widest a picture may be here: 400, or less in a narrow pane. Never upscaled.
pub fn single(width: u32, height: u32, max: f32) -> Single {
    if width == 0 || height == 0 {
        // The network did not say. A fixed box, never resized when the picture arrives.
        return Single { width: max.min(240.), height: 180., fit: Fit::Contain };
    }
    let (w, h) = (width as f32, height as f32);
    let scale = (max / w).min(MAX_HEIGHT / h).min(1.);
    let (scaled_w, scaled_h) = ((w * scale).round(), (h * scale).round());
    if scaled_h >= MAX_HEIGHT && scaled_w < 120. {
        return Single { width: 120., height: MAX_HEIGHT, fit: Fit::CoverTop };
    }
    if scaled_w >= max && scaled_h < 64. {
        return Single { width: max, height: 64., fit: Fit::Cover };
    }
    // Tiny pictures sit at their own size in a box big enough to click.
    Single { width: scaled_w.max(48.), height: scaled_h.max(48.), fit: Fit::Contain }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tile {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// The tiles of two or more pictures, and the height of the whole grid. At most four tiles: a
/// fifth picture and beyond are counted on the fourth.
pub fn grid(count: usize, max: f32) -> (Vec<Tile>, f32) {
    let tile = |x, y, width, height| Tile { x, y, width, height };
    let half = ((max - GAP) / 2.).floor();
    match count {
        0 | 1 => (Vec::new(), 0.),
        2 => (vec![tile(0., 0., half, half), tile(half + GAP, 0., half, half)], half),
        3 => {
            let big = ((max - GAP) * 2. / 3.).floor();
            let small = max - GAP - big;
            let each = ((big - GAP) / 2.).floor();
            let right = big + GAP;
            (
                vec![tile(0., 0., big, big), tile(right, 0., small, each), tile(right, each + GAP, small, each)],
                big,
            )
        }
        _ => {
            let total = (max * 0.75).floor();
            let each = ((total - GAP) / 2.).floor();
            let (right, below) = (half + GAP, each + GAP);
            (
                vec![
                    tile(0., 0., half, each),
                    tile(right, 0., half, each),
                    tile(0., below, half, each),
                    tile(right, below, half, each),
                ],
                total,
            )
        }
    }
}

/// How many pixels along its longer side a picture needs to fill a box of `width` x `height`
/// points at `scale` device pixels a point. Never more than the picture has.
pub fn decode_edge(natural: (u32, u32), width: f32, height: f32, fit: Fit, scale: f32) -> u32 {
    let (w, h) = (natural.0.max(1) as f32, natural.1.max(1) as f32);
    let ratio = match fit {
        Fit::Cover | Fit::CoverTop => (width / w).max(height / h),
        Fit::Contain => (width / w).min(height / h),
    };
    let edge = (w.max(h) * ratio * scale).round();
    if natural.0 == 0 || natural.1 == 0 {
        return (width.max(height) * scale).ceil() as u32;
    }
    (edge as u32).clamp(1, natural.0.max(natural.1))
}

/// What an attachment is drawn as.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Shape {
    Image,
    Gif,
    Video,
    Sticker,
    /// A voice message: its waveform, played in place.
    Voice,
    /// An audio file: its name over a bar, played in place.
    Audio,
    File,
}

pub fn shape(attachment: &Attachment) -> Shape {
    let has_still = attachment.thumb.is_some();
    match attachment.file.kind {
        _ if attachment.sticker => Shape::Sticker,
        MediaKind::Gif => Shape::Gif,
        MediaKind::Video if has_still => Shape::Video,
        MediaKind::Image | MediaKind::Emoji | MediaKind::Avatar | MediaKind::Icon => Shape::Image,
        MediaKind::Audio if !attachment.waveform.is_empty() => Shape::Voice,
        MediaKind::Audio => Shape::Audio,
        MediaKind::Video | MediaKind::File => Shape::File,
    }
}

/// Covered until the user asks to see it.
pub fn is_spoiler(attachment: &Attachment) -> bool {
    attachment.spoiler
}

impl Shape {
    /// Drawn as a picture, alone or as a tile.
    pub fn is_visual(self) -> bool {
        matches!(self, Shape::Image | Shape::Gif | Shape::Video)
    }
}

/// `2.4 MB`, the way Finder says it.
pub fn size_label(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes} bytes");
    }
    let mut value = bytes as f64 / 1000.;
    let mut unit = 0;
    while value >= 999.95 && unit < UNITS.len() - 1 {
        value /= 1000.;
        unit += 1;
    }
    if value < 10. && unit > 0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

/// `1:24`, or `1:02:03` past an hour.
pub fn duration_label(seconds: u32) -> String {
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

fn extension(name: &str) -> &str {
    name.rsplit_once('.').map_or("", |(_, extension)| extension)
}

/// The extension a type is known by, for a file whose name has none ("Voice message").
fn extension_for(mime: &str) -> Option<&'static str> {
    Some(match mime.split(';').next().unwrap_or("").trim() {
        "video/mp4" => "mp4",
        "video/quicktime" => "mov",
        "video/webm" => "webm",
        "video/x-matroska" => "mkv",
        "audio/ogg" | "audio/opus" => "ogg",
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/mp4" | "audio/x-m4a" | "audio/aac" => "m4a",
        "audio/wav" | "audio/x-wav" => "wav",
        "audio/flac" => "flac",
        "audio/webm" => "webm",
        "image/gif" => "gif",
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "application/pdf" => "pdf",
        _ => return None,
    })
}

/// The name a file is opened under: its own, made safe for a file system, with the extension
/// its type has when the name carries none, so the app that opens it knows what it is.
pub fn disk_name(attachment: &Attachment) -> String {
    let mut name: String = attachment
        .name
        .chars()
        .map(|c| if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '-' } else { c })
        .collect();
    name = name.trim().trim_start_matches('.').to_owned();
    if name.is_empty() {
        name = "file".to_owned();
    }
    protocol::clip(&mut name, 120);
    let known = extension(&name);
    let fitting = !known.is_empty() && known.len() <= 5 && !known.contains(' ');
    match extension_for(&attachment.mime) {
        Some(wanted) if !fitting => format!("{name}.{wanted}"),
        _ => name,
    }
}

/// The word for a file's type on its row, since truncation eats the extension: `PDF`.
pub fn type_label(attachment: &Attachment) -> String {
    let extension = extension(&attachment.name);
    if !extension.is_empty() && extension.len() <= 5 {
        return extension.to_uppercase();
    }
    match attachment.mime.split_once('/') {
        Some((_, sub)) if !sub.is_empty() => sub.rsplit(['.', '-', '+']).next().unwrap_or(sub).to_uppercase(),
        _ => "File".to_owned(),
    }
}

pub fn file_icon(attachment: &Attachment) -> Icon {
    let extension = extension(&attachment.name).to_lowercase();
    let mime = attachment.mime.as_str();
    match extension.as_str() {
        "zip" | "gz" | "tar" | "tgz" | "7z" | "rar" | "dmg" => Icon::Archive,
        "csv" | "xls" | "xlsx" | "numbers" | "tsv" => Icon::Sheet,
        "rs" | "js" | "ts" | "tsx" | "py" | "go" | "c" | "h" | "cpp" | "java" | "json" | "toml" | "yaml" | "yml"
        | "sh" | "html" | "css" | "sql" | "swift" | "kt" | "rb" => Icon::Code,
        "txt" | "md" | "pdf" | "doc" | "docx" | "rtf" | "pages" | "log" => Icon::FileText,
        _ if mime.starts_with("audio/") || attachment.file.kind == MediaKind::Audio => Icon::Music,
        _ if mime.starts_with("video/") || attachment.file.kind == MediaKind::Video => Icon::Video,
        _ if mime.starts_with("text/") => Icon::FileText,
        _ => Icon::File,
    }
}

/// Bar heights for a voice message, 3 to 20 px, from its loudness over time: each bar is the
/// loudest sample of its share of the message. A message with no waveform is a level line.
pub fn waveform(samples: &[u8], bars: usize) -> Vec<f32> {
    if samples.is_empty() {
        return vec![3.; bars];
    }
    // Quiet recordings still read as speech: the loudest sample is the full height.
    let peak = f32::from(samples.iter().copied().max().unwrap_or(1).max(1));
    (0..bars)
        .map(|bar| {
            let from = bar * samples.len() / bars;
            let to = ((bar + 1) * samples.len() / bars).clamp(from + 1, samples.len());
            let loudest = samples[from.min(samples.len() - 1)..to].iter().copied().max().unwrap_or(0);
            (3. + 17. * f32::from(loudest) / peak).round()
        })
        .collect()
}

/// The longest side `Fetch` should ask for so a picture fills a box of `width` x `height`
/// points at `scale`: the whole picture's, even where the box crops it. A size the network did
/// not give cannot be scaled from, so the box's own longest side is asked for.
pub fn fetch_size(natural: (u32, u32), width: f32, height: f32, fit: Fit, scale: f32) -> u32 {
    decode_edge(natural, width, height, fit, scale)
}

/// For a square box that shows the whole picture: an avatar, an emoji, a sticker.
pub fn square_size(points: f32, scale: f32) -> u32 {
    (points * scale).ceil() as u32
}

/// What a bot card's bar says, from the color the network gave it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Tone {
    Positive,
    Danger,
    Warning,
    Neutral,
}

/// Snaps `0xRRGGBB` to the nearest of the three semantic colors, or to none for a grey, so a
/// card never brings a network's palette into the conversation.
pub fn card_tone(color: Option<u32>) -> Tone {
    let Some(color) = color else {
        return Tone::Neutral;
    };
    let (r, g, b) = ((color >> 16 & 0xff) as f32, (color >> 8 & 0xff) as f32, (color & 0xff) as f32);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    if max - min < 40. {
        return Tone::Neutral;
    }
    let hue = if max == r {
        60. * ((g - b) / (max - min)).rem_euclid(6.)
    } else if max == g {
        60. * ((b - r) / (max - min) + 2.)
    } else {
        60. * ((r - g) / (max - min) + 4.)
    };
    match hue as u32 {
        0..=19 | 330..=360 => Tone::Danger,
        20..=69 => Tone::Warning,
        70..=179 => Tone::Positive,
        _ => Tone::Neutral,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::MediaFile;

    fn boxed(width: u32, height: u32) -> (f32, f32, Fit) {
        let single = single(width, height, 400.);
        (single.width, single.height, single.fit)
    }

    #[test]
    fn a_single_picture_fits_400_by_300_and_is_never_enlarged() {
        assert_eq!(boxed(2400, 1600), (400., 267., Fit::Contain));
        assert_eq!(boxed(1600, 2400), (200., 300., Fit::Contain));
        assert_eq!(boxed(320, 200), (320., 200., Fit::Contain));
        assert_eq!(boxed(400, 300), (400., 300., Fit::Contain));
        // In the thread pane the limit is the pane.
        let narrow = single(2400, 1600, 310.);
        assert_eq!((narrow.width, narrow.height), (310., 207.));
    }

    #[test]
    fn extreme_shapes_are_cropped_into_a_box_that_can_still_be_clicked() {
        assert_eq!(boxed(300, 3000), (120., 300., Fit::CoverTop));
        assert_eq!(boxed(4000, 200), (400., 64., Fit::Cover));
        assert_eq!(boxed(20, 20), (48., 48., Fit::Contain));
        assert_eq!(boxed(100, 30), (100., 48., Fit::Contain));
        assert_eq!(boxed(0, 0), (240., 180., Fit::Contain));
        // Small and tall, but not scaled down: its own size, not the tall crop.
        assert_eq!(boxed(100, 280), (100., 280., Fit::Contain));
    }

    #[test]
    fn grids_are_the_sizes_in_the_spec_at_400() {
        let sizes = |count| {
            let (tiles, height) = grid(count, 400.);
            (tiles.iter().map(|t| (t.x, t.y, t.width, t.height)).collect::<Vec<_>>(), height)
        };
        assert_eq!(sizes(2), (vec![(0., 0., 198., 198.), (202., 0., 198., 198.)], 198.));
        assert_eq!(
            sizes(3),
            (vec![(0., 0., 264., 264.), (268., 0., 132., 130.), (268., 134., 132., 130.)], 264.)
        );
        let four = vec![(0., 0., 198., 148.), (202., 0., 198., 148.), (0., 152., 198., 148.), (202., 152., 198., 148.)];
        assert_eq!(sizes(4), (four.clone(), 300.));
        assert_eq!(sizes(9), (four, 300.));
        // Every grid ends flush with its right edge.
        for count in 2..=4 {
            for max in [400., 310., 287.] {
                let (tiles, _) = grid(count, max);
                let right = tiles.iter().map(|t| t.x + t.width).fold(0., f32::max);
                assert!(max - right <= 1., "{count} tiles in {max}: ends at {right}");
            }
        }
    }

    #[test]
    fn pictures_are_decoded_at_the_size_they_are_shown_not_the_size_they_are() {
        // A 4000 x 3000 photo in a 198 x 148 tile at 2x: 396 px across, not 4000.
        assert_eq!(decode_edge((4000, 3000), 198., 148., Fit::Cover, 2.), 396);
        // Cover has to fill the short side, so a wide picture keeps more of its long one.
        assert_eq!(decode_edge((4000, 1000), 198., 198., Fit::Cover, 2.), 1584);
        assert_eq!(decode_edge((2400, 1600), 400., 267., Fit::Contain, 2.), 800);
        assert_eq!(decode_edge((320, 200), 320., 200., Fit::Contain, 2.), 320);
        assert_eq!(decode_edge((0, 0), 240., 180., Fit::Contain, 2.), 480);
    }

    #[test]
    fn labels_read_like_finder() {
        assert_eq!(size_label(512), "512 bytes");
        assert_eq!(size_label(2_400_000), "2.4 MB");
        assert_eq!(size_label(18_600_000), "19 MB");
        assert_eq!(size_label(840_000), "840 KB");
        assert_eq!(size_label(3_200_000_000), "3.2 GB");
        assert_eq!(duration_label(84), "1:24");
        assert_eq!(duration_label(7), "0:07");
        assert_eq!(duration_label(3723), "1:02:03");
    }

    #[test]
    fn an_attachment_is_drawn_by_what_it_is() {
        let attachment = |name: &str, kind, mime: &str, thumb: bool, duration| Attachment {
            spoiler: false,
            sticker: false,
            waveform: Vec::new(),
            name: name.into(),
            file: MediaFile::new("f", kind),
            thumb: thumb.then(|| MediaFile::new("t", MediaKind::Image)),
            mime: mime.into(),
            duration,
            round: false,
        };
        assert_eq!(shape(&attachment("a.png", MediaKind::Image, "image/png", true, 0)), Shape::Image);
        assert_eq!(shape(&attachment("a.mp4", MediaKind::Video, "video/mp4", true, 84)), Shape::Video);
        assert_eq!(shape(&attachment("a.mp4", MediaKind::Video, "video/mp4", false, 84)), Shape::File);
        let mut voice = attachment("voice.ogg", MediaKind::Audio, "audio/ogg", false, 47);
        assert_eq!(shape(&voice), Shape::Audio, "no waveform: the audio player");
        voice.waveform = vec![10, 200, 40];
        assert_eq!(shape(&voice), Shape::Voice);
        assert_eq!(shape(&attachment("song.mp3", MediaKind::Audio, "audio/mpeg", false, 0)), Shape::Audio);
        let mut sticker = attachment("s.webp", MediaKind::Image, "image/webp", true, 0);
        sticker.sticker = true;
        assert_eq!(shape(&sticker), Shape::Sticker);
        let report = attachment("Q3 report.final.pdf", MediaKind::File, "application/pdf", false, 0);
        assert_eq!((type_label(&report), file_icon(&report) == Icon::FileText), ("PDF".to_owned(), true));
        let bare = attachment("Makefile", MediaKind::File, "text/x-makefile", false, 0);
        assert_eq!(type_label(&bare), "MAKEFILE");
        let mut hidden = attachment("end.png", MediaKind::Image, "", true, 0);
        assert!(!is_spoiler(&hidden));
        hidden.spoiler = true;
        assert!(is_spoiler(&hidden));
    }

    #[test]
    fn each_box_asks_for_the_pixels_it_shows_at_the_screens_scale() {
        // A single 2400 x 1800 photo in its 400 x 300 box: 800 at 2x, 400 at 1x.
        assert_eq!(fetch_size((2400, 1800), 400., 300., Fit::Contain, 2.), 800);
        assert_eq!(fetch_size((2400, 1800), 400., 300., Fit::Contain, 1.), 400);
        // The three tile sizes. A tile is cropped, so the whole picture is larger than the box:
        // a 4:3 picture covering a 198 pt square is 264 pt wide.
        assert_eq!(fetch_size((1200, 900), 198., 198., Fit::Cover, 2.), 528);
        assert_eq!(fetch_size((1200, 900), 264., 264., Fit::Cover, 2.), 704);
        assert_eq!(fetch_size((900, 1200), 132., 130., Fit::Cover, 2.), 352);
        assert_eq!(fetch_size((1200, 900), 198., 148., Fit::Cover, 2.), 396);
        // Never more than the picture has, and a picture of unknown size asks for its box.
        assert_eq!(fetch_size((320, 320), 400., 300., Fit::Contain, 2.), 320);
        assert_eq!(fetch_size((0, 0), 240., 180., Fit::Contain, 2.), 480);
        // Sticker, link thumbnail, custom emoji, the reply quote's avatar.
        assert_eq!(
            [128., 72., 18., 16.].map(|points| square_size(points, 2.)),
            [256, 144, 36, 32]
        );
        assert_eq!(square_size(18., 1.), 18);
    }

    #[test]
    fn a_file_is_opened_under_a_name_its_app_understands() {
        let named = |name: &str, mime: &str| {
            let mut attachment = Attachment::new(name, MediaFile::new("f", MediaKind::File));
            attachment.mime = mime.to_owned();
            disk_name(&attachment)
        };
        assert_eq!(named("Voice message", "audio/ogg"), "Voice message.ogg");
        assert_eq!(named("Video message", "video/mp4"), "Video message.mp4");
        assert_eq!(named("clip.mov", "video/quicktime"), "clip.mov");
        assert_eq!(named("camera2-0312.mp4", "video/mp4"), "camera2-0312.mp4");
        assert_eq!(named("Q3 report.pdf", "application/pdf"), "Q3 report.pdf");
        assert_eq!(named("../../etc/passwd", ""), "-..-etc-passwd", "never a path");
        assert_eq!(named("", "video/webm"), "file.webm");
        assert_eq!(named("notes", "application/x-unknown"), "notes");
    }

    #[test]
    fn a_voice_message_draws_its_own_loudness() {
        let quiet_then_loud: Vec<u8> = (0..80).map(|n| if n < 40 { 10 } else { 200 }).collect();
        let bars = waveform(&quiet_then_loud, 40);
        assert_eq!(bars.len(), 40);
        assert!(bars[..20].iter().all(|tall| *tall == 4.), "{bars:?}");
        assert!(bars[20..].iter().all(|tall| *tall == 20.));
        // Fewer samples than bars: each sample is held, none is skipped.
        assert_eq!(waveform(&[0, 255], 4), [3., 3., 20., 20.]);
        assert_eq!(waveform(&[], 3), [3., 3., 3.]);
    }

    #[test]
    fn a_cards_color_is_one_of_ours() {
        assert_eq!(card_tone(Some(0x2eb67d)), Tone::Positive);
        assert_eq!(card_tone(Some(0xe01e5a)), Tone::Danger);
        assert_eq!(card_tone(Some(0xed4245)), Tone::Danger);
        assert_eq!(card_tone(Some(0xecb22e)), Tone::Warning);
        assert_eq!(card_tone(Some(0x5865f2)), Tone::Neutral);
        assert_eq!(card_tone(Some(0x808080)), Tone::Neutral);
        assert_eq!(card_tone(None), Tone::Neutral);
    }
}
