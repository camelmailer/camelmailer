//! Open/click tracking rewrite — the port of the tracking pass in
//! `app/lib/postal/message_db` that rewrites the HTML part of an outgoing
//! message. Each `http(s)` link in an HTML body is registered and rewritten
//! to a `/track/c/<token>` redirect, and an invisible `/track/o/<token>.gif`
//! pixel is appended before `</body>`.
//!
//! The rewrite works on the *decoded* HTML of the message's first
//! `text/html` part, found by walking the MIME tree (`html_of`), and the
//! result is encoded again with that part's own Content-Transfer-Encoding
//! before it goes back in its place (`with_html`). Rewriting the encoded
//! bytes instead put a raw `<img src="…">` into quoted-printable text, where
//! every `=` starts an escape: mail clients showed a broken tag at the end of
//! each tracked mail, and `href=3D"…"` links were never rewritten at all.
//! Everything outside that one part — other parts, attachments, line
//! endings — is left byte-for-byte as it was.

/// Returns the byte offset of the body separator (the empty line) so callers
/// can split headers from body.
fn body_offset(raw_message: &[u8]) -> Option<usize> {
    // find CRLFCRLF or LFLF
    raw_message
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|p| p + 4)
        .or_else(|| {
            raw_message
                .windows(2)
                .position(|w| w == b"\n\n")
                .map(|p| p + 2)
        })
}

/// True when the message carries a cryptographic signature that a tracking
/// rewrite would invalidate, so tracking must leave it untouched:
///
/// * a top-level `multipart/signed` container — the S/MIME and PGP/MIME
///   detached-signature form (the signature covers the exact bytes of the
///   signed part), or
/// * an inline PGP clearsigned body (`-----BEGIN PGP SIGNED MESSAGE-----`),
///   whose signature covers the body text a rewrite would change.
///
/// Deliberately narrow: it only inspects the top-level Content-Type header and
/// the body for well-known signature markers, so a normal tracked HTML mail is
/// never misclassified.
pub fn is_signed(raw_message: &[u8]) -> bool {
    let offset = body_offset(raw_message).unwrap_or(raw_message.len());
    let headers = String::from_utf8_lossy(&raw_message[..offset]).to_lowercase();
    // A top-level multipart/signed container: the signature is detached and
    // covers the signed part verbatim, so any rewrite breaks verification.
    if header_is_multipart_signed(&headers) {
        return true;
    }
    // Inline PGP clearsigned content (RFC 4880). The markers are ASCII, so a
    // lossy decode is exact for detection purposes.
    let body = String::from_utf8_lossy(&raw_message[offset..]);
    body.contains("-----BEGIN PGP SIGNED MESSAGE-----")
        || body.contains("-----BEGIN PGP SIGNATURE-----")
}

/// True when the top-level `Content-Type` is `multipart/signed`. Matching the
/// header value (not the whole header block) avoids a false positive from the
/// literal string appearing elsewhere.
fn header_is_multipart_signed(lowercased_headers: &str) -> bool {
    lowercased_headers
        .lines()
        .any(|line| line.starts_with("content-type:") && line.contains("multipart/signed"))
}

/// Rewrite HTTP(S) `href="…"` links using the provided registrar, which
/// returns the tracking URL for an original URL. Returns the rewritten HTML
/// and the list of (original_url) that were rewritten, in order.
pub fn rewrite_links<F: FnMut(&str) -> String>(
    html: &str,
    mut make_click_url: F,
) -> (String, Vec<String>) {
    let mut output = String::with_capacity(html.len());
    let mut rewritten = Vec::new();
    let bytes = html.as_bytes();
    let mut index = 0;

    while index < bytes.len() {
        // look for `href="` (case-insensitive), single or double quoted
        if let Some((attr_end, quote)) = match_href(&html[index..]) {
            let value_start = index + attr_end;
            if let Some(close) = html[value_start..].find(quote) {
                let raw_url = &html[value_start..value_start + close];
                output.push_str(&html[index..value_start]);
                // The href as written in HTML may carry HTML entities
                // (`&amp;`, `&#38;`, …). Decode them to recover the *real*
                // URL to store and redirect to, so the click endpoint hands
                // back the exact link the sender intended (`a=1&b=2`), not a
                // literal `a=1&amp;b=2`. Percent-encoding (`%20`) is part of
                // the URL itself and is deliberately left untouched.
                let decoded = decode_html_entities(raw_url);
                if decoded.starts_with("http://") || decoded.starts_with("https://") {
                    output.push_str(&make_click_url(&decoded));
                    rewritten.push(decoded);
                } else {
                    output.push_str(raw_url);
                }
                index = value_start + close;
                continue;
            }
        }
        // copy one char
        let ch_len = html[index..]
            .chars()
            .next()
            .map(|c| c.len_utf8())
            .unwrap_or(1);
        output.push_str(&html[index..index + ch_len]);
        index += ch_len;
    }
    (output, rewritten)
}

