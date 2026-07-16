//! ARF (Abuse Reporting Format, RFC 5965) feedback-loop parsing: pull the
//! machine-readable feedback fields and the embedded original message out of
//! an inbound spam-complaint report, so the worker can record a
//! stream-scoped complaint for the recipient who complained.
//!
//! The mapping back to a broadcast recipient rides on the original message:
//! every broadcast carries a `List-Unsubscribe: <…/track/u/{token}>` header,
//! and an ARF report embeds that original (as a `message/rfc822` or
//! `message/rfc822-headers` part), so the token — and thus the
//! (server, stream, address) it resolves to — is recoverable from the report.
//!
//! Everything here is infallible from the worker's point of view: any
//! malformed input becomes an [`ArfError`] the caller turns into a held
//! message — never a crash.

use camelmailer_core::message::{header_value, parse_headers};

/// MIME parts can nest; feedback reports never need more than this.
const MAX_MIME_DEPTH: usize = 5;

#[derive(Debug, thiserror::Error)]
pub enum ArfError {
    #[error("the message is not an ARF feedback report: {0}")]
    NotFeedbackReport(String),
    #[error("the feedback report is missing its {0} part")]
    MissingPart(&'static str),
}

/// A parsed ARF feedback report, independent of any tenant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedbackReport {
    /// The `Feedback-Type` of the machine-readable part (e.g. `abuse`),
    /// lowercased.
    pub feedback_type: String,
    /// The `/track/u/{token}` unsubscribe token recovered from the embedded
    /// original message's `List-Unsubscribe` header, if present.
    pub unsubscribe_token: Option<String>,
    /// The `Original-Rcpt-To` of the feedback part, if present.
    pub original_rcpt: Option<String>,
}

/// A decoded leaf MIME part, keeping the content-type so the feedback and
/// original-message parts can be told apart.
struct Part {
    content_type: String,
    body: Vec<u8>,
}

// -------------------------------------------------------------------- MIME

