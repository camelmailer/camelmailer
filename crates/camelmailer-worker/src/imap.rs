//! A small, read-only IMAP4rev1 client (RFC 3501) for the seed placement
//! tests: log in, find the spam folder, `EXAMINE` a mailbox, search for a
//! header and fetch a message's header with `BODY.PEEK`.
//!
//! Read-only by construction: the client has no command that changes a
//! mailbox. `EXAMINE` opens it read-only and `BODY.PEEK` leaves `\Seen`
//! alone, so a seed's messages are never marked read, flagged, moved or
//! deleted.
//!
//! Authentication:
//! - `LOGIN` with an app password (Gmail with 2-Step Verification, Yahoo:
//!   https://help.yahoo.com/kb/SLN4075.html).
//! - `AUTHENTICATE XOAUTH2`, the SASL string
//!   `base64("user=" user "\x01auth=Bearer " token "\x01\x01")`
//!   (Gmail: https://developers.google.com/workspace/gmail/imap/xoauth2-protocol,
//!   Outlook.com: https://learn.microsoft.com/en-us/exchange/client-developer/legacy-protocols/how-to-authenticate-an-imap-pop-smtp-application-by-using-oauth).
//!   Outlook.com accepts nothing else since basic authentication ended on
//!   2024-09-16.

use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

/// The most a single response may hold (header fetches are small).
const MAX_RESPONSE: usize = 4 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ImapError {
    #[error("IMAP connection failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("IMAP server timed out")]
    Timeout,
    #[error("IMAP server refused {command}: {reply}")]
    Refused { command: String, reply: String },
    #[error("IMAP protocol error: {0}")]
    Protocol(String),
}

/// One untagged response: its text, with every literal's bytes in order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Untagged {
    pub text: String,
    pub literals: Vec<Vec<u8>>,
}

pub struct ImapClient {
    stream: BufReader<Box<dyn Io>>,
    tag: u32,
    timeout: Duration,
}