/// Decode the HTML character references an `href` value can legitimately
/// carry (`&amp;`, `&lt;`, `&gt;`, `&quot;`, `&#39;`, and numeric `&#NN;` /
/// `&#xHH;`) back into the characters they stand for. This recovers the true
/// URL that a browser would navigate to. It intentionally does **not** touch
/// percent-encoding (`%20` stays `%20`): that is part of the URL, not an HTML
/// entity. Unknown or malformed references are left verbatim.
fn decode_html_entities(input: &str) -> String {
    if !input.contains('&') {
        return input.to_string();
    }
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'&' {
            if let Some(semicolon) = input[index..].find(';') {
                let entity = &input[index + 1..index + semicolon];
                if let Some(decoded) = decode_one_entity(entity) {
                    out.push(decoded);
                    index += semicolon + 1;
                    continue;
                }
            }
        }
        // not a recognised entity: copy one UTF-8 char verbatim
        let ch = input[index..].chars().next().unwrap();
        out.push(ch);
        index += ch.len_utf8();
    }
    out
}

/// Decode the body of a single entity (the text between `&` and `;`).
fn decode_one_entity(entity: &str) -> Option<char> {
    match entity {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => {
            let number = entity.strip_prefix('#')?;
            let code = match number.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => number.parse::<u32>().ok()?,
            };
            char::from_u32(code)
        }
    }
}

/// If `slice` starts with an `href=` attribute, return (offset of the value
/// after the opening quote, the quote character).
fn match_href(slice: &str) -> Option<(usize, char)> {
    let lower = slice.to_lowercase();
    if !lower.starts_with("href") {
        return None;
    }
    let after = slice["href".len()..].trim_start();
    let consumed_ws = slice["href".len()..].len() - after.len();
    let after = after.strip_prefix('=')?;
    let after_eq = after.trim_start();
    let consumed_ws2 = after.len() - after_eq.len();
    let quote = after_eq.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    // offset from slice start to the char after the opening quote
    let offset = "href".len() + consumed_ws + 1 + consumed_ws2 + 1;
    Some((offset, quote))
}

/// Append an open-tracking pixel before `</body>` (or at the end).
pub fn inject_open_pixel(html: &str, pixel_url: &str) -> String {
    let pixel = format!(
        "<img src=\"{pixel_url}\" alt=\"\" width=\"1\" height=\"1\" style=\"display:none\"/>"
    );
    match html.to_lowercase().rfind("</body>") {
        Some(position) => format!("{}{}{}", &html[..position], pixel, &html[position..]),
        None => format!("{html}{pixel}"),
    }
}

/// How a part's body is encoded on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Encoding {
    /// 7bit, 8bit, binary or none given: the bytes are the text.
    Identity,
    QuotedPrintable,
    Base64,
}

/// Where the HTML part's body sits in the raw message, and how it is encoded.
#[derive(Debug, Clone, Copy)]
struct HtmlPart {
    start: usize,
    end: usize,
    encoding: Encoding,
}

const MAX_DEPTH: usize = 8;

/// The value of a header in a parsed header list (names are lower-case).
fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

/// A parameter of a structured header value (`boundary`, `charset`).
fn parameter(value: &str, name: &str) -> Option<String> {
    value.split(';').skip(1).find_map(|part| {
        let (key, raw) = part.trim().split_once('=')?;
        key.trim()
            .eq_ignore_ascii_case(name)
            .then(|| raw.trim().trim_matches('"').to_string())
    })
}

