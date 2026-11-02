//! Pictures for the demo, drawn and encoded here so chatd links no image code. Every picture is
//! a valid 8-bit PNG of exactly the size asked for: RGB, or RGBA for the kind that has a
//! transparent background (stickers and custom emoji).

use std::f32::consts::TAU;

/// What a picture shows. Each is flat colours and long runs, which is what the encoder below
/// can shrink.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Art {
    /// A head and shoulders on one colour.
    Person,
    /// A Space's icon: a tilted square on one colour.
    Badge,
    /// A sky, a sun and two hills.
    Scene,
    /// Wide bands of colour, as a poster or a card.
    Bands,
    /// One shape on nothing, for stickers and custom emoji: everything outside it is transparent.
    Blob,
}

impl Art {
    pub fn letter(self) -> char {
        match self {
            Art::Person => 'p',
            Art::Badge => 'b',
            Art::Scene => 's',
            Art::Bands => 'n',
            Art::Blob => 'o',
        }
    }

    /// Bytes a pixel: the shape on nothing carries an alpha channel.
    fn channels(self) -> usize {
        if self == Art::Blob { 4 } else { 3 }
    }

    pub fn from_letter(letter: &str) -> Option<Art> {
        [Art::Person, Art::Badge, Art::Scene, Art::Bands, Art::Blob]
            .into_iter()
            .find(|art| letter.len() == 1 && letter.starts_with(art.letter()))
    }
}

type Rgb = [u8; 3];

/// `hue` in degrees, `saturation` and `value` in percent.
fn hsv(hue: u32, saturation: u32, value: u32) -> Rgb {
    let (h, s, v) = ((hue % 360) as f32 / 60.0, saturation as f32 / 100.0, value as f32 / 100.0);
    let c = v * s;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    [r, g, b].map(|channel| ((channel + m) * 255.0) as u8)
}

fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    [0, 1, 2].map(|i| (a[i] as f32 + (b[i] as f32 - a[i] as f32) * t) as u8)
}

fn inside(x: f32, y: f32, cx: f32, cy: f32, rx: f32, ry: f32) -> bool {
    let (dx, dy) = ((x - cx) / rx, (y - cy) / ry);
    dx * dx + dy * dy <= 1.0
}

/// The colour of one pixel of the small pictures. `u` and `v` run from 0 to 1 across and down.
fn pixel(art: Art, hue: u32, u: f32, v: f32) -> Rgb {
    match art {
        Art::Person => {
            let head = inside(u, v, 0.5, 0.40, 0.19, 0.19);
            let shoulders = inside(u, v, 0.5, 1.02, 0.36, 0.34);
            if head || shoulders { hsv(hue, 22, 96) } else { hsv(hue, 55, 72) }
        }
        Art::Badge => {
            if (u - 0.5).abs() + (v - 0.5).abs() < 0.3 { hsv(hue + 40, 30, 98) } else { hsv(hue, 62, 62) }
        }
        _ => hsv(hue, 70, 88),
    }
}

/// Whether a pixel of the shape on nothing is the shape: a disc with a bite out of it.
fn solid(u: f32, v: f32) -> bool {
    inside(u, v, 0.5, 0.5, 0.42, 0.42) && !inside(u, v, 0.62, 0.38, 0.12, 0.12)
}

/// The pixels of a picture, each row led by its filter byte (0: the row as it is). The large
/// kinds work out what a row needs once, so a debug build still makes them quickly.
fn paint(art: Art, seed: u32, width: u32, height: u32) -> Vec<u8> {
    let hue = seed.wrapping_mul(47) % 360;
    let mut raw = Vec::with_capacity((width as usize * art.channels() + 1) * height as usize);
    let across = |x: u32| (x as f32 + 0.5) / width as f32;
    // Where each column's two hills begin, and the sun.
    let phase = (seed % 16) as f32;
    let hills: Vec<(f32, f32)> = match art {
        Art::Scene => (0..width)
            .map(|x| {
                let u = across(x);
                (0.62 + 0.07 * (u * TAU * 1.5 + phase).sin(), 0.78 + 0.06 * (u * TAU * 2.5 + phase * 2.0).sin())
            })
            .collect(),
        _ => Vec::new(),
    };
    let aspect = width as f32 / height as f32;
    let sun = (0.2 + (seed % 7) as f32 * 0.1) * aspect;
    let (near_hill, far_hill, sunlight) = (hsv(hue + 120, 50, 38), hsv(hue + 130, 38, 55), hsv(48, 30, 100));
    for y in 0..height {
        raw.push(0);
        let v = (y as f32 + 0.5) / height as f32;
        match art {
            Art::Scene => {
                // Sixteen steps of sky, so each row is one colour and most rows repeat.
                let step = ((v * 16.0 / 0.7) as u32).min(15);
                let sky = mix(hsv(hue + 200, 60, 85), hsv(hue + 20, 25, 100), step as f32 / 15.0);
                let sunny = (v - 0.28).abs() <= 0.09;
                for (x, (far, near)) in hills.iter().enumerate() {
                    let colour = if v > *near {
                        near_hill
                    } else if v > *far {
                        far_hill
                    } else if sunny && inside(across(x as u32) * aspect, v, sun, 0.28, 0.09, 0.09) {
                        sunlight
                    } else {
                        sky
                    };
                    raw.extend_from_slice(&colour);
                }
            }
            Art::Bands => {
                let band = (v * 5.0) as u32;
                let blocks = [0, 1, 2].map(|block| hsv(hue + band * 24 + block * 8, 45 + block * 10, 90 - band * 9));
                for x in 0..width {
                    raw.extend_from_slice(&blocks[(x * 3 / width) as usize]);
                }
            }
            Art::Blob => {
                for x in 0..width {
                    match solid(across(x), v) {
                        true => {
                            raw.extend_from_slice(&pixel(art, hue, across(x), v));
                            raw.push(255);
                        }
                        false => raw.extend_from_slice(&[0, 0, 0, 0]),
                    }
                }
            }
            _ => {
                for x in 0..width {
                    raw.extend_from_slice(&pixel(art, hue, across(x), v));
                }
            }
        }
    }
    raw
}

