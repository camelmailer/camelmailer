//! MIME message construction for the HTTP send API. Wraps `mail-builder`
//! so a JSON payload (from/to/subject/html/text/attachments/headers) becomes
//! an RFC 5322 message. DKIM signing and open/click tracking are applied
//! later by the worker at delivery time (keyed off the message's
//! authenticated domain), exactly as for SMTP-submitted mail.

use mail_builder::MessageBuilder;

/// A single recipient address with an optional display name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Address {
    pub name: Option<String>,
    pub email: String,
}

impl Address {
    pub fn new(email: impl Into<String>) -> Self {
        Self {
            name: None,
            email: email.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub filename: String,
    pub content_type: String,
    pub data: Vec<u8>,
}

/// Everything needed to build one outbound message.
#[derive(Debug, Clone, Default)]
pub struct BuildParams {
    pub from: Address,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
    pub reply_to: Vec<Address>,
    pub subject: String,
    pub html_body: Option<String>,
    pub text_body: Option<String>,
    /// Extra headers (X-*, List-*, etc.). Reserved names are ignored.
    pub headers: Vec<(String, String)>,
    pub attachments: Vec<Attachment>,
    /// Overrides the auto-generated Message-ID host part.
    pub message_id: Option<String>,
}

fn to_mail_addresses(addresses: &[Address]) -> Vec<(String, String)> {
    addresses
        .iter()
        .map(|a| (a.name.clone().unwrap_or_default(), a.email.clone()))
        .collect()
}

/// Reserved headers that the builder controls; a client-supplied value is
/// ignored so it can't forge routing/identity headers.
fn is_reserved_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "from"
            | "to"
            | "cc"
            | "bcc"
            | "reply-to"
            | "subject"
            | "date"
            | "message-id"
            | "content-type"
            | "content-transfer-encoding"
            | "mime-version"
    )
}

/// Build a complete raw MIME message (RFC 5322) as bytes.
pub fn build_message(params: &BuildParams) -> Vec<u8> {
    let mut builder = MessageBuilder::new();

    builder = builder.from((
        params.from.name.clone().unwrap_or_default(),
        params.from.email.clone(),
    ));
    if !params.to.is_empty() {
        builder = builder.to(to_mail_addresses(&params.to));
    }
    if !params.cc.is_empty() {
        builder = builder.cc(to_mail_addresses(&params.cc));
    }
    if !params.bcc.is_empty() {
        builder = builder.bcc(to_mail_addresses(&params.bcc));
    }
    if !params.reply_to.is_empty() {
        builder = builder.reply_to(to_mail_addresses(&params.reply_to));
    }
    builder = builder.subject(params.subject.clone());
    if let Some(message_id) = &params.message_id {
        builder = builder.message_id(message_id.clone());
    }

    for (name, value) in &params.headers {
        if !is_reserved_header(name) {
            builder = builder.header(
                name.clone(),
                mail_builder::headers::raw::Raw::new(value.clone()),
            );
        }
    }

    match (&params.html_body, &params.text_body) {
        (Some(html), Some(text)) => {
            builder = builder.html_body(html.clone()).text_body(text.clone());
        }
        (Some(html), None) => {
            builder = builder.html_body(html.clone());
        }
        (None, Some(text)) => {
            builder = builder.text_body(text.clone());
        }
        (None, None) => {
            builder = builder.text_body(String::new());
        }
    }

    for attachment in &params.attachments {
        builder = builder.attachment(
            attachment.content_type.clone(),
            attachment.filename.clone(),
            attachment.data.clone(),
        );
    }

    builder.write_to_vec().unwrap_or_default()
}

// ------------------------------------------------------- body extraction

/// The decoded display bodies of a stored raw message — what the shared
/// message page and the deliverability insights operate on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtractedBodies {
    pub html: Option<String>,
    pub text: Option<String>,
}

/// Offset of the first byte after the header block, if any.
fn body_offset(raw: &[u8]) -> Option<usize> {
    raw.windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| i + 4)
        .or_else(|| raw.windows(2).position(|w| w == b"\n\n").map(|i| i + 2))
}

/// The boundary parameter of a multipart content-type, if any.
fn multipart_boundary(content_type: &str) -> Option<String> {
    if !content_type.trim().to_lowercase().starts_with("multipart/") {
        return None;
    }
    content_type.split(';').find_map(|part| {
        let part = part.trim();
        let value = part
            .strip_prefix("boundary=")
            .or_else(|| part.strip_prefix("Boundary="))?;
        Some(value.trim_matches('"').to_string())
    })
}

