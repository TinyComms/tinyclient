//! A QR code from bytes, with nothing borrowed: byte mode, error-correction level M, versions 1
//! to 10 (up to 213 bytes; a Telegram login link is about 60 and lands on version 4, 33 modules
//! a side). ISO/IEC 18004: Reed-Solomon over GF(256), the eight masks scored by the four
//! penalties, format and version information.
//!
//! The sheet draws what `encode` returns as filled squares; nothing here knows about pixels.

/// The largest version made here. Past it a code is too fine for the 200 pt it is drawn in.
const MAX_VERSION: usize = 10;

/// Codewords in the whole symbol, data and correction together, by version.
const TOTAL: [usize; MAX_VERSION] = [26, 44, 70, 100, 134, 172, 196, 242, 292, 346];
/// Level M: correction codewords in each block, and how many blocks, by version.
const CORRECTION: [usize; MAX_VERSION] = [10, 16, 26, 18, 24, 16, 18, 22, 22, 26];
const BLOCKS: [usize; MAX_VERSION] = [1, 1, 1, 2, 2, 4, 4, 4, 5, 5];

/// The two bits that name level M in the format information. L is `0b01`.
const LEVEL_M: u32 = 0b00;

/// A finished symbol: `size` modules a side, without the quiet zone around it.
#[derive(Clone, PartialEq, Eq)]
pub struct Qr {
    size: usize,
    dark: Vec<bool>,
}

impl Qr {
    /// Modules per side: 21 for version 1, four more for each version after.
    pub fn size(&self) -> usize {
        self.size
    }

    /// Whether the module in column `x` of row `y` is ink. Outside the symbol nothing is.
    pub fn dark(&self, x: usize, y: usize) -> bool {
        x < self.size && y < self.size && self.dark[y * self.size + x]
    }
}

/// The smallest symbol that holds `text`, or `None` when it is longer than version 10 takes.
pub fn encode(text: &[u8]) -> Option<Qr> {
    let version = (1..=MAX_VERSION).find(|version| text.len() <= capacity(*version))?;
    let codewords = interleave(version, &data_codewords(version, text));
    let mut grid = Grid::new(version);
    grid.place(&codewords);
    let mask = (0..8)
        .min_by_key(|mask| {
            let mut tried = grid.clone();
            tried.finish(*mask);
            tried.penalty()
        })
        .unwrap_or(0);
    grid.finish(mask);
    Some(Qr { size: grid.size, dark: grid.dark })
}

/// Data codewords at level M.
fn data_len(version: usize) -> usize {
    TOTAL[version - 1] - CORRECTION[version - 1] * BLOCKS[version - 1]
}

/// Bits in the byte count that follows the mode: versions 10 and up use two bytes.
fn count_bits(version: usize) -> usize {
    if version < 10 {
        8
    } else {
        16
    }
}

/// Bytes of text a version holds, after the mode and the count.
fn capacity(version: usize) -> usize {
    (data_len(version) * 8 - 4 - count_bits(version)) / 8
}

/// The message as the standard frames it: mode `0100`, the count, the bytes, a terminator, then
/// the two pad bytes in turn until the version is full.
fn data_codewords(version: usize, text: &[u8]) -> Vec<u8> {
    let mut bits = Bits::default();
    bits.push(0b0100, 4);
    bits.push(text.len() as u32, count_bits(version));
    for byte in text {
        bits.push(u32::from(*byte), 8);
    }
    let room = data_len(version) * 8;
    bits.push(0, (room - bits.len).min(4));
    bits.push(0, (8 - bits.len % 8) % 8);
    let mut codewords = bits.bytes;
    for pad in [0xec, 0x11].into_iter().cycle().take(data_len(version) - codewords.len()) {
        codewords.push(pad);
    }
    codewords
}

/// Bits packed most significant first, as the symbol reads them.
#[derive(Default)]
struct Bits {
    bytes: Vec<u8>,
    len: usize,
}

impl Bits {
    fn push(&mut self, value: u32, count: usize) {
        for at in (0..count).rev() {
            if self.len.is_multiple_of(8) {
                self.bytes.push(0);
            }
            if value >> at & 1 == 1 {
                self.bytes[self.len / 8] |= 0x80 >> (self.len % 8);
            }
            self.len += 1;
        }
    }
}