/// Quote a string for IMAP (`"..."` with `\` and `"` escaped). CR and LF
/// cannot be quoted and are refused.
pub fn quote(value: &str) -> Result<String, ImapError> {
    if value.contains(['\r', '\n']) {
        return Err(ImapError::Protocol("value contains a line break".into()));
    }
    Ok(format!(
        "\"{}\"",
        value.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

/// The SASL XOAUTH2 initial response, base64-encoded.
pub fn xoauth2_string(user: &str, access_token: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .encode(format!("user={user}\x01auth=Bearer {access_token}\x01\x01"))
}

impl ImapClient {
    /// Connect and read the greeting. `tls` wraps the connection in TLS
    /// from the first byte (port 993), verifying the certificate.
    pub async fn connect(
        host: &str,
        port: u16,
        tls: bool,
        timeout: Duration,
    ) -> Result<Self, ImapError> {
        let tcp = tokio::time::timeout(timeout, tokio::net::TcpStream::connect((host, port)))
            .await
            .map_err(|_| ImapError::Timeout)??;
        let stream: Box<dyn Io> = if tls {
            let mut roots = rustls::RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            let config = rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth();
            let connector = tokio_rustls::TlsConnector::from(std::sync::Arc::new(config));
            let name = rustls::pki_types::ServerName::try_from(host.to_string())
                .map_err(|_| ImapError::Protocol(format!("invalid host name {host}")))?;
            Box::new(
                tokio::time::timeout(timeout, connector.connect(name, tcp))
                    .await
                    .map_err(|_| ImapError::Timeout)??,
            )
        } else {
            Box::new(tcp)
        };
        let mut client = Self {
            stream: BufReader::new(stream),
            tag: 0,
            timeout,
        };
        let greeting = client.read_line().await?;
        if !greeting.starts_with("* OK") && !greeting.starts_with("* PREAUTH") {
            return Err(ImapError::Refused {
                command: "the connection".into(),
                reply: greeting.trim_end().to_string(),
            });
        }
        Ok(client)
    }

    async fn read_line(&mut self) -> Result<String, ImapError> {
        let mut line = Vec::new();
        let read = tokio::time::timeout(self.timeout, self.stream.read_until(b'\n', &mut line))
            .await
            .map_err(|_| ImapError::Timeout)??;
        if read == 0 {
            return Err(ImapError::Protocol("connection closed".into()));
        }
        if line.len() > MAX_RESPONSE {
            return Err(ImapError::Protocol("line too long".into()));
        }
        Ok(String::from_utf8_lossy(&line).into_owned())
    }

    /// The literal length a line announces at its end (`{123}`), if any.
    fn literal_length(line: &str) -> Option<usize> {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        let open = trimmed.strip_suffix('}')?.rfind('{')?;
        trimmed[open + 1..trimmed.len() - 1]
            .trim_end_matches('+')
            .parse()
            .ok()
    }

    /// Send one command and collect its untagged responses until the tagged
    /// completion. A `+` continuation (a failed XOAUTH2 attempt's error
    /// challenge) is answered with an empty line, as RFC 7628 asks.
    async fn command(&mut self, command: &str) -> Result<Vec<Untagged>, ImapError> {
        self.tag += 1;
        let tag = format!("C{}", self.tag);
        let line = format!("{tag} {command}\r\n");
        tokio::time::timeout(self.timeout, async {
            self.stream.get_mut().write_all(line.as_bytes()).await?;
            self.stream.get_mut().flush().await
        })
        .await
        .map_err(|_| ImapError::Timeout)??;

        let verb = command
            .split_whitespace()
            .take(if command.starts_with("UID ") { 2 } else { 1 })
            .collect::<Vec<_>>()
            .join(" ");
        let mut untagged: Vec<Untagged> = Vec::new();
        let mut size = 0usize;
        loop {
            let line = self.read_line().await?;
            size += line.len();
            if line.starts_with(&format!("{tag} ")) {
                let status = line[tag.len() + 1..].trim_end();
                if status.starts_with("OK") {
                    return Ok(untagged);
                }
                return Err(ImapError::Refused {
                    command: verb,
                    reply: status.to_string(),
                });
            }
            if line.starts_with('+') {
                tokio::time::timeout(self.timeout, async {
                    self.stream.get_mut().write_all(b"\r\n").await?;
                    self.stream.get_mut().flush().await
                })
                .await
                .map_err(|_| ImapError::Timeout)??;
                continue;
            }
            let mut current = Untagged {
                text: line.clone(),
                literals: Vec::new(),
            };
            let mut pending = line;
            while let Some(length) = Self::literal_length(&pending) {
                size += length;
                if size > MAX_RESPONSE {
                    return Err(ImapError::Protocol("response too large".into()));
                }
                let mut literal = vec![0u8; length];
                tokio::time::timeout(self.timeout, self.stream.read_exact(&mut literal))
                    .await
                    .map_err(|_| ImapError::Timeout)??;
                current.literals.push(literal);
                pending = self.read_line().await?;
                current.text.push_str(&pending);
            }
            untagged.push(current);
        }
    }

    pub async fn login(&mut self, user: &str, password: &str) -> Result<(), ImapError> {
        let command = format!("LOGIN {} {}", quote(user)?, quote(password)?);
        self.command(&command)
            .await
            .map(|_| ())
            .map_err(|e| match e {
                // Never echo the command (it carries the password).
                ImapError::Refused { reply, .. } => ImapError::Refused {
                    command: "LOGIN".into(),
                    reply,
                },
                other => other,
            })
    }

    pub async fn authenticate_xoauth2(
        &mut self,
        user: &str,
        access_token: &str,
    ) -> Result<(), ImapError> {
        let command = format!(
            "AUTHENTICATE XOAUTH2 {}",
            xoauth2_string(user, access_token)
        );
        self.command(&command).await.map(|_| ())
    }

    /// Every mailbox name with its attributes (`LIST "" "*"`).
    pub async fn list(&mut self) -> Result<Vec<(Vec<String>, String)>, ImapError> {
        let responses = self.command("LIST \"\" \"*\"").await?;
        Ok(responses.iter().filter_map(parse_list).collect())
    }

    /// Open a mailbox read-only.
    pub async fn examine(&mut self, mailbox: &str) -> Result<(), ImapError> {
        self.command(&format!("EXAMINE {}", quote(mailbox)?))
            .await
            .map(|_| ())
    }

    /// UIDs of the messages in the open mailbox whose header `name`
    /// contains `value`.
    pub async fn search_header(&mut self, name: &str, value: &str) -> Result<Vec<u32>, ImapError> {
        let responses = self
            .command(&format!(
                "UID SEARCH HEADER {} {}",
                quote(name)?,
                quote(value)?
            ))
            .await?;
        Ok(responses
            .iter()
            .filter_map(|r| r.text.trim_end().strip_prefix("* SEARCH"))
            .flat_map(|ids| {
                ids.split_whitespace()
                    .filter_map(|id| id.parse::<u32>().ok())
                    .collect::<Vec<_>>()
            })
            .collect())
    }

    /// The header of one message, without marking it read.
    pub async fn fetch_header(&mut self, uid: u32) -> Result<Vec<u8>, ImapError> {
        let responses = self
            .command(&format!("UID FETCH {uid} (BODY.PEEK[HEADER])"))
            .await?;
        responses
            .into_iter()
            .find(|r| r.text.contains("FETCH") && !r.literals.is_empty())
            .and_then(|r| r.literals.into_iter().next())
            .ok_or_else(|| ImapError::Protocol(format!("no header for UID {uid}")))
    }

    pub async fn logout(mut self) {
        let _ = self.command("LOGOUT").await;
    }
}

/// `* LIST (\HasNoChildren \Junk) "/" "[Gmail]/Spam"` → attributes and name.
pub fn parse_list(response: &Untagged) -> Option<(Vec<String>, String)> {
    let rest = response.text.trim_end().strip_prefix("* LIST ")?;
    let close = rest.find(')')?;
    let attributes = rest[1..close]
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let after = rest[close + 1..].trim_start();
    // the hierarchy delimiter: a quoted character or NIL
    let after = if let Some(stripped) = after.strip_prefix("NIL") {
        stripped
    } else {
        let mut chars = after.char_indices();
        chars.next()?; // opening quote
        let mut end = None;
        let mut escaped = false;
        for (i, c) in chars {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                end = Some(i);
                break;
            }
        }
        &after[end? + 1..]
    };
    let name = after.trim();
    let name = if let Some(literal) = response.literals.first().filter(|_| name.ends_with('}')) {
        String::from_utf8_lossy(literal).into_owned()
    } else if let Some(quoted) = name.strip_prefix('"').and_then(|n| n.strip_suffix('"')) {
        quoted.replace("\\\"", "\"").replace("\\\\", "\\")
    } else {
        name.to_string()
    };
    Some((attributes, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_escapes_and_refuses_line_breaks() {
        assert_eq!(quote(r#"a"b\c"#).unwrap(), r#""a\"b\\c""#);
        assert!(quote("a\r\nb").is_err());
    }

    #[test]
    fn the_xoauth2_string_follows_the_documented_shape() {
        use base64::Engine;
        let encoded = xoauth2_string(
            "someuser@example.com",
            "ya29.vF9dft4qmTc2Nvb3RlckBhdHRhdmlzdGEuY29tCg",
        );
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        assert_eq!(
            decoded,
            b"user=someuser@example.com\x01auth=Bearer ya29.vF9dft4qmTc2Nvb3RlckBhdHRhdmlzdGEuY29tCg\x01\x01"
        );
    }

    #[test]
    fn list_lines_yield_attributes_and_names() {
        let line = |text: &str| Untagged {
            text: text.into(),
            literals: Vec::new(),
        };
        assert_eq!(
            parse_list(&line(
                "* LIST (\\HasNoChildren \\Junk) \"/\" \"[Gmail]/Spam\"\r\n"
            )),
            Some((
                vec!["\\HasNoChildren".into(), "\\Junk".into()],
                "[Gmail]/Spam".into()
            ))
        );
        assert_eq!(
            parse_list(&line("* LIST (\\HasNoChildren) \".\" INBOX\r\n")),
            Some((vec!["\\HasNoChildren".into()], "INBOX".into()))
        );
        assert_eq!(
            parse_list(&line("* LIST () NIL Bulk\r\n")),
            Some((vec![], "Bulk".into()))
        );
    }

    #[test]
    fn literal_lengths_are_read_from_the_line_end() {
        assert_eq!(
            ImapClient::literal_length("* 1 FETCH (UID 4 BODY[HEADER] {120}\r\n"),
            Some(120)
        );
        assert_eq!(ImapClient::literal_length("* OK done\r\n"), None);
    }
}
