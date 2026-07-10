//! Message inspection: rspamd spam scoring and ClamAV virus scanning —
//! the port of `lib/postal/message_inspectors/` and `spam_check.rb`.

use camelmailer_config::{Clamav, Rspamd};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

#[derive(Debug, Clone, PartialEq)]
pub struct SpamResult {
    pub score: f64,
    pub threshold: f64,
    /// rspamd action, e.g. "no action", "add header", "reject"
    pub action: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirusResult {
    pub found: bool,
    /// Signature name when a threat was found.
    pub details: Option<String>,
}

/// rspamd HTTP client (`/checkv2`).
pub struct RspamdInspector {
    http: reqwest::Client,
    base_url: String,
    password: Option<String>,
    flags: Option<String>,
}

impl RspamdInspector {
    pub fn new(config: &Rspamd) -> Self {
        let scheme = if config.ssl { "https" } else { "http" };
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(20))
                .build()
                .expect("reqwest client"),
            base_url: format!("{scheme}://{}:{}", config.host, config.port),
            password: config.password.clone(),
            flags: config.flags.clone(),
        }
    }

    /// Score a raw message. `threshold` is the server's spam threshold; it
    /// is echoed back in the result for the caller's decision.
    pub async fn check(&self, raw_message: &[u8], threshold: f64) -> Result<SpamResult, String> {
        let mut request = self
            .http
            .post(format!("{}/checkv2", self.base_url))
            .header("content-type", "application/octet-stream")
            .body(raw_message.to_vec());
        if let Some(password) = &self.password {
            request = request.header("Password", password);
        }
        if let Some(flags) = &self.flags {
            request = request.header("Flags", flags);
        }
        let response = request
            .send()
            .await
            .map_err(|error| format!("rspamd request failed: {error}"))?;
        if !response.status().is_success() {
            return Err(format!("rspamd returned {}", response.status()));
        }
        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|error| format!("rspamd response was not JSON: {error}"))?;
        let score = body
            .get("score")
            .and_then(|value| value.as_f64())
            .unwrap_or(0.0);
        let action = body
            .get("action")
            .and_then(|value| value.as_str())
            .unwrap_or("no action")
            .to_string();
        Ok(SpamResult {
            score,
            threshold,
            action,
        })
    }
}

/// ClamAV clamd client (INSTREAM over TCP).
pub struct ClamavInspector {
    host: String,
    port: u16,
}

impl ClamavInspector {
    pub fn new(config: &Clamav) -> Self {
        Self {
            host: config.host.clone(),
            port: config.port,
        }
    }

    pub async fn scan(&self, raw_message: &[u8]) -> Result<VirusResult, String> {
        self.scan_inner(raw_message)
            .await
            .map_err(|error| format!("clamav scan failed: {error}"))
    }

    async fn scan_inner(&self, raw_message: &[u8]) -> std::io::Result<VirusResult> {
        let mut stream = tokio::time::timeout(
            Duration::from_secs(10),
            TcpStream::connect((self.host.as_str(), self.port)),
        )
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "connect timeout"))??;

        stream.write_all(b"zINSTREAM\0").await?;
        // chunks: 4-byte big-endian length prefix, then the data; a zero
        // length terminates the stream.
        for chunk in raw_message.chunks(8192) {
            stream
                .write_all(&(chunk.len() as u32).to_be_bytes())
                .await?;
            stream.write_all(chunk).await?;
        }
        stream.write_all(&0u32.to_be_bytes()).await?;
        stream.flush().await?;

        let mut response = Vec::new();
        stream.read_to_end(&mut response).await?;
        let text = String::from_utf8_lossy(&response);
        let text = text.trim_end_matches(['\0', '\n', ' ']);

        // "stream: OK" or "stream: <Signature> FOUND"
        if text.ends_with("OK") {
            Ok(VirusResult {
                found: false,
                details: None,
            })
        } else if let Some(rest) = text.strip_suffix(" FOUND") {
            let signature = rest.rsplit(':').next().unwrap_or(rest).trim().to_string();
            Ok(VirusResult {
                found: true,
                details: Some(signature),
            })
        } else {
            Err(std::io::Error::other(format!(
                "unexpected clamav response: {text}"
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spam_result_carries_threshold() {
        let result = SpamResult {
            score: 8.0,
            threshold: 5.0,
            action: "reject".into(),
        };
        assert!(result.score > result.threshold);
    }

    #[test]
    fn virus_result_variants() {
        let clean = VirusResult {
            found: false,
            details: None,
        };
        assert!(!clean.found);
    }
}