/// Byte ranges of the parts of a multipart body, between its delimiters.
/// The line break before a delimiter belongs to the delimiter (RFC 2046).
fn multipart_ranges(body: &[u8], boundary: &str) -> Vec<(usize, usize)> {
    let delimiter = format!("--{boundary}");
    let delimiter = delimiter.as_bytes();
    // Delimiter lines: at the start of the body or after a line break.
    let mut lines: Vec<(usize, usize, bool)> = Vec::new(); // (line start, after line, closing)
    let mut at = 0;
    while at + delimiter.len() <= body.len() {
        let line_start = at == 0 || body[at - 1] == b'\n';
        if line_start && body[at..].starts_with(delimiter) {
            let rest = &body[at + delimiter.len()..];
            let closing = rest.starts_with(b"--");
            let line_end = rest
                .iter()
                .position(|&b| b == b'\n')
                .map(|p| at + delimiter.len() + p + 1)
                .unwrap_or(body.len());
            lines.push((at, line_end, closing));
            if closing {
                break;
            }
            at = line_end;
            continue;
        }
        at += 1;
    }

    lines
        .windows(2)
        .map(|pair| {
            let start = pair[0].1;
            let mut end = pair[1].0;
            if end > start && body[end - 1] == b'\n' {
                end -= 1;
                if end > start && body[end - 1] == b'\r' {
                    end -= 1;
                }
            }
            (start, end.max(start))
        })
        .collect()
}

/// The first `text/html` part that is not an attachment, searched depth-first.
/// A charset other than UTF-8 (or its ASCII subset) is left alone: rewriting
/// it as UTF-8 would garble it.
fn find_html(raw: &[u8], base: usize, depth: usize) -> Option<HtmlPart> {
    if depth > MAX_DEPTH {
        return None;
    }
    let headers = camelmailer_core::message::parse_headers(raw);
    let content_type = header(&headers, "content-type").unwrap_or("text/plain");
    let offset = body_offset(raw)?;
    let body = &raw[offset..];
    let media = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();

    if media.starts_with("multipart/") {
        // A signed container is never touched (see `is_signed`).
        if media == "multipart/signed" {
            return None;
        }
        let boundary = parameter(content_type, "boundary")?;
        return multipart_ranges(body, &boundary)
            .into_iter()
            .find_map(|(start, end)| {
                find_html(&body[start..end], base + offset + start, depth + 1)
            });
    }

    if media != "text/html"
        || header(&headers, "content-disposition")
            .is_some_and(|d| d.trim().to_ascii_lowercase().starts_with("attachment"))
    {
        return None;
    }
    if let Some(charset) = parameter(content_type, "charset") {
        if !matches!(
            charset.to_ascii_lowercase().as_str(),
            "utf-8" | "utf8" | "us-ascii"
        ) {
            return None;
        }
    }
    let encoding = match header(&headers, "content-transfer-encoding")
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "quoted-printable" => Encoding::QuotedPrintable,
        "base64" => Encoding::Base64,
        _ => Encoding::Identity,
    };

    Some(HtmlPart {
        start: base + offset,
        end: base + raw.len(),
        encoding,
    })
}

