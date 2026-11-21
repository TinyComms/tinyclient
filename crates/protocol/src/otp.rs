//! One-time passcode (OTP) detection for incoming messages.
//!
//! Client-side and generic across every network. Only offers a copy action — never copies
//! automatically — so a false positive is harmless.

/// Keywords that indicate a one-time verification code in English, Spanish, French,
/// German, Portuguese, and Italian.
const KEYWORDS: &[&str] = &[
    "code",
    "otp",
    "passcode",
    "verification",
    "verify",
    "pin",
    "2fa",
    "one-time",
    "one time",
    "security",
    "login",
    "sign-in",
    "sign in",
    "confirmation",
    "confirm",
    "authenticate",
    "authentication",
    "token",
    // Spanish / Portuguese
    "código",
    "codigo",
    "verificación",
    "verificacion",
    "verificação",
    "verificacao",
    "clave",
    // French
    "vérification",
    "validation",
    // German
    "bestätigungscode",
    "verifizierungscode",
    "einmalcode",
    "sicherheitscode",
    "tan",
    // Italian
    "codice",
    "verifica",
];

/// Extracts a likely one-time passcode (4–8 digits, optionally hyphen-separated like `123-456`
/// or `G-123456`, or a short mixed alphanumeric token near an OTP keyword).
///
/// Returns `None` for plain messages, prices, dates, phone numbers, and order numbers without
/// verification context.
pub fn extract_code(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.len() > 2000 {
        return None;
    }

    // 1. WebOTP format: `@example.com #123456` on its own line (RFC / WICG WebOTP).
    for line in trimmed.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix('@') {
            if let Some((host, after)) = rest.split_once(" #") {
                if host.contains('.') && !host.contains(' ') {
                    let token = after.split_whitespace().next().unwrap_or("");
                    if let Some(code) = valid_digit_code(token) {
                        return Some(code);
                    }
                }
            }
        }
    }

    // 2. Google-style prefixed code `G-123456` (always 6 digits after `G-`).
    for word in trimmed.split_whitespace() {
        let clean = strip_edges(word);
        if let Some(digits) = clean.strip_prefix("G-").or_else(|| clean.strip_prefix("g-")) {
            if digits.len() == 6 && digits.bytes().all(|b| b.is_ascii_digit()) {
                return Some(digits.to_owned());
            }
        }
    }

    // 3. Require an OTP keyword in the message before scanning general tokens.
    let lower = trimmed.to_lowercase();
    let keyword_spans = find_keyword_spans(&lower);
    if keyword_spans.is_empty() {
        return None;
    }

    // Scan tokens with their byte offsets in `trimmed`.
    let mut best: Option<(usize, u8, String)> = None;
    let tokens = tokenize(trimmed);
    let mut i = 0;
    while i < tokens.len() {
        let (start, end, raw) = tokens[i];
        let clean = strip_edges(raw);

        // Skip currency amounts, decimals, dates, times, URLs, phone numbers with `+`.
        if is_excluded_token(trimmed, start, end, raw, clean) {
            i += 1;
            continue;
        }

        // Check space-separated 3+3 digits like `123 456`.
        if clean.len() == 3
            && clean.bytes().all(|b| b.is_ascii_digit())
            && i + 1 < tokens.len()
        {
            let (next_start, next_end, next_raw) = tokens[i + 1];
            let next_clean = strip_edges(next_raw);
            if next_start <= end + 2
                && next_clean.len() == 3
                && next_clean.bytes().all(|b| b.is_ascii_digit())
                && !is_excluded_token(trimmed, next_start, next_end, next_raw, next_clean)
            {
                if let Some(dist) = min_keyword_distance(&keyword_spans, start, next_end) {
                    if dist <= 120 {
                        let code = format!("{clean}{next_clean}");
                        consider(&mut best, dist, 0, code);
                        i += 2;
                        continue;
                    }
                }
            }
        }

        if let Some(dist) = min_keyword_distance(&keyword_spans, start, end) {
            if dist <= 120 {
                // Pure 4–8 digit code or hyphenated 3-3 / 4-4 code.
                if let Some(code) = valid_digit_code(clean) {
                    // Prefer 6-digit codes slightly over 4-digit (e.g. avoid picking a year when a 6-digit code exists).
                    let rank = if code.len() == 6 {
                        0
                    } else if code.len() == 4 && is_plausible_year(&code) {
                        3
                    } else {
                        1
                    };
                    consider(&mut best, dist, rank, code);
                } else if let Some(code) = valid_alnum_code(clean) {
                    consider(&mut best, dist, 2, code);
                }
            }
        }
        i += 1;
    }

    best.map(|(_, _, code)| code)
}

fn consider(best: &mut Option<(usize, u8, String)>, dist: usize, rank: u8, code: String) {
    let replace = match best {
        None => true,
        Some((best_dist, best_rank, _)) => (rank, dist) < (*best_rank, *best_dist),
    };
    if replace {
        *best = Some((dist, rank, code));
    }
}

fn find_keyword_spans(lower: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    for &kw in KEYWORDS {
        let mut from = 0;
        while let Some(pos) = lower[from..].find(kw) {
            let start = from + pos;
            let end = start + kw.len();
            let left_ok = start == 0
                || !lower[..start]
                    .chars()
                    .next_back()
                    .is_some_and(char::is_alphanumeric);
            let right_ok = end == lower.len()
                || !lower[end..]
                    .chars()
                    .next()
                    .is_some_and(char::is_alphanumeric);
            if left_ok && right_ok {
                spans.push((start, end));
            }
            from = end;
        }
    }
    spans
}