/// Decode a quoted-printable body (soft breaks and =XX escapes).
fn decode_quoted_printable(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len());
    let mut i = 0;
    while i < body.len() {
        match body[i] {
            b'=' if body.get(i + 1) == Some(&b'\r') && body.get(i + 2) == Some(&b'\n') => i += 3,
            b'=' if body.get(i + 1) == Some(&b'\n') => i += 2,
            b'=' if i + 2 < body.len() => {
                let hex = std::str::from_utf8(&body[i + 1..i + 3])
                    .ok()
                    .and_then(|s| u8::from_str_radix(s, 16).ok());
                match hex {
                    Some(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    None => {
                        out.push(body[i]);
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

/// Decode a part's body per its Content-Transfer-Encoding header.
fn decode_body(encoding: Option<&str>, body: &[u8]) -> Vec<u8> {
    match encoding.unwrap_or("").trim().to_lowercase().as_str() {
        "base64" => {
            use base64::Engine;
            let compact: Vec<u8> = body
                .iter()
                .copied()
                .filter(|b| !b" \t\r\n".contains(b))
                .collect();
            base64::engine::general_purpose::STANDARD
                .decode(&compact)
                .unwrap_or_else(|_| body.to_vec())
        }
        "quoted-printable" => decode_quoted_printable(body),
        _ => body.to_vec(),
    }
}

fn header_of<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

fn walk_parts(raw: &[u8], depth: usize, bodies: &mut ExtractedBodies) {
    if depth > 8 || (bodies.html.is_some() && bodies.text.is_some()) {
        return;
    }
    let headers = crate::message::parse_headers(raw);
    let content_type = header_of(&headers, "content-type").unwrap_or("text/plain");
    let body = body_offset(raw).map(|offset| &raw[offset..]).unwrap_or(b"");

    if let Some(boundary) = multipart_boundary(content_type) {
        let delimiter = format!("--{boundary}");
        let text = String::from_utf8_lossy(body);
        // pieces[0] is the preamble; the final piece follows `--<boundary>--`
        let pieces: Vec<&str> = text.split(delimiter.as_str()).collect();
        for piece in pieces.iter().skip(1) {
            if piece.starts_with("--") {
                break;
            }
            let part = piece
                .strip_prefix("\r\n")
                .or_else(|| piece.strip_prefix("\n"))
                .unwrap_or(piece);
            // the CRLF preceding the next delimiter belongs to the delimiter
            let part = part
                .strip_suffix("\r\n")
                .or_else(|| part.strip_suffix("\n"))
                .unwrap_or(part);
            walk_parts(part.as_bytes(), depth + 1, bodies);
        }
        return;
    }

    let media_type = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_lowercase();
    // Attachments are never display bodies.
    if header_of(&headers, "content-disposition")
        .unwrap_or("")
        .trim()
        .to_lowercase()
        .starts_with("attachment")
    {
        return;
    }
    let decoded = || -> Option<String> {
        let bytes = decode_body(header_of(&headers, "content-transfer-encoding"), body);
        if bytes.is_empty() {
            return None;
        }
        Some(String::from_utf8_lossy(&bytes).into_owned())
    };
    match media_type.as_str() {
        "text/html" if bodies.html.is_none() => {
            bodies.html = decoded();
        }
        "text/plain" | "" if bodies.text.is_none() => {
            bodies.text = decoded();
        }
        _ => {}
    }
}

/// Extract the decoded HTML and plain-text display bodies of a raw MIME
/// message (walking multipart containers; attachments are skipped, the
/// first part of each kind wins). Charset is treated as UTF-8 (lossy).
pub fn extract_bodies(raw_message: &[u8]) -> ExtractedBodies {
    let mut bodies = ExtractedBodies::default();
    walk_parts(raw_message, 0, &mut bodies);
    bodies
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> BuildParams {
        BuildParams {
            from: Address {
                name: Some("Sender".into()),
                email: "sender@org.example".into(),
            },
            to: vec![Address::new("rcpt@dest.example")],
            subject: "Hello".into(),
            html_body: Some("<p>Hi</p>".into()),
            text_body: Some("Hi".into()),
            ..Default::default()
        }
    }

    fn as_string(bytes: &[u8]) -> String {
        String::from_utf8_lossy(bytes).into_owned()
    }

    #[test]
    fn builds_a_multipart_alternative_with_both_bodies() {
        let raw = as_string(&build_message(&params()));
        assert!(raw.contains("From: "));
        assert!(raw.contains("sender@org.example"));
        assert!(raw.contains("To: "));
        assert!(raw.contains("rcpt@dest.example"));
        assert!(raw.contains("Subject: Hello"));
        assert!(raw.to_lowercase().contains("multipart/alternative"));
        assert!(raw.contains("Hi"));
    }

    #[test]
    fn custom_headers_are_added_but_reserved_ones_ignored() {
        let mut p = params();
        p.headers = vec![
            ("X-Campaign".into(), "spring".into()),
            ("From".into(), "attacker@evil.example".into()),
        ];
        let raw = as_string(&build_message(&p));
        assert!(raw.contains("X-Campaign: spring"));
        assert!(!raw.contains("attacker@evil.example"));
    }

    #[test]
    fn attachments_are_encoded() {
        let mut p = params();
        p.attachments = vec![Attachment {
            filename: "hello.txt".into(),
            content_type: "text/plain".into(),
            data: b"attachment-body".to_vec(),
        }];
        let raw = as_string(&build_message(&p));
        assert!(raw.to_lowercase().contains("multipart/mixed"));
        assert!(raw.contains("hello.txt"));
    }

    #[test]
    fn text_only_message_builds() {
        let mut p = params();
        p.html_body = None;
        let raw = as_string(&build_message(&p));
        assert!(raw.contains("Subject: Hello"));
        assert!(raw.contains("Hi"));
    }

    // ------------------------------------------------- extract_bodies

    #[test]
    fn extracts_both_bodies_from_multipart_alternative() {
        let raw = build_message(&params());
        let bodies = extract_bodies(&raw);
        assert_eq!(bodies.html.as_deref(), Some("<p>Hi</p>"));
        assert_eq!(bodies.text.as_deref(), Some("Hi"));
    }

    #[test]
    fn extracts_single_part_messages() {
        let mut p = params();
        p.html_body = None;
        let bodies = extract_bodies(&build_message(&p));
        assert_eq!(bodies.text.as_deref(), Some("Hi"));
        assert_eq!(bodies.html, None);

        let mut p = params();
        p.text_body = None;
        let bodies = extract_bodies(&build_message(&p));
        assert_eq!(bodies.html.as_deref(), Some("<p>Hi</p>"));
        assert_eq!(bodies.text, None);
    }

    #[test]
    fn walks_nested_multipart_and_skips_attachments() {
        let mut p = params();
        p.attachments = vec![Attachment {
            filename: "hello.txt".into(),
            content_type: "text/plain".into(),
            data: b"attachment-body".to_vec(),
        }];
        // multipart/mixed( multipart/alternative(text, html), attachment )
        let bodies = extract_bodies(&build_message(&p));
        assert_eq!(bodies.html.as_deref(), Some("<p>Hi</p>"));
        assert_eq!(bodies.text.as_deref(), Some("Hi"));
        // the text/plain attachment must not shadow the real text body
        assert!(!bodies.text.unwrap().contains("attachment-body"));
    }

    #[test]
    fn decodes_quoted_printable_and_base64_bodies() {
        let qp = b"Content-Type: text/plain\r\n\
                   Content-Transfer-Encoding: quoted-printable\r\n\
                   \r\n\
                   Gr=C3=BC=C3=9Fe=\r\n\
                   !\r\n";
        assert_eq!(
            extract_bodies(qp).text.as_deref(),
            Some("Gr\u{fc}\u{df}e!\r\n")
        );

        let b64 = b"Content-Type: text/html\r\n\
                    Content-Transfer-Encoding: base64\r\n\
                    \r\n\
                    PGI+aGk8L2I+\r\n";
        assert_eq!(extract_bodies(b64).html.as_deref(), Some("<b>hi</b>"));
    }

    #[test]
    fn headerless_or_unknown_content_is_treated_as_text() {
        let plain = b"Subject: x\r\n\r\njust text\r\n";
        assert_eq!(extract_bodies(plain).text.as_deref(), Some("just text\r\n"));
        let bodies = extract_bodies(b"");
        assert_eq!(bodies, ExtractedBodies::default());
    }
}
