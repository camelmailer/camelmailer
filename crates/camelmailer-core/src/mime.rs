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
}