const fn crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut n = 0;
    while n < 256 {
        let mut c = n as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 == 1 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        table[n] = c;
        n += 1;
    }
    table
}

static CRC: [u32; 256] = crc_table();

pub fn crc32(parts: &[&[u8]]) -> u32 {
    let mut crc = !0u32;
    for part in parts {
        for byte in *part {
            crc = CRC[((crc ^ *byte as u32) & 0xff) as usize] ^ (crc >> 8);
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    // 5552 is the most bytes that cannot overflow before the modulo.
    for chunk in data.chunks(5552) {
        for byte in chunk {
            a += *byte as u32;
            b += a;
        }
        a %= 65_521;
        b %= 65_521;
    }
    (b << 16) | a
}

/// Deflate with the fixed Huffman codes: literals, and one kind of match (258 bytes from
/// `distance` back). That is enough to fold repeated pixels and repeated rows.
struct Deflate {
    out: Vec<u8>,
    held: u32,
    bits: u32,
}

impl Deflate {
    fn put(&mut self, value: u32, bits: u32) {
        self.held |= value << self.bits;
        self.bits += bits;
        while self.bits >= 8 {
            self.out.push(self.held as u8);
            self.held >>= 8;
            self.bits -= 8;
        }
    }

    /// Huffman codes go in most significant bit first.
    fn code(&mut self, code: u32, bits: u32) {
        self.put(code.reverse_bits() >> (32 - bits), bits);
    }

    fn literal(&mut self, byte: u8) {
        match byte {
            0..=143 => self.code(0x30 + byte as u32, 8),
            _ => self.code(0x190 + (byte as u32 - 144), 9),
        }
    }

    fn repeat(&mut self, distance: usize) {
        // Length 258 is symbol 285, with no extra bits.
        self.code(0xc5, 8);
        let (mut symbol, mut base) = (0u32, 1usize);
        loop {
            let extra = if symbol < 2 { 0 } else { symbol / 2 - 1 };
            if distance < base + (1 << extra) {
                self.code(symbol, 5);
                self.put((distance - base) as u32, extra);
                return;
            }
            base += 1 << extra;
            symbol += 1;
        }
    }
}

const RUN: usize = 258;

fn zlib(data: &[u8], row: usize, pixel: usize) -> Vec<u8> {
    let mut deflate = Deflate {
        out: vec![0x78, 0x01],
        held: 0,
        bits: 0,
    };
    // One final block, fixed codes.
    deflate.put(0b011, 3);
    let mut at = 0;
    'next: while at < data.len() {
        if at + RUN <= data.len() {
            for distance in [row, pixel] {
                if at >= distance && data[at..at + RUN] == data[at - distance..at - distance + RUN] {
                    deflate.repeat(distance);
                    at += RUN;
                    continue 'next;
                }
            }
        }
        deflate.literal(data[at]);
        at += 1;
    }
    deflate.code(0, 7);
    deflate.put(0, 7);
    deflate.out.extend(adler32(data).to_be_bytes());
    deflate.out
}

fn chunk(out: &mut Vec<u8>, name: &[u8; 4], data: &[u8]) {
    out.extend((data.len() as u32).to_be_bytes());
    out.extend(name);
    out.extend(data);
    out.extend(crc32(&[name, data]).to_be_bytes());
}

pub fn encode(art: Art, seed: u32, width: u32, height: u32) -> Vec<u8> {
    let row = width as usize * art.channels() + 1;
    let raw = paint(art, seed, width, height);
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::with_capacity(13);
    header.extend(width.to_be_bytes());
    header.extend(height.to_be_bytes());
    // 8 bits a channel, RGB (2) or RGBA (6), no interlace.
    header.extend([8, if art.channels() == 4 { 6 } else { 2 }, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &header);
    chunk(&mut out, b"IDAT", &zlib(&raw, row, art.channels()));
    chunk(&mut out, b"IEND", &[]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksums_match_the_published_values() {
        assert_eq!(crc32(&[b"123456789"]), 0xcbf4_3926);
        assert_eq!(adler32(b"Wikipedia"), 0x11e6_0398);
    }

    #[test]
    fn a_picture_says_its_own_size_and_shrinks() {
        let png = encode(Art::Scene, 3, 320, 200);
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), 320);
        assert_eq!(u32::from_be_bytes(png[20..24].try_into().unwrap()), 200);
        assert!(png.len() < 320 * 200 * 3 / 4, "{} bytes", png.len());
    }

    #[test]
    fn a_sticker_has_an_alpha_channel_and_its_corners_are_transparent() {
        let png = encode(Art::Blob, 3, 48, 48);
        assert_eq!(png[25], 6, "colour type RGBA");
        let raw = paint(Art::Blob, 3, 48, 48);
        assert_eq!(raw.len(), (48 * 4 + 1) * 48);
        assert_eq!(&raw[1..5], &[0, 0, 0, 0], "the top left pixel");
        let middle = (48 * 4 + 1) * 24 + 1 + 20 * 4;
        assert_eq!(raw[middle + 3], 255, "the shape itself is opaque");
    }
}