/// How many data codewords each block of a version holds. The later blocks are one longer when
/// the total does not divide evenly.
fn block_lens(version: usize) -> Vec<usize> {
    let (total, blocks, correction) = (TOTAL[version - 1], BLOCKS[version - 1], CORRECTION[version - 1]);
    let short = total / blocks - correction;
    let long_ones = total % blocks;
    (0..blocks).map(|block| short + usize::from(block >= blocks - long_ones)).collect()
}

/// Splits the data into its blocks, gives each its correction codewords, and deals them out a
/// codeword at a time: all the data first, then all the correction.
fn interleave(version: usize, data: &[u8]) -> Vec<u8> {
    let correction = CORRECTION[version - 1];
    let mut blocks = Vec::new();
    let mut rest = data;
    for len in block_lens(version) {
        let (block, after) = rest.split_at(len);
        blocks.push((block, remainder(block, correction)));
        rest = after;
    }
    let longest = blocks.iter().map(|(block, _)| block.len()).max().unwrap_or(0);
    let mut dealt = Vec::with_capacity(TOTAL[version - 1]);
    for at in 0..longest {
        dealt.extend(blocks.iter().filter_map(|(block, _)| block.get(at)));
    }
    for at in 0..correction {
        dealt.extend(blocks.iter().map(|(_, check)| check[at]));
    }
    dealt
}

/// A product in GF(256) under the QR polynomial x^8 + x^4 + x^3 + x^2 + 1.
fn multiply(a: u8, b: u8) -> u8 {
    let mut product = 0u8;
    for bit in (0..8).rev() {
        product = (product << 1) ^ if product & 0x80 != 0 { 0x1d } else { 0 };
        if b >> bit & 1 == 1 {
            product ^= a;
        }
    }
    product
}

/// The generator polynomial of `degree`: the product of (x - 2^i) for i below it. Highest power
/// first, without the leading 1.
fn generator(degree: usize) -> Vec<u8> {
    let mut terms = vec![0u8; degree];
    terms[degree - 1] = 1;
    let mut root = 1u8;
    for _ in 0..degree {
        for at in 0..degree {
            terms[at] = multiply(terms[at], root) ^ terms.get(at + 1).copied().unwrap_or(0);
        }
        root = multiply(root, 2);
    }
    terms
}

/// The Reed-Solomon correction codewords for one block: what is left of the data, shifted up
/// by `count` places, after dividing by the generator.
fn remainder(data: &[u8], count: usize) -> Vec<u8> {
    let divisor = generator(count);
    let mut left = vec![0u8; count];
    for byte in data {
        let factor = byte ^ left.remove(0);
        left.push(0);
        for (place, term) in left.iter_mut().zip(&divisor) {
            *place ^= multiply(*term, factor);
        }
    }
    left
}

/// What is left of `value`, shifted up by the divisor's degree, after dividing by `divisor`
/// with no carries: the check bits of the format and version information.
fn bch(value: u32, divisor: u32) -> u32 {
    let degree = 31 - divisor.leading_zeros();
    let mut left = value << degree;
    for bit in (degree..32).rev() {
        if left >> bit & 1 == 1 {
            left ^= divisor << (bit - degree);
        }
    }
    left
}

/// The 15 format bits: level and mask, ten check bits, and the fixed pattern that keeps them
/// from ever being all light.
fn format_bits(level: u32, mask: u32) -> u32 {
    let data = level << 3 | mask;
    (data << 10 | bch(data, 0x537)) ^ 0x5412
}

/// The 18 version bits of versions 7 and up: the number and twelve check bits.
fn version_bits(version: u32) -> u32 {
    version << 12 | bch(version, 0x1f25)
}

/// Where the centers of the alignment patterns fall, on both axes. Version 1 has none.
fn alignment(version: usize) -> &'static [usize] {
    match version {
        2 => &[6, 18],
        3 => &[6, 22],
        4 => &[6, 26],
        5 => &[6, 30],
        6 => &[6, 34],
        7 => &[6, 22, 38],
        8 => &[6, 24, 42],
        9 => &[6, 26, 46],
        10 => &[6, 28, 50],
        _ => &[],
    }
}

