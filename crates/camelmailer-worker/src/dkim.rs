//! DKIM signing (RFC 6376, rsa-sha256, relaxed/relaxed) — the port of
//! `lib/postal/dkim_header.rb`. Messages are signed at delivery time with
//! the installation signing key and the configured selector
//! (`dns.dkim_identifier`); the stored message stays unsigned, matching the
//! Ruby behaviour.

use crate::signer::Signer;
use base64::Engine;
use sha2::{Digest, Sha256};

/// Headers included in the signature when present, in this order.
const SIGNED_HEADERS: &[&str] = &[
    "from",
    "sender",
    "reply-to",
    "to",
    "cc",
    "subject",
    "date",
    "message-id",
];

/// Split a raw message into (raw header lines, body bytes). Header lines
/// keep their original name case and folding.
fn split_message(raw_message: &[u8]) -> (Vec<String>, &[u8]) {
    let mut headers: Vec<String> = Vec::new();
    let mut offset = 0;
    let mut current: Option<String> = None;
    for line in raw_message.split_inclusive(|&b| b == b'\n') {
        offset += line.len();
        let text = String::from_utf8_lossy(line);
        let trimmed = text.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if trimmed.starts_with([' ', '\t']) {
            if let Some(header) = current.as_mut() {
                header.push_str("\r\n");
                header.push_str(trimmed);
            }
        } else {
            if let Some(header) = current.take() {
                headers.push(header);
            }
            current = Some(trimmed.to_string());
        }
    }
    if let Some(header) = current.take() {
        headers.push(header);
    }
    let body = raw_message.get(offset..).unwrap_or_default();
    (headers, body)
}

/// Relaxed header canonicalization: lowercase the name, unfold, collapse
/// whitespace runs to a single space, trim.
fn canonicalize_header(raw_header: &str) -> Option<(String, String)> {
    let (name, value) = raw_header.split_once(':')?;
    let name = name.trim().to_lowercase();
    let unfolded = value.replace("\r\n", " ").replace('\n', " ");
    let mut collapsed = String::with_capacity(unfolded.len());
    let mut last_was_space = false;
    for character in unfolded.chars() {
        if character == ' ' || character == '\t' {
            if !last_was_space {
                collapsed.push(' ');
            }
            last_was_space = true;
        } else {
            collapsed.push(character);
            last_was_space = false;
        }
    }
    Some((name, collapsed.trim().to_string()))
}

/// Relaxed body canonicalization.
fn canonicalize_body(body: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(body);
    let mut lines: Vec<String> = text
        .split('\n')
        .map(|line| {
            let line = line.trim_end_matches('\r');
            // collapse interior WSP runs, strip trailing WSP
            let mut out = String::with_capacity(line.len());
            let mut last_was_space = false;
            for character in line.chars() {
                if character == ' ' || character == '\t' {
                    if !last_was_space {
                        out.push(' ');
                    }
                    last_was_space = true;
                } else {
                    out.push(character);
                    last_was_space = false;
                }
            }
            out.trim_end().to_string()
        })
        .collect();
    // remove trailing empty lines
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    if lines.is_empty() {
        return Vec::new();
    }
    let mut out = lines.join("\r\n").into_bytes();
    out.extend_from_slice(b"\r\n");
    out
}

/// Produce a complete `DKIM-Signature: ...` header line (without trailing
/// CRLF) for the message.
pub fn sign(
    raw_message: &[u8],
    domain: &str,
    selector: &str,
    signer: &Signer,
    timestamp: i64,
) -> String {
    let (raw_headers, body) = split_message(raw_message);

    // pick the signed headers in SIGNED_HEADERS order (first occurrence)
    let canonicalized: Vec<(String, String)> = raw_headers
        .iter()
        .filter_map(|raw| canonicalize_header(raw))
        .collect();
    let mut selected: Vec<(String, String)> = Vec::new();
    for name in SIGNED_HEADERS {
        if let Some(header) = canonicalized.iter().find(|(n, _)| n == name) {
            selected.push(header.clone());
        }
    }

    let body_hash =
        base64::engine::general_purpose::STANDARD.encode(Sha256::digest(canonicalize_body(body)));
    let header_names: Vec<&str> = selected.iter().map(|(n, _)| n.as_str()).collect();

    let unsigned_dkim = format!(
        "v=1; a=rsa-sha256; c=relaxed/relaxed; d={domain}; s={selector}; t={timestamp}; \
         h={}; bh={body_hash}; b=",
        header_names.join(":")
    );

    // signing input: each signed header canonicalized + CRLF, then the
    // canonicalized DKIM-Signature header itself with empty b=, no CRLF
    let mut signing_input = String::new();
    for (name, value) in &selected {
        signing_input.push_str(&format!("{name}:{value}\r\n"));
    }
    let (dkim_name, dkim_value) =
        canonicalize_header(&format!("DKIM-Signature: {unsigned_dkim}")).expect("has a colon");
    signing_input.push_str(&format!("{dkim_name}:{dkim_value}"));

    let signature = base64::engine::general_purpose::STANDARD
        .encode(signer.sign_sha256(signing_input.as_bytes()));
    format!("DKIM-Signature: {unsigned_dkim}{signature}")
}

