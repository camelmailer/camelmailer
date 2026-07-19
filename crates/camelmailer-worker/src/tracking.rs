//! Open/click tracking rewrite — the port of the tracking pass in
//! `app/lib/postal/message_db` that rewrites the HTML part of an outgoing
//! message. Each `http(s)` link in an HTML body is registered and rewritten
//! to a `/track/c/<token>` redirect, and an invisible `/track/o/<token>.gif`
//! pixel is appended before `</body>`.
//!
//! This operates on the message's HTML part only. Finding it in a full MIME
//! message is a larger job; here we rewrite an HTML body (single-part
//! `text/html` messages and the common inline case), which is enough to
//! exercise and verify the machinery end to end.

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

fn is_html(headers: &str) -> bool {
    headers.to_lowercase().contains("text/html")
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

/// Split a raw message into (headers, body_html) when the body is HTML.
pub fn html_body(raw_message: &[u8]) -> Option<(String, String)> {
    let offset = body_offset(raw_message)?;
    let headers = String::from_utf8_lossy(&raw_message[..offset]).to_string();
    if !is_html(&headers) {
        return None;
    }
    let body = String::from_utf8_lossy(&raw_message[offset..]).to_string();
    Some((headers, body))
}

/// Reassemble a raw message from its header block and a new HTML body.
pub fn reassemble(headers: &str, new_body: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(headers.len() + new_body.len());
    out.extend_from_slice(headers.as_bytes());
    out.extend_from_slice(new_body.as_bytes());
    out
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
    fn html_body_only_for_html_messages() {
        let html = b"Content-Type: text/html\r\n\r\n<body>x</body>";
        let (headers, body) = html_body(html).unwrap();
        assert!(headers.contains("text/html"));
        assert_eq!(body, "<body>x</body>");

        let text = b"Content-Type: text/plain\r\n\r\nplain";
        assert!(html_body(text).is_none());
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

    #[test]
    fn reassemble_round_trips() {
        let raw = b"Content-Type: text/html\r\n\r\n<body>x</body>";
        let (headers, body) = html_body(raw).unwrap();
        let rebuilt = reassemble(&headers, &body);
        assert_eq!(rebuilt, raw);
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

        // Not an HTML top level, so the tracking pass never rewrites it: the
        // worker returns the raw message byte-for-byte (no line-ending rewrite,
        // no dropped bytes). Regression guard for the mangled-attachment bug.
        assert!(html_body(&raw).is_none());
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
    // non-ASCII UTF-8 round-trips through split → (no-op) rewrite → reassemble
    // with every byte intact.
    #[test]
    fn html_body_round_trips_mixed_endings_and_utf8() {
        let raw = "Content-Type: text/html; charset=utf-8\r\n\r\n\
<p>Grüße\nüber\r\nZeilen — ohne Verlust</p>"
            .as_bytes();
        let (headers, body) = html_body(raw).unwrap();
        let (rewritten, urls) = rewrite_links(&body, |u| u.to_string());
        assert!(urls.is_empty());
        let rebuilt = reassemble(&headers, &rewritten);
        assert_eq!(rebuilt, raw, "no bytes may be dropped or altered");
    }
}