/// Whether mask `mask` flips the module in column `x` of row `y`.
fn masked(mask: u32, x: usize, y: usize) -> bool {
    match mask {
        0 => (x + y).is_multiple_of(2),
        1 => y.is_multiple_of(2),
        2 => x.is_multiple_of(3),
        3 => (x + y).is_multiple_of(3),
        4 => (x / 3 + y / 2).is_multiple_of(2),
        5 => x * y % 2 + x * y % 3 == 0,
        6 => (x * y % 2 + x * y % 3).is_multiple_of(2),
        _ => ((x + y) % 2 + x * y % 3).is_multiple_of(2),
    }
}

/// The symbol while it is being built. `function` marks the modules that are not data: the
/// patterns a reader finds the symbol by, and the format and version information.
#[derive(Clone)]
struct Grid {
    version: usize,
    size: usize,
    dark: Vec<bool>,
    function: Vec<bool>,
}

impl Grid {
    /// An empty symbol with every fixed pattern drawn and the information areas reserved.
    fn new(version: usize) -> Grid {
        let size = version * 4 + 17;
        let mut grid = Grid { version, size, dark: vec![false; size * size], function: vec![false; size * size] };
        // Timing: alternating lines joining the finders, dark on even places.
        for at in 0..size {
            grid.set(6, at, at % 2 == 0);
            grid.set(at, 6, at % 2 == 0);
        }
        // Finders in three corners, each with its light separator.
        for (x, y) in [(3, 3), (size - 4, 3), (3, size - 4)] {
            grid.finder(x as isize, y as isize);
        }
        // Alignment patterns on every crossing of the positions, except under a finder.
        let places = alignment(version);
        let last = places.len().saturating_sub(1);
        for (row, y) in places.iter().enumerate() {
            for (column, x) in places.iter().enumerate() {
                let under_finder = (column == 0 && row == 0) || (column == 0 && row == last) || (column == last && row == 0);
                if !under_finder {
                    grid.alignment(*x, *y);
                }
            }
        }
        // Reserved now so the data flows around them; the real bits come in `finish`.
        grid.format(0);
        grid.version_info();
        grid
    }

    fn at(&self, x: usize, y: usize) -> bool {
        self.dark[y * self.size + x]
    }

    /// Sets a module that is not data.
    fn set(&mut self, x: usize, y: usize, dark: bool) {
        self.dark[y * self.size + x] = dark;
        self.function[y * self.size + x] = true;
    }

    /// A 7 x 7 finder centered on (`x`, `y`) and the light ring around it, clipped to the symbol.
    fn finder(&mut self, x: isize, y: isize) {
        for dy in -4..=4isize {
            for dx in -4..=4isize {
                let (column, row) = (x + dx, y + dy);
                let inside = (0..self.size as isize).contains(&column) && (0..self.size as isize).contains(&row);
                if inside {
                    let ring = dx.abs().max(dy.abs());
                    self.set(column as usize, row as usize, ring != 2 && ring != 4);
                }
            }
        }
    }

    /// A 5 x 5 alignment pattern centered on (`x`, `y`).
    fn alignment(&mut self, x: usize, y: usize) {
        for dy in 0..5usize {
            for dx in 0..5usize {
                let ring = dx.abs_diff(2).max(dy.abs_diff(2));
                self.set(x + dx - 2, y + dy - 2, ring != 1);
            }
        }
    }

    /// Both copies of the format information for `mask`, and the module that is always dark.
    fn format(&mut self, mask: u32) {
        let bits = format_bits(LEVEL_M, mask);
        let bit = |at: usize| bits >> at & 1 == 1;
        let size = self.size;
        // Around the top-left finder, stepping over the timing lines.
        for at in 0..6 {
            self.set(8, at, bit(at));
        }
        self.set(8, 7, bit(6));
        self.set(8, 8, bit(7));
        self.set(7, 8, bit(8));
        for at in 9..15 {
            self.set(14 - at, 8, bit(at));
        }
        // Split between the other two finders.
        for at in 0..8 {
            self.set(size - 1 - at, 8, bit(at));
        }
        for at in 8..15 {
            self.set(8, size - 15 + at, bit(at));
        }
        self.set(8, size - 8, true);
    }