/// Prepend a DKIM-Signature header to a raw message.
pub fn sign_and_prepend(
    raw_message: &[u8],
    domain: &str,
    selector: &str,
    signer: &Signer,
    timestamp: i64,
) -> Vec<u8> {
    let header = sign(raw_message, domain, selector, signer, timestamp);
    let mut out = Vec::with_capacity(raw_message.len() + header.len() + 2);
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(raw_message);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::signature::Verifier;

    fn test_signer() -> Signer {
        let pem = include_str!("../tests/fixtures/test_signing_key.pem");
        Signer::from_pem(pem).unwrap()
    }

    #[test]
    fn relaxed_header_canonicalization_matches_rfc_6376() {
        // RFC 6376 §3.4.5 example
        assert_eq!(canonicalize_header("A: X"), Some(("a".into(), "X".into())));
        assert_eq!(
            canonicalize_header("B : Y\t\r\n\tZ  "),
            Some(("b".into(), "Y Z".into()))
        );
    }

    #[test]
    fn relaxed_body_canonicalization_matches_rfc_6376() {
        // RFC 6376 §3.4.5 example
        let body = b" C \r\nD \t E\r\n\r\n\r\n";
        assert_eq!(canonicalize_body(body), b" C\r\nD E\r\n".to_vec());
        // empty body → empty
        assert_eq!(canonicalize_body(b"\r\n\r\n"), Vec::<u8>::new());
    }

    #[test]
    fn split_message_unfolds_and_finds_the_body() {
        let raw = b"From: a@b.c\r\nSubject: Hello\r\n world\r\n\r\nBody line\r\n";
        let (headers, body) = split_message(raw);
        assert_eq!(headers.len(), 2);
        assert_eq!(headers[1], "Subject: Hello\r\n world");
        assert_eq!(body, b"Body line\r\n");
    }

    #[test]
    fn signature_header_carries_the_expected_tags() {
        let raw =
            b"From: sender@org.example\r\nTo: rcpt@dest.example\r\nSubject: Test\r\n\r\nHello\r\n";
        let header = sign(raw, "org.example", "postal", &test_signer(), 1_700_000_000);
        assert!(header.starts_with("DKIM-Signature: v=1; a=rsa-sha256; c=relaxed/relaxed; "));
        assert!(header.contains("d=org.example;"));
        assert!(header.contains("s=postal;"));
        assert!(header.contains("t=1700000000;"));
        assert!(header.contains("h=from:to:subject;"));
        assert!(header.contains("bh="));
        assert!(header.contains("b="));
    }

    #[test]
    fn the_signature_verifies_against_the_public_key() {
        let raw = b"From: sender@org.example\r\nSubject: Verify me\r\n\r\nSome body content.\r\n";
        let signer = test_signer();
        let header = sign(raw, "org.example", "postal", &signer, 1_700_000_000);

        // reconstruct the signing input exactly as a verifier would
        let b_position = header.rfind("b=").unwrap();
        let signature_b64 = &header[b_position + 2..];
        let unsigned = &header["DKIM-Signature: ".len()..b_position + 2];

        let (raw_headers, _) = split_message(raw);
        let mut signing_input = String::new();
        for name in ["from", "subject"] {
            let header_line = raw_headers
                .iter()
                .filter_map(|h| canonicalize_header(h))
                .find(|(n, _)| n == name)
                .unwrap();
            signing_input.push_str(&format!("{}:{}\r\n", header_line.0, header_line.1));
        }
        let (n, v) = canonicalize_header(&format!("DKIM-Signature: {unsigned}")).unwrap();
        signing_input.push_str(&format!("{n}:{v}"));

        let signature_bytes = base64::engine::general_purpose::STANDARD
            .decode(signature_b64)
            .unwrap();
        let verifying_key = rsa::pkcs1v15::VerifyingKey::<Sha256>::new(signer.public_key());
        let signature = rsa::pkcs1v15::Signature::try_from(signature_bytes.as_slice()).unwrap();
        verifying_key
            .verify(signing_input.as_bytes(), &signature)
            .expect("DKIM signature must verify");
    }

    #[test]
    fn sign_and_prepend_keeps_the_original_message_intact() {
        let raw = b"From: a@org.example\r\n\r\nBody\r\n";
        let signed = sign_and_prepend(raw, "org.example", "postal", &test_signer(), 1);
        assert!(signed.starts_with(b"DKIM-Signature: "));
        assert!(signed.ends_with(raw));
    }
}
