//! dlctl: one request to the chatd of a TINYCLIENT_DIR, printed as JSON.
//!
//!   dlctl <TINYCLIENT_DIR> '<request json>'     e.g. '{"op":"status"}'
//!   dlctl <TINYCLIENT_DIR> @b64:<base64 of the json>   (for ssh: no quoting to get wrong)
//!   dlctl <TINYCLIENT_DIR> --sock               the socket/pipe path it would use
//!
//! Exit 0 with the answer on stdout; 2 when chatd is not there; 3 on a bad request; 4 when the
//! answer is `{"op":"error",...}` (printed too). Read timeout 60 s (`wait` answers within 30 s).
use std::time::Duration;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: dlctl <TINYCLIENT_DIR> '<request json>' | --sock");
        std::process::exit(3);
    }
    // support_dir() reads TINYCLIENT_DIR; set it for this process only.
    std::env::set_var("TINYCLIENT_DIR", &args[1]);
    let path = protocol::chat_sock_path();
    if args[2] == "--sock" {
        println!("{}", path.display());
        return;
    }
    // `@b64:<base64 of the json>` survives any shell and ssh quoting (Windows cmd included).
    let text = match args[2].strip_prefix("@b64:") {
        Some(b) => match b64(b) {
            Some(t) => t,
            None => {
                eprintln!("bad @b64: argument");
                std::process::exit(3);
            }
        },
        None => args[2].clone(),
    };
    let request: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("bad request json: {e}");
            std::process::exit(3);
        }
    };
    let mut stream = match protocol::ChatStream::connect(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("chatd not reachable at {}: {e}", path.display());
            std::process::exit(2);
        }
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    if let Err(e) = protocol::write_frame(&mut stream, &request) {
        eprintln!("write: {e}");
        std::process::exit(2);
    }
    let answer: serde_json::Value = match protocol::read_frame(&mut stream) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("read: {e}");
            std::process::exit(2);
        }
    };
    println!("{}", serde_json::to_string(&answer).unwrap_or_default());
    if answer.get("op").and_then(|v| v.as_str()) == Some("error") {
        std::process::exit(4);
    }
}

/// Standard base64 (padding optional), without a crate.
fn b64(s: &str) -> Option<String> {
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            _ => return None,
        } as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    String::from_utf8(out).ok()
}