    /// Both copies of the version information, which only versions 7 and up carry.
    fn version_info(&mut self) {
        if self.version < 7 {
            return;
        }
        let bits = version_bits(self.version as u32);
        for at in 0..18 {
            let dark = bits >> at & 1 == 1;
            let (long, short) = (self.size - 11 + at % 3, at / 3);
            self.set(long, short, dark);
            self.set(short, long, dark);
        }
    }

    /// The data modules in the order codewords are laid: two columns at a time from the right
    /// edge, snaking up and down, stepping over the vertical timing line.
    fn path(&self) -> Vec<(usize, usize)> {
        let mut path = Vec::new();
        let mut right = self.size - 1;
        let mut upward = true;
        loop {
            if right == 6 {
                right = 5;
            }
            for step in 0..self.size {
                let y = if upward { self.size - 1 - step } else { step };
                for x in [right, right - 1] {
                    if !self.function[y * self.size + x] {
                        path.push((x, y));
                    }
                }
            }
            upward = !upward;
            if right < 2 {
                return path;
            }
            right -= 2;
        }
    }

    /// Lays the codewords, most significant bit first. The few modules left over stay light.
    fn place(&mut self, codewords: &[u8]) {
        for (at, (x, y)) in self.path().into_iter().enumerate() {
            let dark = codewords.get(at / 8).is_some_and(|byte| byte >> (7 - at % 8) & 1 == 1);
            self.dark[y * self.size + x] = dark;
        }
    }

    /// Flips the data modules the mask picks. Doing it twice undoes it.
    fn mask(&mut self, mask: u32) {
        for y in 0..self.size {
            for x in 0..self.size {
                let at = y * self.size + x;
                if !self.function[at] && masked(mask, x, y) {
                    self.dark[at] = !self.dark[at];
                }
            }
        }
    }

    /// Applies a mask and writes the format information that names it.
    fn finish(&mut self, mask: u32) {
        self.mask(mask);
        self.format(mask);
    }

    /// How hard the symbol is to read, by the standard's four rules. The mask with the lowest
    /// score is used.
    fn penalty(&self) -> usize {
        let size = self.size;
        let mut penalty = 0;
        // Rules 1 and 3 read lines: every row, then every column.
        let mut line = vec![false; size];
        for across in [true, false] {
            for a in 0..size {
                for (b, module) in line.iter_mut().enumerate() {
                    *module = if across { self.at(b, a) } else { self.at(a, b) };
                }
                penalty += runs(&line) + finder_like(&line);
            }
        }
        // Rule 2: every 2 x 2 block of one color.
        for y in 0..size - 1 {
            for x in 0..size - 1 {
                let color = self.at(x, y);
                if color == self.at(x + 1, y) && color == self.at(x, y + 1) && color == self.at(x + 1, y + 1) {
                    penalty += 3;
                }
            }
        }
        // Rule 4: ten for every five percent the ink is away from half.
        let dark = self.dark.iter().filter(|dark| **dark).count();
        let total = size * size;
        let away = (dark * 20).abs_diff(total * 10);
        penalty + (away.div_ceil(total).saturating_sub(1)) * 10
    }
}

/// Rule 1: a run of five modules of one color costs 3, and each module past five one more.
fn runs(line: &[bool]) -> usize {
    line.chunk_by(|a, b| a == b).map(|run| run.len()).filter(|len| *len >= 5).map(|len| len - 2).sum()
}