/// Decode a quoted-printable body (soft breaks and =XX escapes).
fn decode_quoted_printable(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len());
    let mut i = 0;
    while i < body.len() {
        if body[i] == b'=' {
            if body.get(i + 1) == Some(&b'\r') && body.get(i + 2) == Some(&b'\n') {
                i += 3;
                continue;
            }
            if body.get(i + 1) == Some(&b'\n') {
                i += 2;
                continue;
            }
            if i + 2 < body.len() {
                let hex = std::str::from_utf8(&body[i + 1..i + 3]).ok();
                if let Some(byte) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    out.push(byte);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(body[i]);
        i += 1;
    }
    out
}

/// The body bytes of a raw MIME entity (everything after the first blank
/// line).
fn body_of(raw: &[u8]) -> &[u8] {
    let mut i = 0;
    while i < raw.len() {
        let line_end = raw[i..]
            .iter()
            .position(|&b| b == b'\n')
            .map(|p| i + p)
            .unwrap_or(raw.len());
        let line = &raw[i..line_end];
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            return &raw[(line_end + 1).min(raw.len())..];
        }
        i = line_end + 1;
    }
    &[]
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

/// The boundary parameter of a multipart content-type, if any.
fn multipart_boundary(content_type: &str) -> Option<String> {
    if !content_type.trim().to_lowercase().starts_with("multipart/") {
        return None;
    }
    for parameter in content_type.split(';').skip(1) {
        let parameter = parameter.trim();
        if let Some(value) = parameter
            .strip_prefix("boundary=")
            .or_else(|| parameter.strip_prefix("BOUNDARY="))
            .or_else(|| parameter.strip_prefix("Boundary="))
        {
            return Some(value.trim_matches('"').to_string());
        }
    }
    None
}

/// The decoded payload of a leaf part, honouring its transfer encoding.
fn decode_leaf(headers: &[(String, String)], body: &[u8]) -> Vec<u8> {
    let encoding = header(headers, "content-transfer-encoding")
        .unwrap_or("")
        .trim()
        .to_lowercase();
    match encoding.as_str() {
        "base64" => {
            use base64::Engine;
            let compact: Vec<u8> = body
                .iter()
                .copied()
                .filter(|b| !b.is_ascii_whitespace())
                .collect();
            base64::engine::general_purpose::STANDARD
                .decode(&compact)
                .unwrap_or_else(|_| body.to_vec())
        }
        "quoted-printable" => decode_quoted_printable(body),
        _ => body.to_vec(),
    }
}

/// Collect the decoded leaf parts (depth-first), keeping each part's
/// content-type.
fn collect_parts(raw: &[u8], depth: usize, parts: &mut Vec<Part>) {
    if depth > MAX_MIME_DEPTH {
        return;
    }
    let headers = parse_headers(raw);
    let body = body_of(raw);
    let content_type = header(&headers, "content-type").unwrap_or("").to_string();

    if let Some(boundary) = multipart_boundary(&content_type) {
        let delimiter = format!("--{boundary}");
        let text = String::from_utf8_lossy(body);
        let mut pieces: Vec<&str> = text.split(&delimiter).collect();
        if pieces.len() > 1 {
            pieces.remove(0); // preamble
        }
        for piece in pieces {
            let piece = piece.strip_prefix("--").unwrap_or(piece); // closing marker
            let piece = piece.trim_start_matches(['\r', '\n']);
            if piece.trim().is_empty() {
                continue;
            }
            collect_parts(piece.as_bytes(), depth + 1, parts);
        }
        return;
    }

    parts.push(Part {
        content_type,
        body: decode_leaf(&headers, body),
    });
}

// ---------------------------------------------------------------- detection

/// Whether the raw message's top-level `Content-Type` is a
/// `multipart/report; report-type="feedback-report"` — the RFC 5965 ARF
/// envelope. Cheap enough to gate every inbound message on.
pub fn is_feedback_report(raw_message: &[u8]) -> bool {
    header_value(raw_message, "content-type")
        .map(|value| content_type_is_feedback_report(&value))
        .unwrap_or(false)
}

fn content_type_is_feedback_report(content_type: &str) -> bool {
    let lower = content_type.to_lowercase();
    lower.contains("multipart/report")
        && lower.split(';').skip(1).map(str::trim).any(|parameter| {
            parameter
                .strip_prefix("report-type=")
                .map(|value| {
                    value
                        .trim_matches('"')
                        .eq_ignore_ascii_case("feedback-report")
                })
                .unwrap_or(false)
        })
}

/// Pull the `/track/u/{token}` token out of a `List-Unsubscribe` header value
/// like `<https://track.example.com/track/u/abc123>, <mailto:…>`.
fn token_from_list_unsubscribe(value: &str) -> Option<String> {
    let marker = "/track/u/";
    let start = value.find(marker)? + marker.len();
    let token: String = value[start..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    (!token.is_empty()).then_some(token)
}

// ----------------------------------------------------------------- extract

/// Extract the feedback fields from a raw inbound ARF report: verify the
/// envelope, read `Feedback-Type` / `Original-Rcpt-To` from the
/// `message/feedback-report` part, and recover the unsubscribe token from
/// the embedded original message's `List-Unsubscribe` header.
pub fn extract_feedback(raw_message: &[u8]) -> Result<FeedbackReport, ArfError> {
    let content_type = header_value(raw_message, "content-type").unwrap_or_default();
    if !content_type_is_feedback_report(&content_type) {
        return Err(ArfError::NotFeedbackReport(format!(
            "top-level Content-Type is {content_type:?}, expected \
             multipart/report; report-type=feedback-report"
        )));
    }

    let mut parts = Vec::new();
    collect_parts(raw_message, 0, &mut parts);

    // The machine-readable part carries the feedback fields (RFC 5965 §3.1).
    let feedback = parts
        .iter()
        .find(|part| {
            part.content_type
                .to_lowercase()
                .trim_start()
                .starts_with("message/feedback-report")
        })
        .ok_or(ArfError::MissingPart("message/feedback-report"))?;
    let feedback_headers = parse_headers(&feedback.body);
    let feedback_type = header(&feedback_headers, "feedback-type")
        .unwrap_or("")
        .trim()
        .to_lowercase();
    let original_rcpt = header(&feedback_headers, "original-rcpt-to")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());

    // The embedded original message (full body or headers only) carries the
    // List-Unsubscribe header the token rides on.
    let unsubscribe_token = parts
        .iter()
        .find(|part| {
            let lower = part.content_type.to_lowercase();
            let lower = lower.trim_start();
            lower.starts_with("message/rfc822") || lower.starts_with("message/rfc822-headers")
        })
        .and_then(|part| header_value(&part.body, "list-unsubscribe"))
        .and_then(|value| token_from_list_unsubscribe(&value));

    Ok(FeedbackReport {
        feedback_type,
        unsubscribe_token,
        original_rcpt,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A realistic ARF report whose embedded original carries a broadcast
    /// `List-Unsubscribe` header (RFC 5965 Appendix B shape).
    fn sample_arf() -> Vec<u8> {
        b"From: abuse@isp.example\r\n\
          To: fbl@track.example.com\r\n\
          Subject: FW: Earn money\r\n\
          MIME-Version: 1.0\r\n\
          Content-Type: multipart/report; report-type=\"feedback-report\";\r\n\
          \tboundary=\"part-boundary\"\r\n\
          \r\n\
          --part-boundary\r\n\
          Content-Type: text/plain; charset=\"US-ASCII\"\r\n\
          \r\n\
          This is an email abuse report for an email message received\r\n\
          from IP 10.67.41.167 on Thu, 8 Mar 2005 14:00:00 EDT.\r\n\
          --part-boundary\r\n\
          Content-Type: message/feedback-report\r\n\
          \r\n\
          Feedback-Type: abuse\r\n\
          User-Agent: SomeGenerator/1.0\r\n\
          Version: 1\r\n\
          Original-Mail-From: <broadcast@sender.example.com>\r\n\
          Original-Rcpt-To: <recipient@customer.example>\r\n\
          Arrival-Date: Thu, 8 Mar 2005 14:00:00 EDT\r\n\
          Source-IP: 10.67.41.167\r\n\
          Reported-Domain: sender.example.com\r\n\
          \r\n\
          --part-boundary\r\n\
          Content-Type: message/rfc822\r\n\
          \r\n\
          From: <broadcast@sender.example.com>\r\n\
          Received: from mailserver.example.net\r\n\
          To: <recipient@customer.example>\r\n\
          Subject: Earn money\r\n\
          MIME-Version: 1.0\r\n\
          List-Unsubscribe: <http://track.example.com/track/u/abc123>, <mailto:unsubscribe@sender.example.com>\r\n\
          List-Unsubscribe-Post: List-Unsubscribe=One-Click\r\n\
          Message-ID: <8787KJKJ3K4J3K4J3K4J3.mail@sender.example.com>\r\n\
          \r\n\
          Spam Spam Spam\r\n\
          Spam Spam Spam\r\n\
          --part-boundary--\r\n"
            .to_vec()
    }

    #[test]
    fn detects_the_feedback_report_envelope() {
        assert!(is_feedback_report(&sample_arf()));
        assert!(!is_feedback_report(
            b"Content-Type: text/plain\r\n\r\njust a mail\r\n"
        ));
        // multipart/report of another report-type (a DSN) is not an ARF.
        assert!(!is_feedback_report(
            b"Content-Type: multipart/report; report-type=\"delivery-status\"\r\n\r\n"
        ));
    }

    #[test]
    fn extracts_the_feedback_type_and_unsubscribe_token() {
        let report = extract_feedback(&sample_arf()).unwrap();
        assert_eq!(report.feedback_type, "abuse");
        assert_eq!(report.unsubscribe_token.as_deref(), Some("abc123"));
        assert_eq!(
            report.original_rcpt.as_deref(),
            Some("<recipient@customer.example>")
        );
    }

    #[test]
    fn handles_a_report_whose_original_is_headers_only() {
        // message/rfc822-headers carries just the header block, still enough
        // to recover the List-Unsubscribe token.
        let message = b"MIME-Version: 1.0\r\n\
            Content-Type: multipart/report; report-type=\"feedback-report\"; boundary=\"b\"\r\n\
            \r\n\
            --b\r\n\
            Content-Type: message/feedback-report\r\n\
            \r\n\
            Feedback-Type: abuse\r\n\
            Version: 1\r\n\
            \r\n\
            --b\r\n\
            Content-Type: message/rfc822-headers\r\n\
            \r\n\
            From: <broadcast@sender.example.com>\r\n\
            List-Unsubscribe: <https://track.example.com/track/u/tok999>\r\n\
            \r\n\
            --b--\r\n"
            .to_vec();
        let report = extract_feedback(&message).unwrap();
        assert_eq!(report.feedback_type, "abuse");
        assert_eq!(report.unsubscribe_token.as_deref(), Some("tok999"));
        assert_eq!(report.original_rcpt, None);
    }

    #[test]
    fn a_report_without_a_token_still_parses() {
        let message =
            b"Content-Type: multipart/report; report-type=\"feedback-report\"; boundary=\"b\"\r\n\
            \r\n\
            --b\r\n\
            Content-Type: message/feedback-report\r\n\
            \r\n\
            Feedback-Type: abuse\r\n\
            \r\n\
            --b\r\n\
            Content-Type: message/rfc822\r\n\
            \r\n\
            From: <someone@elsewhere.example>\r\n\
            Subject: not a broadcast\r\n\
            \r\n\
            body\r\n\
            --b--\r\n"
                .to_vec();
        let report = extract_feedback(&message).unwrap();
        assert_eq!(report.feedback_type, "abuse");
        assert_eq!(report.unsubscribe_token, None);
    }

    #[test]
    fn non_feedback_reports_error_instead_of_panicking() {
        assert!(matches!(
            extract_feedback(b"Content-Type: text/plain\r\n\r\nhello"),
            Err(ArfError::NotFeedbackReport(_))
        ));
        // ARF envelope but no machine-readable part.
        let message =
            b"Content-Type: multipart/report; report-type=\"feedback-report\"; boundary=\"b\"\r\n\
            \r\n\
            --b\r\n\
            Content-Type: text/plain\r\n\
            \r\n\
            just a note\r\n\
            --b--\r\n";
        assert!(matches!(
            extract_feedback(message),
            Err(ArfError::MissingPart("message/feedback-report"))
        ));
    }

    #[test]
    fn token_extraction_stops_at_the_url_terminator() {
        assert_eq!(
            token_from_list_unsubscribe("<http://x/track/u/abc123>, <mailto:u@x>").as_deref(),
            Some("abc123")
        );
        assert_eq!(
            token_from_list_unsubscribe("<http://x/track/u/abc123/more>").as_deref(),
            Some("abc123")
        );
        assert_eq!(token_from_list_unsubscribe("<mailto:u@x>"), None);
    }
}