fn decode_quoted_printable(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len());
    let mut i = 0;
    while i < body.len() {
        match body[i] {
            b'=' if body[i + 1..].starts_with(b"\r\n") => i += 3,
            b'=' if body[i + 1..].starts_with(b"\n") => i += 2,
            b'=' if i + 2 < body.len() => {
                match std::str::from_utf8(&body[i + 1..i + 3])
                    .ok()
                    .and_then(|hex| u8::from_str_radix(hex, 16).ok())
                {
                    Some(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    None => {
                        out.push(b'=');
                        i += 1;
                    }
                }
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    out
}

/// Quoted-printable (RFC 2045 §6.7): line breaks of the text stay hard line
/// breaks (CRLF), lines are soft-broken before 76 characters, and `=`,
/// non-ASCII bytes and whitespace at a line's end are escaped.
fn encode_quoted_printable(text: &[u8]) -> Vec<u8> {
    const MAX: usize = 75; // room for the soft break's "="
    let mut out = Vec::with_capacity(text.len() * 11 / 10);
    let lines: Vec<&[u8]> = text.split(|&b| b == b'\n').collect();
    for (n, line) in lines.iter().enumerate() {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let mut width = 0;
        for (i, &byte) in line.iter().enumerate() {
            let last = i + 1 == line.len();
            let literal = (byte == b' ' || byte == b'\t') && !last
                || (33..=126).contains(&byte) && byte != b'=';
            let piece: Vec<u8> = if literal {
                vec![byte]
            } else {
                format!("={byte:02X}").into_bytes()
            };
            if width + piece.len() > MAX {
                out.extend_from_slice(b"=\r\n");
                width = 0;
            }
            width += piece.len();
            out.extend_from_slice(&piece);
        }
        if n + 1 < lines.len() {
            out.extend_from_slice(b"\r\n");
        }
    }
    out
}

fn decode(encoding: Encoding, body: &[u8]) -> Option<Vec<u8>> {
    match encoding {
        Encoding::Identity => Some(body.to_vec()),
        Encoding::QuotedPrintable => Some(decode_quoted_printable(body)),
        Encoding::Base64 => {
            use base64::Engine;
            let compact: Vec<u8> = body
                .iter()
                .copied()
                .filter(|b| !b.is_ascii_whitespace())
                .collect();
            base64::engine::general_purpose::STANDARD
                .decode(compact)
                .ok()
        }
    }
}

fn encode(encoding: Encoding, text: &[u8]) -> Vec<u8> {
    match encoding {
        Encoding::Identity => text.to_vec(),
        Encoding::QuotedPrintable => encode_quoted_printable(text),
        Encoding::Base64 => {
            use base64::Engine;
            let b64 = base64::engine::general_purpose::STANDARD.encode(text);
            let lines: Vec<&str> = b64
                .as_bytes()
                .chunks(76)
                .map(|chunk| std::str::from_utf8(chunk).unwrap_or_default())
                .collect();
            lines.join("\r\n").into_bytes()
        }
    }
}

/// The decoded HTML of a message's first `text/html` part, if it has one that
/// tracking may rewrite.
pub fn html_of(raw_message: &[u8]) -> Option<String> {
    let part = find_html(raw_message, 0, 0)?;
    let bytes = decode(part.encoding, &raw_message[part.start..part.end])?;
    String::from_utf8(bytes).ok()
}

/// The message with its first `text/html` part replaced by `html`, encoded as
/// that part was. Every other byte of the message is kept as it was.
pub fn with_html(raw_message: &[u8], html: &str) -> Option<Vec<u8>> {
    let part = find_html(raw_message, 0, 0)?;
    let encoded = encode(part.encoding, html.as_bytes());
    let mut out = Vec::with_capacity(raw_message.len() + encoded.len());
    out.extend_from_slice(&raw_message[..part.start]);
    out.extend_from_slice(&encoded);
    out.extend_from_slice(&raw_message[part.end..]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_http_links_and_leaves_others() {
        let html = r#"<a href="https://example.com/a">A</a> <a href="mailto:x@y.z">M</a>"#;
        let (out, urls) = rewrite_links(html, |url| format!("TRACK[{url}]"));
        assert_eq!(urls, vec!["https://example.com/a"]);
        assert!(out.contains("href=\"TRACK[https://example.com/a]\""));
        assert!(out.contains("href=\"mailto:x@y.z\""));
    }

    #[test]
    fn rewrites_single_quoted_links() {
        let html = "<a href='http://example.com'>x</a>";
        let (out, urls) = rewrite_links(html, |_| "T".to_string());
        assert_eq!(urls.len(), 1);
        assert!(out.contains("href='T'"));
    }

    #[test]
    fn injects_pixel_before_body_close() {
        let out = inject_open_pixel("<html><body>hi</body></html>", "http://t/o.gif");
        assert!(out.contains("hi<img src=\"http://t/o.gif\""));
        assert!(out.ends_with("</body></html>"));
    }

    #[test]
    fn injects_pixel_at_end_without_body_tag() {
        let out = inject_open_pixel("hi", "PIX");
        assert!(out.ends_with("style=\"display:none\"/>"));
        assert!(out.starts_with("hi<img"));
    }

    #[test]
    fn detects_signed_messages() {
        // top-level multipart/signed (S/MIME or PGP/MIME)
        let smime = b"Content-Type: multipart/signed; protocol=\"application/pkcs7-signature\"; \
            micalg=sha-256; boundary=\"b\"\r\n\r\n--b\r\nContent-Type: text/html\r\n\r\n\
            <body><a href=\"https://x.example\">x</a></body>\r\n--b--\r\n";
        assert!(is_signed(smime));

        // inline PGP clearsigned HTML body
        let pgp = b"Content-Type: text/html\r\n\r\n-----BEGIN PGP SIGNED MESSAGE-----\r\n\
            Hash: SHA256\r\n\r\n<body><a href=\"https://x.example\">x</a></body>\r\n\
            -----BEGIN PGP SIGNATURE-----\r\n...\r\n-----END PGP SIGNATURE-----\r\n";
        assert!(is_signed(pgp));

        // ordinary HTML mail is NOT flagged
        let plain =
            b"Content-Type: text/html\r\n\r\n<body><a href=\"https://x.example\">x</a></body>";
        assert!(!is_signed(plain));

        // the literal string in body text must not trip the header check
        let mentions = b"Content-Type: text/html\r\n\r\n<body>we use multipart/signed mail</body>";
        assert!(!is_signed(mentions));
    }

    // Item 4: a long href carrying HTML entities and percent-encoding must
    // round-trip so the click endpoint redirects to the *exact* original URL.
    // `&amp;` decodes to `&`; `%20` (real URL bytes) is preserved verbatim.
    #[test]
    fn long_urls_with_entities_round_trip_to_the_original() {
        let long_query: String = (0..40).map(|i| format!("k{i}=v{i}&amp;")).collect();
        let original_href =
            format!("https://example.com/path%20with%20space/page?{long_query}end=1&amp;x=%2F");
        let html = format!("<a href=\"{original_href}\">link</a>");

        let mut stored: Vec<String> = Vec::new();
        let (out, urls) = rewrite_links(&html, |url| {
            stored.push(url.to_string());
            "TRACK".to_string()
        });

        // What we store (and hand to the redirect) is the decoded, true URL.
        let expected: String = original_href.replace("&amp;", "&");
        assert_eq!(stored, vec![expected.clone()]);
        assert_eq!(urls, vec![expected.clone()]);
        // Long URLs are never truncated (storage columns are unbounded TEXT).
        assert!(expected.len() > 255);
        // Percent-encoding is preserved exactly; no HTML entities remain.
        assert!(expected.contains("%20with%20space"));
        assert!(expected.contains("x=%2F"));
        assert!(!expected.contains("&amp;"));
        assert!(expected.contains("end=1&x=%2F"));
        // The href in the outgoing HTML is replaced by the tracking URL.
        assert!(out.contains("href=\"TRACK\""));
    }

    #[test]
    fn decode_html_entities_leaves_percent_encoding_and_unknown_refs() {
        assert_eq!(decode_html_entities("a=1&amp;b=2"), "a=1&b=2");
        assert_eq!(decode_html_entities("x%20y"), "x%20y");
        assert_eq!(decode_html_entities("a&#38;b&#x26;c"), "a&b&c");
        // A stray ampersand that is not an entity is preserved.
        assert_eq!(decode_html_entities("a & b"), "a & b");
        assert_eq!(decode_html_entities("no entities"), "no entities");
    }

    // Item 3 (body fidelity): a MIME message whose top level is not text/html
    // — the common case of a multipart message carrying a base64 attachment
    // with bare LFs and mixed CRLF/LF line endings — must pass through the
    // tracking pass byte-for-byte. `html_body` returns None for it, so the
    // worker returns the raw message unchanged: no line-ending rewrite, no
    // dropped bytes, no mangled base64. Regression guard.
    #[test]
    fn multipart_with_bare_lf_attachment_is_left_untouched() {
        use base64::Engine;
        // A binary attachment payload, base64-encoded as a single stream and
        // then split with a BARE LF (not CRLF) mid-way — the exact shape that
        // upstream mangled (bare LF → CRLF, dropped bytes).
        let payload: &[u8] = b"\x00\x01\x02 bare-LF attachment bytes, preserved verbatim \xfe\xff";
        let b64 = base64::engine::general_purpose::STANDARD.encode(payload);
        let split = b64.len() / 2;
        let attachment_body = format!("{}\n{}", &b64[..split], &b64[split..]);

        let raw = format!(
            "From: sender@acme.com\r\n\
Content-Type: multipart/mixed; boundary=\"b0\"\r\n\
\r\n\
--b0\r\n\
Content-Type: text/plain\r\n\
\r\n\
hello\nworld\r\n\
--b0\r\n\
Content-Type: application/octet-stream\r\n\
Content-Transfer-Encoding: base64\r\n\
Content-Disposition: attachment; filename=\"a.bin\"\r\n\
\r\n\
{attachment_body}\r\n\
--b0--"
        );
        let raw = raw.into_bytes();

        // No HTML part, so the tracking pass never rewrites it: the worker
        // returns the raw message byte-for-byte (no line-ending rewrite, no
        // dropped bytes). Regression guard for the mangled-attachment bug.
        assert!(html_of(&raw).is_none());
        assert!(with_html(&raw, "<p>x</p>").is_none());
        assert!(!is_signed(&raw));

        // And the (untouched) base64 still decodes to the exact payload — the
        // bare LF between chunks is pure whitespace to a base64 decoder.
        let compact: String = attachment_body
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(compact.as_bytes())
            .unwrap();
        assert_eq!(decoded, payload);
    }

    // Item 3: a single-part text/html body with mixed line endings and
    // non-ASCII UTF-8 round-trips through decode → (no-op) rewrite → encode
    // with every byte intact.
    #[test]
    fn html_body_round_trips_mixed_endings_and_utf8() {
        let raw = "Content-Type: text/html; charset=utf-8\r\n\r\n\
<p>Grüße\nüber\r\nZeilen — ohne Verlust</p>"
            .as_bytes();
        let html = html_of(raw).unwrap();
        let (rewritten, urls) = rewrite_links(&html, |u| u.to_string());
        assert!(urls.is_empty());
        let rebuilt = with_html(raw, &rewritten).unwrap();
        assert_eq!(rebuilt, raw, "no bytes may be dropped or altered");
    }

    /// What a tracked message reads as once a client has decoded it.
    fn displayed_html(raw: &[u8]) -> String {
        camelmailer_core::mime::extract_bodies(raw).html.unwrap()
    }

    /// The message the API builds for an HTML body with umlauts and long
    /// lines: quoted-printable, as `mail-builder` encodes it.
    fn built_message(html: &str, text: Option<&str>) -> Vec<u8> {
        camelmailer_core::mime::build_message(&camelmailer_core::mime::BuildParams {
            from: camelmailer_core::mime::Address::new("sender@example.com"),
            to: vec![camelmailer_core::mime::Address::new("kunde@example.com")],
            subject: "Ihr Raum".into(),
            html_body: Some(html.into()),
            text_body: text.map(str::to_string),
            ..Default::default()
        })
    }

    // The bug this module had: a raw pixel and untouched hrefs inside
    // quoted-printable text. The client saw `<img src="" alt="width=&quot;"
    // height=" style=isplay:none">` and no link was ever tracked.
    #[test]
    fn tracks_a_quoted_printable_body_and_keeps_it_quoted_printable() {
        let link = "https://app.example.com/r/abc?invite=xyz&amp;utm=mail";
        let html = format!(
            "<html><body><p>Guten Tag Frau Müller, über diesen Link öffnen Sie Ihren Raum, der für Sie vorbereitet wurde:</p>\n<a href=\"{link}\">Raum öffnen</a></body></html>"
        );
        let raw = built_message(&html, None);
        let raw_text = String::from_utf8_lossy(&raw).to_lowercase();
        assert!(raw_text.contains("content-transfer-encoding: quoted-printable"));

        // Quoted-printable carries line breaks as CRLF.
        let decoded = html_of(&raw).unwrap();
        assert_eq!(decoded, html.replace('\n', "\r\n"));
        let (rewritten, urls) =
            rewrite_links(&decoded, |_| "https://track.example/track/c/TOKEN".into());
        assert_eq!(
            urls,
            vec!["https://app.example.com/r/abc?invite=xyz&utm=mail"]
        );
        let rewritten = inject_open_pixel(&rewritten, "https://track.example/track/o/OPEN.gif");
        let tracked = with_html(&raw, &rewritten).unwrap();

        let shown = displayed_html(&tracked);
        assert!(shown.contains(r#"<a href="https://track.example/track/c/TOKEN">Raum öffnen</a>"#));
        assert!(shown.contains(r#"<img src="https://track.example/track/o/OPEN.gif" alt="" width="1" height="1" style="display:none"/></body>"#));
        assert!(shown.contains("Frau Müller"));

        // Still valid quoted-printable: every `=` is an escape or a soft
        // break, and no line is longer than 76 characters.
        let body = &tracked[tracked.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4..];
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body
            .replace("=\r\n", "")
            .contains("src=3D\"https://track.example"));
        assert!(body.lines().all(|line| line.len() <= 76), "{body}");
    }

    #[test]
    fn rewrites_only_the_html_part_of_a_multipart_message() {
        use base64::Engine;
        let attachment = base64::engine::general_purpose::STANDARD.encode([0u8, 1, 2, 3, 254, 255]);
        let raw = format!(
            "From: sender@example.com\r\n\
Content-Type: multipart/mixed; boundary=\"outer\"\r\n\
\r\n\
--outer\r\n\
Content-Type: multipart/alternative; boundary=inner\r\n\
\r\n\
--inner\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Grüße, https://example.com\r\n\
--inner\r\n\
Content-Type: text/html; charset=\"utf-8\"\r\n\
Content-Transfer-Encoding: quoted-printable\r\n\
\r\n\
<p>Gr=C3=BC=C3=9Fe <a href=3D\"https://example.com\">x</a></p>\r\n\
--inner--\r\n\
--outer\r\n\
Content-Type: application/pdf\r\n\
Content-Transfer-Encoding: base64\r\n\
Content-Disposition: attachment; filename=\"a.pdf\"\r\n\
\r\n\
{attachment}\r\n\
--outer--\r\n"
        )
        .into_bytes();

        let html = html_of(&raw).unwrap();
        assert_eq!(html, "<p>Grüße <a href=\"https://example.com\">x</a></p>");
        let tracked = with_html(&raw, &inject_open_pixel(&html, "https://t/o.gif")).unwrap();
        let text = String::from_utf8(tracked.clone()).unwrap();

        assert!(displayed_html(&tracked).ends_with(r#"style="display:none"/>"#));
        // Everything around the HTML part is untouched.
        let before_html = "Grüße, https://example.com\r\n--inner\r\n";
        assert!(text.contains(before_html));
        assert!(text.ends_with(&format!("{attachment}\r\n--outer--\r\n")));
        assert!(text.contains("\r\n--inner--\r\n--outer\r\n"));
    }

    #[test]
    fn keeps_a_base64_html_part_base64() {
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode("<body>Grüße</body>");
        let raw = format!("Content-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\n{encoded}\r\n").into_bytes();

        let html = html_of(&raw).unwrap();
        assert_eq!(html, "<body>Grüße</body>\n".trim_end());
        let tracked = with_html(&raw, &inject_open_pixel(&html, "https://t/o.gif")).unwrap();
        assert!(displayed_html(&tracked).contains(r#"Grüße<img src="https://t/o.gif""#));
        assert!(String::from_utf8_lossy(&tracked).contains("Content-Transfer-Encoding: base64"));
    }

    #[test]
    fn leaves_an_html_part_in_another_charset_alone() {
        let raw = b"Content-Type: text/html; charset=iso-8859-1\r\n\r\n<body>Gr\xfc\xdfe</body>";
        assert!(html_of(raw).is_none());
        assert!(with_html(raw, "<body/>").is_none());
    }

    #[test]
    fn quoted_printable_round_trips() {
        let text =
            "a=b über\r\nzweite Zeile mit Leerzeichen am Ende \r\n".to_string() + &"x".repeat(200);
        let encoded = encode_quoted_printable(text.as_bytes());
        assert!(String::from_utf8(encoded.clone())
            .unwrap()
            .lines()
            .all(|l| l.len() <= 76));
        assert_eq!(decode_quoted_printable(&encoded), text.as_bytes());
    }
}
