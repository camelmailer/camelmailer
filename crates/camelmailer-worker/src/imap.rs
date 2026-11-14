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

/// The most a single response may hold, literals included (header fetches
/// are small). Checked before a literal is allocated.
const MAX_RESPONSE: usize = 4 * 1024 * 1024;
/// The longest line accepted, CRLF included. Checked while reading, so a
/// server that never sends a line end cannot make the client buffer more.
const MAX_LINE: usize = 1024 * 1024;
/// The most untagged responses one command may produce.
const MAX_UNTAGGED: usize = 100_000;

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
        Self::start(stream, timeout).await
    }

    /// Read the greeting on an open stream.
    async fn start(stream: Box<dyn Io>, timeout: Duration) -> Result<Self, ImapError> {
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

    /// One line up to and including its `\n`, at most [`MAX_LINE`] bytes.
    /// Invalid UTF-8 is replaced, never trusted.
    async fn read_line(&mut self) -> Result<String, ImapError> {
        let timeout = self.timeout;
        let mut line = Vec::new();
        tokio::time::timeout(timeout, async {
            loop {
                let available = self.stream.fill_buf().await?;
                if available.is_empty() {
                    return Err(ImapError::Protocol("connection closed".into()));
                }
                let (take, done) = match available.iter().position(|b| *b == b'\n') {
                    Some(at) => (at + 1, true),
                    None => (available.len(), false),
                };
                if line.len() + take > MAX_LINE {
                    return Err(ImapError::Protocol("line too long".into()));
                }
                line.extend_from_slice(&available[..take]);
                self.stream.consume(take);
                if done {
                    return Ok(());
                }
            }
        })
        .await
        .map_err(|_| ImapError::Timeout)??;
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
        let too_large = || ImapError::Protocol("response too large".into());
        loop {
            let line = self.read_line().await?;
            size = size.saturating_add(line.len());
            if size > MAX_RESPONSE {
                return Err(too_large());
            }
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
                size = size.saturating_add(length);
                if size > MAX_RESPONSE {
                    return Err(too_large());
                }
                let mut literal = vec![0u8; length];
                tokio::time::timeout(self.timeout, self.stream.read_exact(&mut literal))
                    .await
                    .map_err(|_| ImapError::Timeout)??;
                current.literals.push(literal);
                pending = self.read_line().await?;
                size = size.saturating_add(pending.len());
                if size > MAX_RESPONSE {
                    return Err(too_large());
                }
                current.text.push_str(&pending);
            }
            if untagged.len() >= MAX_UNTAGGED {
                return Err(ImapError::Protocol("too many responses".into()));
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
    let inner = rest.strip_prefix('(')?;
    let close = inner.find(')')?;
    let attributes = inner[..close]
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let after = inner[close + 1..].trim_start();
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

    // ------------------------------------------------ hostile servers
    //
    // The client parses whatever an IMAP server sends, so a broken or
    // hostile server must never make it panic or buffer without bound. The
    // tests below feed it crafted and generated responses over an in-memory
    // stream. The generator is a seeded xorshift, so a failure reproduces
    // with the printed seed; it needs no extra crate and runs in `cargo test`.

    /// Run `op` against a server that sends `bytes` and then closes.
    async fn against(bytes: Vec<u8>, op: u8) -> Result<usize, ImapError> {
        let (client_side, server_side) = tokio::io::duplex(64 * 1024);
        let (mut from_client, mut to_client) = tokio::io::split(server_side);
        // Swallow whatever the client sends, so its commands never fail on
        // a closed pipe; the response below is all that matters.
        tokio::spawn(async move {
            let mut sink = [0u8; 1024];
            while matches!(from_client.read(&mut sink).await, Ok(n) if n > 0) {}
        });
        tokio::spawn(async move {
            // Feed in uneven pieces so CRLF and literals are split, then
            // close the server's side: the client reads the end of stream.
            let mut rest = &bytes[..];
            let mut piece = 1;
            while !rest.is_empty() {
                let n = piece.min(rest.len());
                if to_client.write_all(&rest[..n]).await.is_err() {
                    return;
                }
                rest = &rest[n..];
                piece = (piece * 7 + 3) % 4096 + 1;
            }
            let _ = to_client.shutdown().await;
        });
        let mut client =
            ImapClient::start(Box::new(client_side), Duration::from_millis(500)).await?;
        let size = match op % 5 {
            0 => client.search_header("X-Test", "token").await?.len(),
            1 => client.list().await?.len(),
            2 => client.fetch_header(1).await?.len(),
            3 => client.examine("INBOX").await.map(|_| 0)?,
            _ => client.login("user", "pass").await.map(|_| 0)?,
        };
        Ok(size)
    }

    fn greeting(rest: &[u8]) -> Vec<u8> {
        let mut bytes = b"* OK ready\r\n".to_vec();
        bytes.extend_from_slice(rest);
        bytes
    }

    #[tokio::test]
    async fn a_huge_or_overflowing_literal_is_refused_before_it_is_allocated() {
        for announced in ["{999999999}", "{18446744073709551615}", "{4194305}"] {
            let bytes = greeting(format!("* 1 FETCH (BODY[HEADER] {announced}\r\nabc").as_bytes());
            let error = against(bytes, 2).await.unwrap_err();
            assert!(
                error.to_string().contains("too large"),
                "{announced}: {error}"
            );
        }
        // too large to parse at all: no literal, an ordinary line
        let bytes = greeting(b"* 1 FETCH {99999999999999999999999}\r\nC1 OK\r\n");
        assert!(against(bytes, 2).await.is_err());
    }

    #[tokio::test]
    async fn a_line_without_an_end_stops_at_the_line_limit() {
        let mut bytes = greeting(b"* SEARCH ");
        bytes.extend(std::iter::repeat_n(b'1', MAX_LINE + 10));
        let error = against(bytes, 0).await.unwrap_err();
        assert!(error.to_string().contains("line too long"), "{error}");
        // the greeting itself is bounded too
        let bytes = vec![b'*'; MAX_LINE + 1];
        assert!(against(bytes, 0).await.is_err());
    }

    #[tokio::test]
    async fn endless_untagged_responses_stop_at_the_limits() {
        let mut bytes = greeting(b"");
        for _ in 0..=MAX_UNTAGGED {
            bytes.extend_from_slice(b"* X\r\n");
        }
        let error = against(bytes, 0).await.unwrap_err();
        assert!(error.to_string().contains("too many"), "{error}");
        let mut bytes = greeting(b"");
        let line = format!("* {}\r\n", "y".repeat(60_000));
        for _ in 0..100 {
            bytes.extend_from_slice(line.as_bytes());
        }
        let error = against(bytes, 0).await.unwrap_err();
        assert!(error.to_string().contains("too large"), "{error}");
    }

    #[tokio::test]
    async fn invalid_utf8_nul_bytes_and_split_line_ends_are_tolerated() {
        let bytes = greeting(b"* SEARCH 4 \xff\xfe\x00 7\r\n* \xc3\r\nC1 OK done\r\n");
        assert_eq!(against(bytes, 0).await.unwrap(), 2);
        // a missing tag: the server closes instead of completing
        assert!(against(greeting(b"* SEARCH 4\r\n"), 0).await.is_err());
        // no greeting at all
        assert!(against(b"C1 OK\r\n".to_vec(), 0).await.is_err());
        assert!(against(Vec::new(), 0).await.is_err());
    }

    #[test]
    fn hostile_list_lines_never_panic() {
        let line = |text: &str| Untagged {
            text: text.into(),
            literals: vec![b"Name".to_vec()],
        };
        for text in [
            "* LIST )",
            "* LIST é) \"/\" x",
            "* LIST (\\Junk",
            "* LIST (\\Junk) \"",
            "* LIST (\\Junk) \"\\",
            "* LIST (\\Junk) \"/\" \"unterminated",
            "* LIST (((((((((((((((((((((( \"/\" x",
            "* LIST () NIL {4}",
            "* LIST (\0\0) \"\0\" \0",
            "* LIST ",
            "* LIST (é) \"é\" é",
        ] {
            let _ = parse_list(&line(text));
        }
        assert_eq!(parse_list(&line("* LIST )")), None);
        assert_eq!(
            parse_list(&line("* LIST () NIL {4}")).map(|(_, name)| name),
            Some("Name".to_string())
        );
    }

    /// A seeded xorshift64.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    const FRAGMENTS: &[&[u8]] = &[
        b"* ",
        b"C1 ",
        b"C2 ",
        b"OK",
        b"NO",
        b"BAD",
        b" done",
        b"\r\n",
        b"\n",
        b"\r",
        b"{",
        b"}",
        b"{999999999}\r\n",
        b"{18446744073709551615}\r\n",
        b"{5}\r\n",
        b"{0}\r\n",
        b"{3+}\r\n",
        b"\"",
        b"\\",
        b"(",
        b")",
        b"((((((((((((((((",
        b"* LIST ",
        b"* SEARCH ",
        b"1 2 3 ",
        b"* 1 FETCH (UID 1 BODY[HEADER] ",
        b"\0",
        b"\xff\xfe",
        b"\xc3",
        b"\xe2\x82",
        b"+ ",
        b"+ eyJzdGF0dXMiOiI0MDAifQ==\r\n",
        b"NIL",
        b"\\Junk",
        b"\"/\"",
        b"INBOX",
        b"* BYE\r\n",
        b"C1 OK done\r\n",
    ];

    #[tokio::test]
    async fn generated_server_responses_never_panic_or_hang() {
        for seed in 1..=2_000u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let mut bytes = if rng.below(8) == 0 {
                Vec::new()
            } else {
                b"* OK ready\r\n".to_vec()
            };
            for _ in 0..rng.below(60) {
                match rng.below(20) {
                    // now and then a long run of one fragment
                    0 => {
                        let fragment = FRAGMENTS[rng.below(FRAGMENTS.len())];
                        for _ in 0..rng.below(20_000) {
                            bytes.extend_from_slice(fragment);
                        }
                    }
                    1 => bytes.push(rng.next() as u8),
                    _ => bytes.extend_from_slice(FRAGMENTS[rng.below(FRAGMENTS.len())]),
                }
            }
            let op = rng.below(5) as u8;
            let outcome = tokio::time::timeout(Duration::from_secs(5), against(bytes, op)).await;
            assert!(outcome.is_ok(), "seed {seed} hung");
            if let Ok(Ok(size)) = outcome {
                assert!(size <= MAX_RESPONSE, "seed {seed}: {size}");
            }
        }
    }
}