/// Rule 3: 40 for each place a line looks like the middle of a finder, dark-light-dark-dark-
/// dark-light-dark, with four light modules on either side of it.
fn finder_like(line: &[bool]) -> usize {
    const BEFORE: [bool; 11] = [false, false, false, false, true, false, true, true, true, false, true];
    const AFTER: [bool; 11] = [true, false, true, true, true, false, true, false, false, false, false];
    line.windows(11).filter(|window| *window == BEFORE || *window == AFTER).count() * 40
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in for a login link of `len` bytes: the real prefix, then base64url letters.
    fn link(len: usize) -> Vec<u8> {
        let letters = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut link = b"tg://login?token=".to_vec();
        link.extend((0..len.saturating_sub(17)).map(|at| letters[(at * 37 + len) % letters.len()]));
        link.truncate(len);
        link
    }

    /// The value of a polynomial, highest power first, at `x`.
    fn evaluate(codewords: &[u8], x: u8) -> u8 {
        codewords.iter().fold(0, |sum, codeword| multiply(sum, x) ^ codeword)
    }

    /// Reads a finished symbol back the way a phone does, trusting none of the encoder's own
    /// bookkeeping but the map of which modules are data: the format information names the
    /// mask, the codewords are read off the unmasked grid and dealt back into blocks, every
    /// block must be a multiple of the generator (all syndromes zero), and the bytes are parsed
    /// out of the data.
    fn decode(qr: &Qr) -> Vec<u8> {
        let size = qr.size();
        let version = (size - 17) / 4;
        assert_eq!(version * 4 + 17, size);

        // Both copies of the format information, read from the modules.
        let first: Vec<(usize, usize)> = (0..6)
            .map(|at| (8, at))
            .chain([(8, 7), (8, 8), (7, 8)])
            .chain((9..15).map(|at| (14 - at, 8)))
            .collect();
        let second: Vec<(usize, usize)> = (0..8).map(|at| (size - 1 - at, 8)).chain((8..15).map(|at| (8, size - 15 + at))).collect();
        let read = |places: &[(usize, usize)]| {
            places.iter().enumerate().fold(0u32, |bits, (at, (x, y))| bits | u32::from(qr.dark(*x, *y)) << at)
        };
        let format = read(&first);
        assert_eq!(format, read(&second), "the two copies of the format information agree");
        let mask = (0..8).find(|mask| format_bits(LEVEL_M, *mask) == format).expect("level M and a known mask");

        // The version information, where there is any.
        if version >= 7 {
            for at in 0..18 {
                let dark = version_bits(version as u32) >> at & 1 == 1;
                assert_eq!(qr.dark(size - 11 + at % 3, at / 3), dark);
                assert_eq!(qr.dark(at / 3, size - 11 + at % 3), dark);
            }
        }

        // Unmask and read the codewords.
        let mut grid = Grid::new(version);
        grid.dark.clone_from(&qr.dark);
        grid.mask(mask);
        let path = grid.path();
        assert_eq!(path.len() / 8, TOTAL[version - 1], "the data modules hold exactly the version's codewords");
        let mut codewords = vec![0u8; TOTAL[version - 1]];
        for (at, (x, y)) in path.iter().enumerate().take(codewords.len() * 8) {
            if grid.at(*x, *y) {
                codewords[at / 8] |= 0x80 >> (at % 8);
            }
        }

        // Deal them back into blocks.
        let lens = block_lens(version);
        let correction = CORRECTION[version - 1];
        let mut blocks: Vec<Vec<u8>> = lens.iter().map(|_| Vec::new()).collect();
        let mut dealt = codewords.iter();
        for at in 0..*lens.iter().max().unwrap() {
            for (block, len) in blocks.iter_mut().zip(&lens) {
                if at < *len {
                    block.push(*dealt.next().unwrap());
                }
            }
        }
        let mut checks: Vec<Vec<u8>> = lens.iter().map(|_| Vec::new()).collect();
        for _ in 0..correction {
            for check in &mut checks {
                check.push(*dealt.next().unwrap());
            }
        }
        assert!(dealt.next().is_none());

        // Every block, data then correction, is zero at each root of the generator.
        let mut data: Vec<u8> = Vec::new();
        for (block, check) in blocks.iter().zip(&checks) {
            let whole: Vec<u8> = block.iter().chain(check).copied().collect();
            let mut root = 1u8;
            for _ in 0..correction {
                assert_eq!(evaluate(&whole, root), 0, "a syndrome is not zero");
                root = multiply(root, 2);
            }
            data.extend(block);
        }

        // Mode, count, bytes.
        let bit = |at: usize| u32::from(data[at / 8] >> (7 - at % 8) & 1u8);
        let number = |from: usize, count: usize| (from..from + count).fold(0u32, |value, at| value << 1 | bit(at));
        assert_eq!(number(0, 4), 0b0100, "byte mode");
        let count = number(4, count_bits(version)) as usize;
        let start = 4 + count_bits(version);
        (0..count).map(|at| number(start + at * 8, 8) as u8).collect()
    }

    #[test]
    fn reed_solomon_matches_the_hello_world_example() {
        // ISO 18004 / Thonky, "HELLO WORLD" at 1-M: sixteen data codewords, ten for correction.
        let data = [32, 91, 11, 120, 209, 114, 220, 77, 67, 64, 236, 17, 236, 17, 236, 17];
        assert_eq!(remainder(&data, 10), [196, 35, 39, 119, 235, 215, 231, 226, 93, 23]);
        // The generator polynomials the standard prints for 7 and 10 codewords, as powers of 2.
        let power = |exponent: usize| (0..exponent).fold(1u8, |value, _| multiply(value, 2));
        assert_eq!(generator(7), [87, 229, 146, 149, 238, 102, 21].map(power));
        assert_eq!(generator(10), [251, 67, 46, 61, 118, 70, 64, 94, 32, 45].map(power));
        // The field itself: 2^8 wraps to 29, and 2 has order 255.
        assert_eq!(multiply(0x80, 2), 0x1d);
        assert_eq!(power(255), 1);
    }

    #[test]
    fn format_and_version_information_match_the_printed_strings() {
        assert_eq!(format_bits(LEVEL_M, 0), 0b101010000010010);
        assert_eq!(format_bits(0b01, 4), 0b110011000101111);
        // The rest of the standard's table for level M.
        let level_m = [0b101010000010010, 0b101000100100101, 0b101111001111100, 0b101101101001011];
        assert_eq!([0, 1, 2, 3].map(|mask| format_bits(LEVEL_M, mask)), level_m);
        assert_eq!(version_bits(7), 0b000111110010010100);
        assert_eq!(version_bits(10), 0b001010010011010011);
    }

    #[test]
    fn the_message_is_framed_as_the_standard_says() {
        // "hi" at version 1: mode 0100, count 00000010, 'h' 'i', terminator, then the pad bytes.
        let framed = data_codewords(1, b"hi");
        assert_eq!(framed, [0x40, 0x26, 0x86, 0x90, 0xec, 0x11, 0xec, 0x11, 0xec, 0x11, 0xec, 0x11, 0xec, 0x11, 0xec, 0x11]);
        // The capacities the standard lists for bytes at level M.
        assert_eq!((1..=10).map(capacity).collect::<Vec<_>>(), [14, 26, 42, 62, 84, 106, 122, 152, 180, 213]);
        // Blocks: version 5 is two of 43; version 8 two of 38 and two of 39; version 10 four of 43 and one of 44.
        assert_eq!(block_lens(5), [43, 43]);
        assert_eq!(block_lens(8), [38, 38, 39, 39]);
        assert_eq!(block_lens(10), [43, 43, 43, 43, 44]);
        // A full version takes no terminator and no pad.
        assert_eq!(data_codewords(1, &[0xff; 14]).len(), 16);
    }

    #[test]
    fn the_fixed_patterns_are_where_a_reader_looks_for_them() {
        for len in [1, 14, 15, 60, 62, 63, 84, 107, 123, 181, 213] {
            let qr = encode(&link(len)).unwrap();
            let size = qr.size();
            let version = (size - 17) / 4;
            // Finders: a dark 7 x 7 ring, a light ring, a dark 3 x 3 heart; light all around.
            for (left, top) in [(0, 0), (size - 7, 0), (0, size - 7)] {
                for dy in 0..7usize {
                    for dx in 0..7usize {
                        let ring = dx.abs_diff(3).max(dy.abs_diff(3));
                        assert_eq!(qr.dark(left + dx, top + dy), ring != 2, "finder at {left},{top} of version {version}");
                    }
                }
            }
            for at in 0..8 {
                for (x, y) in [(7, at), (at, 7), (size - 8, at), (size - 8 + at, 7), (7, size - 8 + at), (at, size - 8)] {
                    assert!(!qr.dark(x, y), "separator at {x},{y} of version {version}");
                }
            }
            // Timing: alternating between the finders.
            for at in 8..size - 8 {
                assert_eq!(qr.dark(at, 6), at % 2 == 0);
                assert_eq!(qr.dark(6, at), at % 2 == 0);
            }
            // The module that is always dark.
            assert!(qr.dark(8, size - 8));
            // Alignment: the bottom-right one is always there from version 2, four in from the edge.
            if version >= 2 {
                let center = size - 7;
                for dy in 0..5usize {
                    for dx in 0..5usize {
                        let ring = dx.abs_diff(2).max(dy.abs_diff(2));
                        assert_eq!(qr.dark(center + dx - 2, center + dy - 2), ring != 1, "alignment of version {version}");
                    }
                }
            }
            // From version 7 there are more: six in all, the middle one at the center.
            if version >= 7 {
                let middle = alignment(version)[1];
                assert_eq!(middle, size / 2);
                assert!(qr.dark(middle, middle) && !qr.dark(middle + 1, middle) && qr.dark(middle + 2, middle));
            }
        }
    }

    #[test]
    fn a_symbol_reads_back_as_what_went_in() {
        // Every version, at its edges: the shortest and longest text each one takes.
        let mut longest = 0;
        for version in 1..=MAX_VERSION {
            for len in [longest + 1, capacity(version)] {
                let text = link(len);
                let qr = encode(&text).unwrap();
                assert_eq!(qr.size(), version * 4 + 17, "{len} bytes take version {version}");
                assert_eq!(decode(&qr), text, "{len} bytes at version {version}");
            }
            longest = capacity(version);
        }
        // Bytes that are not text, and nothing at all.
        let odd: Vec<u8> = (0..=255).rev().collect();
        assert_eq!(decode(&encode(&odd[..200]).unwrap()), &odd[..200]);
        assert_eq!(decode(&encode(b"").unwrap()), b"");
        // Past version 10 there is no code.
        assert!(encode(&[b'a'; 214]).is_none());
    }

    #[test]
    fn a_login_link_is_version_four_and_fits_the_tile() {
        let qr = encode(b"tg://login?token=AQJ3kd0P7m2xYbV5cRt8hLwZq1NnEoU6sGfTiKy4DaM").unwrap();
        assert_eq!(qr.size(), 33);
        // 200 / 33 is 6 points a module: 198 across, inside the 200 the tile leaves.
        assert_eq!(200 / qr.size(), 6);
        assert!(!qr.dark(33, 0) && !qr.dark(0, 33));
    }

    #[test]
    fn every_mask_is_scored_and_the_lowest_wins() {
        let text = link(60);
        let version = 4;
        let codewords = interleave(version, &data_codewords(version, &text));
        let mut grid = Grid::new(version);
        grid.place(&codewords);
        let scores: Vec<usize> = (0..8)
            .map(|mask| {
                let mut tried = grid.clone();
                tried.finish(mask);
                tried.penalty()
            })
            .collect();
        let best = scores.iter().position(|score| score == scores.iter().min().unwrap()).unwrap() as u32;
        let mut expected = grid.clone();
        expected.finish(best);
        assert!(encode(&text).unwrap().dark == expected.dark);
        // Masking twice is no mask, which is what lets a reader undo it.
        let mut twice = grid.clone();
        twice.mask(3);
        twice.mask(3);
        assert!(twice.dark == grid.dark);
        // The rules, on lines small enough to count by hand.
        let line = |text: &str| text.chars().map(|c| c == '#').collect::<Vec<_>>();
        assert_eq!(runs(&line("#####.")), 3);
        assert_eq!(runs(&line("#######.....#.#")), 5 + 3);
        assert_eq!(runs(&line("####.####")), 0);
        assert_eq!(finder_like(&line("#.###.#....")), 40);
        assert_eq!(finder_like(&line("....#.###.#....")), 80);
        assert_eq!(finder_like(&line("#.###.#...#")), 0);
    }
}
