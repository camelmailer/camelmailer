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
        .or_else(|| raw_message.windows(2).position(|w| w == b"\n\n").map(|p| p + 2))
}

fn is_html(headers: &str) -> bool {
    headers.to_lowercase().contains("text/html")
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
                let url = &html[value_start..value_start + close];
                output.push_str(&html[index..value_start]);
                if url.starts_with("http://") || url.starts_with("https://") {
                    output.push_str(&make_click_url(url));
                    rewritten.push(url.to_string());
                } else {
                    output.push_str(url);
                }
                index = value_start + close;
                continue;
            }
        }
        // copy one char
        let ch_len = html[index..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
        output.push_str(&html[index..index + ch_len]);
        index += ch_len;
    }
    (output, rewritten)
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
    fn reassemble_round_trips() {
        let raw = b"Content-Type: text/html\r\n\r\n<body>x</body>";
        let (headers, body) = html_body(raw).unwrap();
        let rebuilt = reassemble(&headers, &body);
        assert_eq!(rebuilt, raw);
    }
}
