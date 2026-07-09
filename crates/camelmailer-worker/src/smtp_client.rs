//! A minimal async SMTP client for outbound delivery — the port of
//! `app/lib/smtp_client.rb` / the `send_message` path of the SMTP sender.

use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

/// The outcome of a delivery attempt, classified like Postal's
/// Sent / SoftFail / HardFail statuses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendOutcome {
    Sent { response: String },
    /// Retryable: connection problems and 4xx replies.
    SoftFail { response: String },
    /// Permanent: 5xx replies.
    HardFail { response: String },
}

fn classify(code: u16, response: String) -> SendOutcome {
    match code {
        200..=399 => SendOutcome::Sent { response },
        400..=499 => SendOutcome::SoftFail { response },
        _ => SendOutcome::HardFail { response },
    }
}

struct SmtpConnection {
    reader: BufReader<tokio::net::tcp::OwnedReadHalf>,
    writer: tokio::net::tcp::OwnedWriteHalf,
    timeout: Duration,
}

impl SmtpConnection {
    /// Read one (possibly multiline) SMTP reply; returns (code, full text).
    async fn read_reply(&mut self) -> std::io::Result<(u16, String)> {
        let mut full = String::new();
        loop {
            let mut line = String::new();
            let read = tokio::time::timeout(self.timeout, self.reader.read_line(&mut line))
                .await
                .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "read timeout"))??;
            if read == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "connection closed",
                ));
            }
            full.push_str(&line);
            // "250-..." continues, "250 ..." (or a short line) terminates
            if line.len() < 4 || line.as_bytes().get(3) != Some(&b'-') {
                let code = line
                    .get(0..3)
                    .and_then(|c| c.parse::<u16>().ok())
                    .unwrap_or(0);
                return Ok((code, full.trim().to_string()));
            }
        }
    }

    async fn send_line(&mut self, line: &str) -> std::io::Result<()> {
        self.writer
            .write_all(format!("{line}\r\n").as_bytes())
            .await
    }

    async fn command(&mut self, line: &str) -> std::io::Result<(u16, String)> {
        self.send_line(line).await?;
        self.read_reply().await
    }
}

/// Deliver a raw message over SMTP. Any I/O error is a soft failure
/// (retryable); SMTP rejections are classified by status code.
#[allow(clippy::too_many_arguments)]
pub async fn send_message(
    host: &str,
    port: u16,
    helo_hostname: &str,
    mail_from: &str,
    rcpt_to: &str,
    raw_message: &[u8],
    timeout: Duration,
) -> SendOutcome {
    match try_send(host, port, helo_hostname, mail_from, rcpt_to, raw_message, timeout).await {
        Ok(outcome) => outcome,
        Err(error) => SendOutcome::SoftFail {
            response: format!("connection error to {host}:{port}: {error}"),
        },
    }
}

async fn try_send(
    host: &str,
    port: u16,
    helo_hostname: &str,
    mail_from: &str,
    rcpt_to: &str,
    raw_message: &[u8],
    timeout: Duration,
) -> std::io::Result<SendOutcome> {
    let stream = tokio::time::timeout(timeout, TcpStream::connect((host, port)))
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "connect timeout"))??;
    let (read_half, write_half) = stream.into_split();
    let mut connection = SmtpConnection {
        reader: BufReader::new(read_half),
        writer: write_half,
        timeout,
    };

    let (code, response) = connection.read_reply().await?;
    if code != 220 {
        return Ok(classify(code, response));
    }

    let (code, response) = connection.command(&format!("EHLO {helo_hostname}")).await?;
    if !(200..=299).contains(&code) {
        // fall back to HELO for ancient servers
        let (code, response) = connection.command(&format!("HELO {helo_hostname}")).await?;
        if !(200..=299).contains(&code) {
            return Ok(classify(code, response));
        }
    } else {
        let _ = response;
    }

    let (code, response) = connection
        .command(&format!("MAIL FROM:<{mail_from}>"))
        .await?;
    if !(200..=299).contains(&code) {
        return Ok(classify(code, response));
    }

    let (code, response) = connection.command(&format!("RCPT TO:<{rcpt_to}>")).await?;
    if !(200..=299).contains(&code) {
        return Ok(classify(code, response));
    }

    let (code, response) = connection.command("DATA").await?;
    if code != 354 {
        return Ok(classify(code, response));
    }

    // body with dot-stuffing and CRLF normalization
    let mut body = Vec::with_capacity(raw_message.len() + 64);
    let mut lines: Vec<&[u8]> = raw_message.split(|&b| b == b'\n').collect();
    if lines.last() == Some(&&b""[..]) {
        lines.pop(); // trailing newline produces an empty segment, not a line
    }
    for line in lines {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.first() == Some(&b'.') {
            body.push(b'.');
        }
        body.extend_from_slice(line);
        body.extend_from_slice(b"\r\n");
    }
    connection.writer.write_all(&body).await?;
    let (code, response) = connection.command(".").await?;
    let outcome = classify(code, response);

    let _ = connection.send_line("QUIT").await;
    Ok(outcome)
}