fn min_keyword_distance(spans: &[(usize, usize)], start: usize, end: usize) -> Option<usize> {
    spans
        .iter()
        .map(|&(ks, ke)| {
            if end <= ks {
                ks - end
            } else if start >= ke {
                start - ke
            } else {
                0
            }
        })
        .min()
}

fn tokenize(text: &str) -> Vec<(usize, usize, &str)> {
    let mut out = Vec::new();
    let mut start = None;
    for (idx, ch) in text.char_indices() {
        if ch.is_whitespace() {
            if let Some(s) = start.take() {
                out.push((s, idx, &text[s..idx]));
            }
        } else if start.is_none() {
            start = Some(idx);
        }
    }
    if let Some(s) = start {
        out.push((s, text.len(), &text[s..]));
    }
    out
}

fn strip_edges(token: &str) -> &str {
    token.trim_matches(|c: char| {
        matches!(
            c,
            '.' | ',' | ':' | ';' | '!' | '?' | '"' | '\'' | '(' | ')' | '[' | ']' | '<' | '>' | '#' | '*'
        )
    })
}

fn is_excluded_token(full: &str, start: usize, _end: usize, raw: &str, clean: &str) -> bool {
    if clean.is_empty() {
        return true;
    }
    // Currency prefix right before the digits.
    if raw.starts_with('$') || raw.starts_with('€') || raw.starts_with('£') || raw.starts_with('¥') {
        return true;
    }
    if start > 0 {
        let prev = full[..start].chars().next_back().unwrap_or(' ');
        if matches!(prev, '$' | '€' | '£' | '¥') {
            return true;
        }
    }
    // Phone numbers starting with `+`.
    if raw.starts_with('+') || clean.starts_with('+') {
        return true;
    }
    // URLs, emails, decimals, commas, times, dates.
    if clean.contains("://")
        || clean.contains('@')
        || clean.contains('.')
        || clean.contains(',')
        || clean.contains(':')
        || clean.contains('/')
    {
        return true;
    }
    false
}

fn valid_digit_code(clean: &str) -> Option<String> {
    if clean.bytes().all(|b| b.is_ascii_digit()) {
        return (4..=8).contains(&clean.len()).then(|| clean.to_owned());
    }
    // Hyphen-separated digits like `123-456` or `1234-5678` (reject phone-like `555-0199`?
    // Only 3-3 or 4-4 symmetric pairs are standard OTP formats).
    if let Some((a, b)) = clean.split_once('-') {
        if !b.contains('-')
            && a.bytes().all(|c| c.is_ascii_digit())
            && b.bytes().all(|c| c.is_ascii_digit())
            && ((a.len() == 3 && b.len() == 3) || (a.len() == 4 && b.len() == 4))
        {
            return Some(format!("{a}{b}"));
        }
    }
    None
}

fn valid_alnum_code(clean: &str) -> Option<String> {
    // Short uppercase-ish alphanumeric token (5–8 chars) containing at least one ASCII digit
    // and one ASCII letter (e.g. `K7X9P2`, `A4B8C2`). Reject lowercase words or mixed-case
    // normal English words with a trailing digit.
    if !(5..=8).contains(&clean.len()) {
        return None;
    }
    if !clean.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()) {
        return None;
    }
    let has_digit = clean.bytes().any(|b| b.is_ascii_digit());
    let has_alpha = clean.bytes().any(|b| b.is_ascii_uppercase());
    (has_digit && has_alpha).then(|| clean.to_owned())
}

fn is_plausible_year(digits: &str) -> bool {
    matches!(digits.parse::<u16>(), Ok(1900..=2099))
}

#[cfg(test)]
mod tests {
    use super::extract_code;

    #[test]
    fn detects_real_world_otp_shapes() {
        assert_eq!(
            extract_code("Your verification code is 482910."),
            Some("482910".into())
        );
        assert_eq!(
            extract_code("G-593821 is your Google verification code."),
            Some("593821".into())
        );
        assert_eq!(
            extract_code("Use 1234 as your login PIN."),
            Some("1234".into())
        );
        assert_eq!(
            extract_code("Your one-time passcode: 9182-7364. Expires in 5 minutes."),
            Some("91827364".into())
        );
        assert_eq!(
            extract_code("Your security code is 482 910"),
            Some("482910".into())
        );
        assert_eq!(
            extract_code("Your 2FA code is 482-910"),
            Some("482910".into())
        );
        assert_eq!(
            extract_code("Enter OTP K7X9P2 to sign in."),
            Some("K7X9P2".into())
        );
        // WebOTP format
        assert_eq!(
            extract_code("Your Acme code.\n\n@acme.example.com #741852"),
            Some("741852".into())
        );
        // Other supported languages
        assert_eq!(
            extract_code("Tu código de verificación es 654321."),
            Some("654321".into())
        );
        assert_eq!(
            extract_code("Votre code de vérification : 839201"),
            Some("839201".into())
        );
        assert_eq!(
            extract_code("Ihr Bestätigungscode lautet 294810."),
            Some("294810".into())
        );
        assert_eq!(
            extract_code("Il tuo codice di verifica è 102938."),
            Some("102938".into())
        );
    }

    #[test]
    fn ignores_false_positives() {
        assert_eq!(extract_code("Your order #482910 has shipped!"), None);
        assert_eq!(extract_code("You spent $1234 at Coffee Shop."), None);
        assert_eq!(extract_code("Call me at +15551234567 tomorrow."), None);
        assert_eq!(extract_code("Meeting on 2026-11-21 at 14:30."), None);
        assert_eq!(extract_code("Are we still on for Tuesday?"), None);
        assert_eq!(
            extract_code("Your verification fee is $1234.56 on 11/21/2026."),
            None
        );
    }
}
