//! Where the demo's files are fetched from: a server on localhost that makes each one when it
//! is asked for. It stands in for the networks' file stores, so a file the demo left unfetched
//! goes through the real media cache and shows loading, then landed or failed.

use super::png::{self, Art};
use std::io::{self, BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::thread::sleep;
use std::time::Duration;

/// Starts the server and answers its origin, `http://127.0.0.1:PORT`.
pub fn start() -> io::Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let origin = format!("http://{}", listener.local_addr()?);
    std::thread::Builder::new().name("demo-files".into()).spawn(move || {
        for stream in listener.incoming().flatten() {
            let _ = std::thread::Builder::new().stack_size(256 * 1024).spawn(move || {
                let _ = answer(stream);
            });
        }
    })?;
    Ok(origin)
}

/// A one-page PDF of exactly `size` bytes, when `size` is large enough to hold one.
fn pdf(size: usize) -> Vec<u8> {
    let build = |pad: usize| {
        let mut out = format!("%PDF-1.4\n%{}\n", "-".repeat(pad)).into_bytes();
        let text = "BT /F1 24 Tf 72 700 Td (Trail camera wiring) Tj ET";
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
             /Resources << /Font << /F1 5 0 R >> >> >>"
                .to_owned(),
            format!("<< /Length {} >>\nstream\n{text}\nendstream", text.len()),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
        ];
        let mut offsets = Vec::new();
        for (number, object) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend(format!("{} 0 obj\n{object}\nendobj\n", number + 1).bytes());
        }
        let xref = out.len();
        out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).bytes());
        for offset in offsets {
            out.extend(format!("{offset:010} 00000 n \n").bytes());
        }
        let trailer = format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1);
        out.extend(trailer.bytes());
        out
    };
    let mut pad = 0;
    // The padding moves the cross-reference table, whose offset is written in digits: settle it.
    for _ in 0..4 {
        let length = build(pad).len();
        if length == size {
            break;
        }
        pad = (pad + size).saturating_sub(length);
    }
    build(pad)
}

/// A zip of exactly `size` bytes holding one stored text file.
fn zip(size: usize) -> Vec<u8> {
    let name = b"camera.log";
    let fixed = 30 + name.len() + 46 + name.len() + 22;
    let line = b"03:12:44 motion on trail camera 2, 14 frames kept\n";
    let data: Vec<u8> = line.iter().copied().cycle().take(size.saturating_sub(fixed)).collect();
    let crc = png::crc32(&[&data]);
    let length = data.len() as u32;
    let mut out = Vec::with_capacity(size);
    // Local header: version 2.0, stored, no flags.
    out.extend(b"PK\x03\x04\x14\0\0\0\0\0\0\0\x21\0");
    for word in [crc, length, length] {
        out.extend(word.to_le_bytes());
    }
    out.extend((name.len() as u16).to_le_bytes());
    out.extend([0, 0]);
    out.extend(name);
    out.extend(&data);
    let directory = out.len() as u32;
    out.extend(b"PK\x01\x02\x14\0\x14\0\0\0\0\0\0\0\x21\0");
    for word in [crc, length, length] {
        out.extend(word.to_le_bytes());
    }
    out.extend((name.len() as u16).to_le_bytes());
    // No extra field or comment, disk 0, no attributes, and the local header is at 0.
    out.extend([0; 16]);
    out.extend(name);
    let directory_size = out.len() as u32 - directory;
    out.extend(b"PK\x05\x06\0\0\0\0\x01\0\x01\0");
    out.extend(directory_size.to_le_bytes());
    out.extend(directory.to_le_bytes());
    out.extend([0, 0]);
    out
}

/// Bytes of a [`wav`] that lasts `seconds`.
pub fn wav_bytes(seconds: u32) -> u64 {
    44 + 8_000 * seconds as u64
}

/// Two falling notes, 8 kHz, 8 bits, one channel.
fn wav(seconds: u32) -> Vec<u8> {
    let samples = 8_000 * seconds;
    let mut out = Vec::with_capacity(wav_bytes(seconds) as usize);
    out.extend(b"RIFF");
    out.extend((36 + samples).to_le_bytes());
    out.extend(b"WAVEfmt \x10\0\0\0\x01\0\x01\0");
    for word in [8_000u32, 8_000] {
        out.extend(word.to_le_bytes());
    }
    out.extend(b"\x01\0\x08\0data");
    out.extend(samples.to_le_bytes());
    for n in 0..samples {
        let t = n as f32 / 8_000.0;
        let pitch = if t % 1.0 < 0.5 { 440.0 } else { 330.0 };
        let fade = 1.0 - (t % 0.5) * 1.6;
        let wave = (t * pitch * std::f32::consts::TAU).sin() * fade.max(0.0);
        out.push((128.0 + wave * 70.0) as u8);
    }
    out
}

/// What a path serves: its content type and bytes. `None` is a 404.
fn file(path: &str) -> Option<(&'static str, Vec<u8>)> {
    let mut parts = path.trim_start_matches('/').split('/');
    let kind = parts.next()?;
    let art = if kind == "art" { Art::from_letter(parts.next()?) } else { None };
    let mut number = || parts.next()?.parse::<u32>().ok();
    match kind {
        "art" => {
            let (seed, width, height) = (number()?, number()?, number()?);
            let art = art?;
            (width <= 4096 && height <= 4096).then(|| ("image/png", png::encode(art, seed, width, height)))
        }
        "pdf" => Some(("application/pdf", pdf(number()? as usize))),
        "zip" => Some(("application/zip", zip(number()? as usize))),
        "wav" => Some(("audio/wav", wav(number()?.min(60)))),
        "photo" => {
            let name = parts.next()?;
            super::photos::bytes(name).map(|bytes| (super::photos::mime(name), bytes))
        }
        _ => None,
    }
}

fn answer(stream: TcpStream) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut reader = BufReader::new(stream);
    let mut request = String::new();
    reader.read_line(&mut request)?;
    let mut line = String::new();
    while reader.read_line(&mut line)? > 2 {
        line.clear();
    }
    let target = request.split_whitespace().nth(1).unwrap_or("/");
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    // `?wait=MS` holds the answer back so the loading state can be seen.
    if let Some(wait) = query.strip_prefix("wait=").and_then(|wait| wait.parse::<u64>().ok()) {
        sleep(Duration::from_millis(wait.min(10_000)));
    }
    let mut stream = reader.into_inner();
    match file(path) {
        Some((mime, bytes)) => {
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                bytes.len()
            );
            stream.write_all(head.as_bytes())?;
            stream.write_all(&bytes)
        }
        None => stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_are_exactly_the_size_they_were_asked_for() {
        for size in [900, 4_096, 188_416] {
            let pdf = pdf(size);
            assert_eq!(pdf.len(), size);
            assert!(pdf.starts_with(b"%PDF-1.4") && pdf.ends_with(b"%%EOF\n"));
            assert_eq!(zip(size).len(), size);
        }
        assert_eq!(wav(4).len() as u64, wav_bytes(4));
    }
}
