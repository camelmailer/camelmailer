//! The SMTP session state machine — a line-for-line port of
//! `app/lib/smtp_server/client.rb`.
//!
//! [`Session`] is a pure state machine: it receives one line of input at a
//! time via [`Session::handle`] and returns the reply to send (if any). It
//! performs no I/O itself; storage lookups go through
//! [`camelmailer_core::Store`] and accepted messages are handed to a
//! [`camelmailer_core::MessageSink`]. This is what makes the protocol fully
//! unit-testable, mirroring the Ruby specs in
//! `spec/lib/smtp_server/client/`.

use base64::alphabet;
use base64::engine::{DecodePaddingMode, Engine, GeneralPurpose, GeneralPurposeConfig};
use camelmailer_core::received_header::{self, ReceiveMethod};
use camelmailer_core::{
    Credential, MessageScope, MessageSink, QueuedMessage, ResolvedRoute, RouteMode, Server, Store,
};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;

/// Base64 decoding as lenient as Ruby's `Base64.decode64` (which ignores
/// characters outside the alphabet and tolerates missing padding).
fn decode64_lenient(input: &str) -> Vec<u8> {
    let filtered: String = input
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '='))
        .collect();
    let engine = GeneralPurpose::new(
        &alphabet::STANDARD,
        GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
    );
    engine.decode(&filtered).unwrap_or_default()
}

fn encode64(input: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(input)
}

/// The runtime configuration a session needs (a slice of the full config).
#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub smtp_hostname: String,
    pub tls_enabled: bool,
    /// in megabytes
    pub max_message_size: u64,
    pub return_path_domain: String,
    pub custom_return_path_prefix: String,
    pub route_domain: String,
}

impl From<&camelmailer_config::Config> for SessionConfig {
    fn from(config: &camelmailer_config::Config) -> Self {
        Self {
            smtp_hostname: config.camelmailer.smtp_hostname.clone(),
            tls_enabled: config.smtp_server.tls_enabled,
            max_message_size: config.smtp_server.max_message_size,
            return_path_domain: config.dns.return_path_domain.clone(),
            custom_return_path_prefix: config.dns.custom_return_path_prefix.clone(),
            route_domain: config.dns.route_domain.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Preauth,
    Welcome,
    Welcomed,
    MailFromReceived,
    RcptToReceived,
}

/// The reply to send to the client for one line of input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// No reply (mid-DATA).
    None,
    Line(String),
    /// A multiline reply (EHLO). Lines already carry their prefixes.
    Lines(Vec<String>),
}

impl Reply {
    fn line(text: impl Into<String>) -> Self {
        Self::Line(text.into())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecipientKind {
    Bounce,
    Route,
    Credential,
}

#[derive(Debug, Clone)]
pub struct Recipient {
    pub kind: RecipientKind,
    pub rcpt_to: String,
    pub server: Server,
    pub route: Option<ResolvedRoute>,
}

/// Pending continuation state — the port of the `@proc` closures in Ruby.
enum InputMode {
    Command,
    AuthPlain,
    AuthLoginUsername,
    AuthLoginPassword,
    AuthCramMd5 { challenge: String },
    Data,
}

pub struct Session {
    config: SessionConfig,
    store: Arc<dyn Store>,
    sink: Arc<dyn MessageSink>,
    clock: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>,

    state: State,
    input_mode: InputMode,
    trace_id: String,
    ip_address: Option<String>,
    /// The reverse-DNS hostname of the client, resolved by the server layer
    /// before DATA. Falls back to the IP address.
    resolved_hostname: Option<String>,
    helo_name: Option<String>,
    tls: bool,
    start_tls: bool,
    finished: bool,

    credential: Option<Credential>,
    recipients: Vec<Recipient>,
    mail_from: Option<String>,
    data: Option<Vec<u8>>,
    headers: Option<HashMap<String, Vec<String>>>,
    receiving_headers: bool,
    header_key: Option<String>,

    cr_present: bool,
    previous_cr_present: Option<bool>,
}

impl Session {
    pub fn new(
        config: SessionConfig,
        store: Arc<dyn Store>,
        sink: Arc<dyn MessageSink>,
        ip_address: Option<String>,
    ) -> Self {
        let state = if ip_address.is_some() {
            State::Welcome
        } else {
            State::Preauth
        };
        Self {
            config,
            store,
            sink,
            clock: Arc::new(Utc::now),
            state,
            input_mode: InputMode::Command,
            trace_id: camelmailer_core::token::generate_trace_id(),
            ip_address,
            resolved_hostname: None,
            helo_name: None,
            tls: false,
            start_tls: false,
            finished: false,
            credential: None,
            recipients: vec![],
            mail_from: None,
            data: None,
            headers: None,
            receiving_headers: false,
            header_key: None,
            cr_present: false,
            previous_cr_present: None,
        }
    }

    /// The greeting banner sent when a connection opens (or after a PROXY
    /// line has identified the client).
    pub fn banner(&self) -> String {
        format!(
            "220 {} ESMTP CamelMailer/{}",
            self.config.smtp_hostname, self.trace_id
        )
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn trace_id(&self) -> &str {
        &self.trace_id
    }

    pub fn ip_address(&self) -> Option<&str> {
        self.ip_address.as_deref()
    }

    pub fn helo_name(&self) -> Option<&str> {
        self.helo_name.as_deref()
    }

    pub fn credential(&self) -> Option<&Credential> {
        self.credential.as_ref()
    }

    pub fn recipients(&self) -> &[Recipient] {
        &self.recipients
    }

    pub fn mail_from(&self) -> Option<&str> {
        self.mail_from.as_deref()
    }

    pub fn headers(&self) -> Option<&HashMap<String, Vec<String>>> {
        self.headers.as_ref()
    }

    pub fn raw_data(&self) -> Option<&[u8]> {
        self.data.as_deref()
    }

    pub fn finished(&self) -> bool {
        self.finished
    }

    /// Whether the transport should upgrade to TLS after flushing the reply.
    pub fn take_start_tls(&mut self) -> bool {
        std::mem::take(&mut self.start_tls)
    }

    /// Mark the transport as TLS-protected (after a completed handshake).
    pub fn set_tls(&mut self, tls: bool) {
        self.tls = tls;
    }

    pub fn set_resolved_hostname(&mut self, hostname: String) {
        self.resolved_hostname = Some(hostname);
    }

    /// Replace the clock (tests freeze time with this).
    pub fn set_clock(&mut self, clock: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>) {
        self.clock = clock;
    }

    fn transaction_reset(&mut self) {
        self.recipients.clear();
        self.mail_from = None;
        self.data = None;
        self.headers = None;
    }

    /// Handle one line of input. The line must not contain the trailing
    /// `\n`; a trailing `\r` is significant (bare-LF detection) and is
    /// stripped here, mirroring `Client#handle`.
    pub fn handle(&mut self, line: &str) -> Reply {
        let data = if let Some(stripped) = line.strip_suffix('\r') {
            self.cr_present = true;
            stripped.to_string()
        } else {
            self.cr_present = false;
            line.to_string()
        };

        let reply = if self.state == State::Preauth {
            self.proxy(&data)
        } else {
            match &self.input_mode {
                InputMode::Command => self.handle_command(&data),
                InputMode::AuthPlain => self.auth_plain_continue(&data),
                InputMode::AuthLoginUsername => self.auth_login_username(),
                InputMode::AuthLoginPassword => self.auth_login_password(&data),
                InputMode::AuthCramMd5 { challenge } => {
                    let challenge = challenge.clone();
                    self.auth_cram_md5_continue(&data, &challenge)
                }
                InputMode::Data => self.data_line(&data),
            }
        };

        self.previous_cr_present = Some(self.cr_present);
        reply
    }

    fn handle_command(&mut self, data: &str) -> Reply {
        let upper = data.trim_start().to_ascii_uppercase();
        if upper.starts_with("QUIT") {
            self.quit()
        } else if upper.starts_with("STARTTLS") {
            self.starttls()
        } else if upper.starts_with("EHLO") {
            self.ehlo(data)
        } else if upper.starts_with("HELO") {
            self.helo(data)
        } else if upper.starts_with("RSET") {
            self.rset()
        } else if upper.starts_with("NOOP") {
            self.noop()
        } else if upper.starts_with("AUTH PLAIN") {
            self.auth_plain(data)
        } else if upper.starts_with("AUTH LOGIN") {
            self.auth_login(data)
        } else if upper.starts_with("AUTH CRAM-MD5") {
            self.auth_cram_md5()
        } else if upper.starts_with("MAIL FROM") {
            self.mail_from_command(data)
        } else if upper.starts_with("RCPT TO") {
            self.rcpt_to(data)
        } else if upper.starts_with("DATA") {
            self.data_command()
        } else {
            Reply::line("502 Invalid/unsupported command")
        }
    }

    fn proxy(&mut self, data: &str) -> Reply {
        // PROXY inet-protocol client-ip proxy-ip client-port proxy-port
        let parts: Vec<&str> = data.split(' ').collect();
        if parts.len() == 6 && parts[0] == "PROXY" && parts.iter().all(|p| !p.is_empty()) {
            self.ip_address = Some(parts[2].to_string());
            self.state = State::Welcome;
            return Reply::Line(self.banner());
        }
        self.finished = true;
        Reply::line("502 Proxy Error")
    }

    fn quit(&mut self) -> Reply {
        self.finished = true;
        Reply::line("221 Closing Connection")
    }

    fn starttls(&mut self) -> Reply {
        if self.config.tls_enabled {
            self.start_tls = true;
            self.tls = true;
            Reply::line("220 Ready to start TLS")
        } else {
            Reply::line("502 TLS not available")
        }
    }

    fn ehlo(&mut self, data: &str) -> Reply {
        self.helo_name = data.trim().split_once(' ').map(|(_, rest)| rest.to_string());
        self.transaction_reset();
        self.state = State::Welcomed;

        let mut capabilities: Vec<&str> = vec![];
        // STARTTLS is offered until the session has been upgraded.
        if self.config.tls_enabled && !self.tls {
            capabilities.push("STARTTLS");
        }
        // Only advertise AUTH once the session is TLS-protected (post-STARTTLS)
        // so submission credentials can never travel in cleartext. When TLS is
        // disabled entirely we fall back to advertising it (legacy/plaintext
        // deployments). CRAM-MD5 is deliberately omitted: the CRAM-MD5
        // mechanism requires a non-standard "org/server" username and breaks
        // standard clients. PLAIN/LOGIN (password == credential key) is the
        // correct, secure path.
        if self.tls || !self.config.tls_enabled {
            capabilities.push("AUTH PLAIN LOGIN");
        }

        // Frame the multiline reply: every line carries the "250-"
        // continuation prefix except the final one, which must use "250 "
        // (a space) to terminate the reply.
        let mut lines: Vec<String> = vec!["My capabilities are".into()];
        lines.extend(capabilities.into_iter().map(String::from));
        let count = lines.len();
        Reply::Lines(
            lines
                .into_iter()
                .enumerate()
                .map(|(index, line)| {
                    let separator = if index == count - 1 { " " } else { "-" };
                    format!("250{separator}{line}")
                })
                .collect(),
        )
    }

    fn helo(&mut self, data: &str) -> Reply {
        self.helo_name = data.trim().split_once(' ').map(|(_, rest)| rest.to_string());
        self.transaction_reset();
        self.state = State::Welcomed;
        Reply::Line(format!("250 {}", self.config.smtp_hostname))
    }

    fn rset(&mut self) -> Reply {
        self.transaction_reset();
        self.state = State::Welcomed;
        Reply::line("250 OK")
    }

    fn noop(&mut self) -> Reply {
        Reply::line("250 OK")
    }

    fn auth_plain(&mut self, data: &str) -> Reply {
        let rest = strip_prefix_ci(data, "AUTH PLAIN");
        let rest = rest.strip_prefix(' ').unwrap_or(rest);
        if rest.trim().is_empty() {
            self.input_mode = InputMode::AuthPlain;
            Reply::line("334")
        } else {
            let rest = rest.to_string();
            self.auth_plain_continue(&rest)
        }
    }

    fn auth_plain_continue(&mut self, data: &str) -> Reply {
        self.input_mode = InputMode::Command;
        let decoded = decode64_lenient(data);
        let decoded = String::from_utf8_lossy(&decoded);
        let parts: Vec<&str> = decoded.split('\0').collect();
        if parts.len() < 2 {
            return Reply::line("535 Authenticated failed - protocol error");
        }
        let password = parts[parts.len() - 1].to_string();
        self.authenticate(&password)
    }

    fn auth_login(&mut self, data: &str) -> Reply {
        let rest = strip_prefix_ci(data, "AUTH LOGIN");
        let rest = rest.strip_prefix(' ').unwrap_or(rest);
        if rest.trim().is_empty() {
            self.input_mode = InputMode::AuthLoginUsername;
            Reply::line("334 VXNlcm5hbWU6") // "Username:"
        } else {
            // A username was provided inline; we don't need it — ask for the
            // password next.
            self.auth_login_username()
        }
    }

    fn auth_login_username(&mut self) -> Reply {
        self.input_mode = InputMode::AuthLoginPassword;
        Reply::line("334 UGFzc3dvcmQ6") // "Password:"
    }

    fn auth_login_password(&mut self, data: &str) -> Reply {
        self.input_mode = InputMode::Command;
        let password = decode64_lenient(data);
        let password = String::from_utf8_lossy(&password).to_string();
        self.authenticate(&password)
    }

    fn authenticate(&mut self, password: &str) -> Reply {
        match self.store.find_smtp_credential_by_key(password) {
            Some(credential) => {
                self.store.record_credential_use(credential.id);
                let grant = self.grant_message(&credential);
                self.credential = Some(credential);
                Reply::Line(grant)
            }
            None => Reply::line("535 Invalid credential"),
        }
    }

    fn grant_message(&self, credential: &Credential) -> String {
        let server = self.store.server(credential.server_id);
        let organization =
            server.as_ref().and_then(|s| self.store.organization(s.organization_id));
        format!(
            "235 Granted for {}/{}",
            organization.map(|o| o.permalink).unwrap_or_default(),
            server.map(|s| s.permalink).unwrap_or_default()
        )
    }

    fn auth_cram_md5(&mut self) -> Reply {
        let challenge = format!(
            "<{}@{}>",
            camelmailer_core::token::generate_hex(20),
            self.config.smtp_hostname
        );
        let encoded = encode64(challenge.as_bytes());
        self.input_mode = InputMode::AuthCramMd5 { challenge };
        Reply::Line(format!("334 {encoded}"))
    }

    fn auth_cram_md5_continue(&mut self, data: &str, challenge: &str) -> Reply {
        use hmac::{Hmac, Mac};
        type HmacMd5 = Hmac<md5::Md5>;

        self.input_mode = InputMode::Command;
        let decoded = decode64_lenient(data);
        let decoded = String::from_utf8_lossy(&decoded);
        let mut split = decoded.splitn(2, ' ');
        let username = split.next().unwrap_or("").trim_end_matches(['\r', '\n']);
        let password = split.next().unwrap_or("").trim_end_matches(['\r', '\n']);

        let mut permalinks = username.splitn(2, ['/', '_']);
        let org_permalink = permalinks.next().unwrap_or("");
        let server_permalink = permalinks.next().unwrap_or("");
        let Some(server) = self
            .store
            .find_server_by_permalinks(org_permalink, server_permalink)
        else {
            return Reply::line("535 Denied");
        };

        for credential in self.store.smtp_credentials_for_server(server.id) {
            let mut mac = HmacMd5::new_from_slice(credential.key.as_bytes())
                .expect("HMAC accepts any key length");
            mac.update(challenge.as_bytes());
            let correct_response: String = mac
                .finalize()
                .into_bytes()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            if password == correct_response {
                self.store.record_credential_use(credential.id);
                let grant = self.grant_message(&credential);
                self.credential = Some(credential);
                return Reply::Line(grant);
            }
        }

        Reply::line("535 Denied")
    }

    fn mail_from_command(&mut self, data: &str) -> Reply {
        if !matches!(self.state, State::Welcomed | State::MailFromReceived) {
            return Reply::line("503 EHLO/HELO first please");
        }

        self.state = State::MailFromReceived;
        self.transaction_reset();

        // Discard the AUTH= parameter and anything after it; we don't trust
        // clients to set it.
        let line = match data.find("AUTH=") {
            Some(position) => data[..position].trim_end(),
            None => data,
        };
        self.mail_from = Some(extract_address(line, "MAIL FROM"));
        Reply::line("250 OK")
    }

    fn rcpt_to(&mut self, data: &str) -> Reply {
        if !matches!(self.state, State::MailFromReceived | State::RcptToReceived) {
            return Reply::line("503 EHLO/HELO and MAIL FROM first please");
        }

        let rcpt_to = extract_address(data, "RCPT TO");
        if rcpt_to.is_empty() {
            return Reply::line("501 RCPT TO should not be empty");
        }

        let (uname, domain) = match rcpt_to.split_once('@') {
            Some((u, d)) if !d.is_empty() => (u, d),
            _ => return Reply::line("501 Invalid RCPT TO"),
        };

        let (uname, tag) = match uname.split_once('+') {
            Some((u, t)) => (u, Some(t)),
            None => (uname, None),
        };

        let return_path_prefix = format!("{}.", self.config.custom_return_path_prefix);
        if domain == self.config.return_path_domain || domain.starts_with(&return_path_prefix) {
            // This is a return path
            self.state = State::RcptToReceived;
            match self.store.find_server_by_token(uname) {
                Some(server) if server.suspended => {
                    Reply::line("535 Mail server has been suspended")
                }
                Some(server) => {
                    self.recipients.push(Recipient {
                        kind: RecipientKind::Bounce,
                        rcpt_to,
                        server,
                        route: None,
                    });
                    Reply::line("250 OK")
                }
                None => Reply::line("550 Invalid server token"),
            }
        } else if domain == self.config.route_domain {
            // This is an email direct to a route
            self.state = State::RcptToReceived;
            match self.store.find_route_by_token(uname) {
                Some(resolved) if resolved.server.suspended => {
                    Reply::line("535 Mail server has been suspended")
                }
                Some(resolved) if resolved.route.mode == RouteMode::Reject => {
                    Reply::line("550 Route does not accept incoming messages")
                }
                Some(resolved) => {
                    let tag_suffix = tag.map(|t| format!("+{t}")).unwrap_or_default();
                    let actual_rcpt_to = format!(
                        "{}{}@{}",
                        resolved.route.name, tag_suffix, resolved.domain_name
                    );
                    self.recipients.push(Recipient {
                        kind: RecipientKind::Route,
                        rcpt_to: actual_rcpt_to,
                        server: resolved.server.clone(),
                        route: Some(resolved),
                    });
                    Reply::line("250 OK")
                }
                None => Reply::line("550 Invalid route token"),
            }
        } else if let Some(credential) = self.credential.clone() {
            // This is outgoing mail for an authenticated user
            self.state = State::RcptToReceived;
            match self.store.server(credential.server_id) {
                Some(server) if server.suspended => {
                    Reply::line("535 Mail server has been suspended")
                }
                Some(server) => {
                    self.recipients.push(Recipient {
                        kind: RecipientKind::Credential,
                        rcpt_to,
                        server,
                        route: None,
                    });
                    Reply::line("250 OK")
                }
                None => Reply::line("535 Mail server has been suspended"),
            }
        } else if let Some(resolved) = self.store.find_route_by_name_and_domain(uname, domain) {
            // This is incoming mail for a route
            self.state = State::RcptToReceived;
            if resolved.server.suspended {
                Reply::line("535 Mail server has been suspended")
            } else if resolved.route.mode == RouteMode::Reject {
                Reply::line("550 Route does not accept incoming messages")
            } else {
                self.recipients.push(Recipient {
                    kind: RecipientKind::Route,
                    rcpt_to,
                    server: resolved.server.clone(),
                    route: Some(resolved),
                });
                Reply::line("250 OK")
            }
        } else {
            // The client is trying to relay without authentication; try to
            // authenticate by IP address.
            let ip_credential = self
                .ip_address
                .as_deref()
                .and_then(|ip| ip.parse::<IpAddr>().ok())
                .and_then(|ip| self.store.find_ip_credential(ip));

            if let Some(credential) = ip_credential {
                self.store.record_credential_use(credential.id);
                self.credential = Some(credential);
                self.rcpt_to(data)
            } else {
                Reply::line("530 Authentication required")
            }
        }
    }

    fn data_command(&mut self) -> Reply {
        if self.state != State::RcptToReceived {
            return Reply::line("503 HELO/EHLO, MAIL FROM and RCPT TO before sending data");
        }

        let mut data: Vec<u8> = Vec::new();
        let mut headers: HashMap<String, Vec<String>> = HashMap::new();
        self.receiving_headers = true;
        self.header_key = None;

        let credential_server = self
            .credential
            .as_ref()
            .and_then(|c| self.store.server(c.server_id));
        let privacy_mode = credential_server
            .as_ref()
            .map(|s| s.privacy_mode)
            .unwrap_or(false);
        let ip = self.ip_address.clone().unwrap_or_default();
        let resolved_hostname = self.resolved_hostname.clone().unwrap_or_else(|| ip.clone());
        let received_header = received_header::generate(
            privacy_mode,
            self.helo_name.as_deref().unwrap_or_default(),
            &resolved_hostname,
            &ip,
            ReceiveMethod::Smtp,
            &self.config.smtp_hostname,
            (self.clock)(),
        );

        let envelope_header = format!("<{}>", self.mail_from.as_deref().unwrap_or_default());
        data.extend_from_slice(format!("X-Envelope-From: {envelope_header}\r\n").as_bytes());
        headers.insert("x-envelope-from".into(), vec![envelope_header]);

        data.extend_from_slice(format!("Received: {received_header}\r\n").as_bytes());
        headers.insert("received".into(), vec![received_header]);

        self.data = Some(data);
        self.headers = Some(headers);
        self.input_mode = InputMode::Data;
        Reply::line("354 Go ahead")
    }

    fn data_line(&mut self, line: &str) -> Reply {
        if line == "." && self.cr_present && self.previous_cr_present == Some(true) {
            self.input_mode = InputMode::Command;
            return self.finish_data();
        }

        // Dot-stuffing: a leading ".." becomes "."
        let line: std::borrow::Cow<'_, str> = if let Some(stripped) = line.strip_prefix("..") {
            std::borrow::Cow::Owned(format!(".{stripped}"))
        } else {
            std::borrow::Cow::Borrowed(line)
        };
        let line: &str = &line;

        if self.receiving_headers {
            if line.is_empty() {
                self.receiving_headers = false;
            } else if line.starts_with(char::is_whitespace) {
                // Continuation of the previous header
                if let (Some(key), Some(headers)) = (&self.header_key, self.headers.as_mut()) {
                    if let Some(values) = headers.get_mut(&key.to_lowercase()) {
                        if let Some(last) = values.last_mut() {
                            last.push_str(line);
                        }
                    }
                }
            } else {
                let (key, value) = match split_header(line) {
                    Some((k, v)) => (k, v),
                    None => (line.to_string(), String::new()),
                };
                self.header_key = Some(key.clone());
                self.headers
                    .as_mut()
                    .expect("headers exist while receiving data")
                    .entry(key.to_lowercase())
                    .or_default()
                    .push(value);
            }
        }

        if let Some(data) = self.data.as_mut() {
            data.extend_from_slice(line.as_bytes());
            data.extend_from_slice(b"\r\n");
        }
        Reply::None
    }

    fn finish_data(&mut self) -> Reply {
        let data = self.data.clone().unwrap_or_default();
        let headers = self.headers.clone().unwrap_or_default();

        if data.len() as u64 > self.config.max_message_size * 1024 * 1024 {
            self.transaction_reset();
            self.state = State::Welcomed;
            return Reply::Line(format!(
                "552 Message too large (maximum size {}MB)",
                self.config.max_message_size
            ));
        }

        let loop_marker = format!("by {}", self.config.smtp_hostname);
        let received_by_us = headers
            .get("received")
            .map(|values| values.iter().filter(|v| v.contains(&loop_marker)).count())
            .unwrap_or(0);
        if received_by_us > 4 {
            self.transaction_reset();
            self.state = State::Welcomed;
            return Reply::line("550 Loop detected");
        }

        let mut authenticated_domain_id = None;
        if let Some(credential) = &self.credential {
            let server = self.store.server(credential.server_id);
            authenticated_domain_id =
                self.find_authenticated_domain(credential.server_id, &headers, server.as_ref());
            if authenticated_domain_id.is_none() {
                self.transaction_reset();
                self.state = State::Welcomed;
                return Reply::line("530 From/Sender name is not valid");
            }
        }

        let mail_from = self.mail_from.clone().unwrap_or_default();
        for recipient in std::mem::take(&mut self.recipients) {
            match recipient.kind {
                RecipientKind::Credential => {
                    // Outgoing messages are just inserted
                    self.sink.queue_message(QueuedMessage {
                        server_id: recipient.server.id,
                        rcpt_to: recipient.rcpt_to,
                        mail_from: mail_from.clone(),
                        raw_message: data.clone(),
                        received_with_ssl: self.tls,
                        scope: MessageScope::Outgoing,
                        bounce: false,
                        domain_id: authenticated_domain_id,
                        credential_id: self.credential.as_ref().map(|c| c.id),
                        route_id: None,
            tag: None,
            metadata: None,
                    });
                }
                RecipientKind::Bounce => {
                    match self.store.return_path_route_for_server(recipient.server.id) {
                        Some(rp_route) => {
                            // Deliver through the return path route
                            self.sink.queue_message(QueuedMessage {
                                server_id: recipient.server.id,
                                rcpt_to: recipient.rcpt_to,
                                mail_from: mail_from.clone(),
                                raw_message: data.clone(),
                                received_with_ssl: self.tls,
                                scope: MessageScope::Incoming,
                                bounce: true,
                                domain_id: rp_route.route.domain_id,
                                credential_id: None,
                                route_id: Some(rp_route.route.id),
            tag: None,
            metadata: None,
                            });
                        }
                        None => {
                            // No return path route; insert the message
                            // without going through a route.
                            self.sink.queue_message(QueuedMessage {
                                server_id: recipient.server.id,
                                rcpt_to: recipient.rcpt_to,
                                mail_from: mail_from.clone(),
                                raw_message: data.clone(),
                                received_with_ssl: self.tls,
                                scope: MessageScope::Incoming,
                                bounce: true,
                                domain_id: None,
                                credential_id: None,
                                route_id: None,
            tag: None,
            metadata: None,
                            });
                        }
                    }
                }
                RecipientKind::Route => {
                    let route = recipient.route.as_ref().expect("route recipients carry a route");
                    self.sink.queue_message(QueuedMessage {
                        server_id: recipient.server.id,
                        rcpt_to: recipient.rcpt_to,
                        mail_from: mail_from.clone(),
                        raw_message: data.clone(),
                        received_with_ssl: self.tls,
                        scope: MessageScope::Incoming,
                        bounce: false,
                        domain_id: route.route.domain_id,
                        credential_id: None,
                        route_id: Some(route.route.id),
            tag: None,
            metadata: None,
                    });
                }
            }
        }

        self.transaction_reset();
        self.state = State::Welcomed;
        Reply::line("250 OK")
    }

    fn find_authenticated_domain(
        &self,
        server_id: camelmailer_core::Id,
        headers: &HashMap<String, Vec<String>>,
        server: Option<&Server>,
    ) -> Option<camelmailer_core::Id> {
        let mut headers_to_check = vec!["from"];
        if server.map(|s| s.allow_sender).unwrap_or(false) {
            headers_to_check.push("sender");
        }
        for header_name in headers_to_check {
            let values: Vec<&str> = headers
                .get(header_name)
                .map(|v| v.iter().map(String::as_str).collect())
                .unwrap_or_default();
            if let Some(domain_id) = self.store.find_authenticated_domain(server_id, &values) {
                return Some(domain_id);
            }
        }
        None
    }
}

/// Strip a case-insensitive command prefix (e.g. "AUTH PLAIN") from a line.
fn strip_prefix_ci<'a>(data: &'a str, prefix: &str) -> &'a str {
    if data.len() >= prefix.len() && data[..prefix.len()].eq_ignore_ascii_case(prefix) {
        &data[prefix.len()..]
    } else {
        data
    }
}

/// Extract the address from a `MAIL FROM:`/`RCPT TO:` line, mirroring the
/// Ruby gsub chain: remove the command prefix (with optional whitespace
/// around the colon), then everything up to the last `<` and from the first
/// `>` onwards, then trim.
fn extract_address(data: &str, command: &str) -> String {
    let mut rest = strip_prefix_ci(data, command).to_string();
    // strip optional whitespace, colon, whitespace
    let trimmed = rest.trim_start();
    let trimmed = trimmed.strip_prefix(':').unwrap_or(trimmed);
    rest = trimmed.trim_start().to_string();

    if let Some(position) = rest.rfind('<') {
        rest = rest[position + 1..].to_string();
    }
    if let Some(position) = rest.find('>') {
        rest = rest[..position].to_string();
    }
    rest.trim().to_string()
}

/// Split a header line on the first colon plus following whitespace
/// (Ruby: `split(/:\s*/, 2)`).
fn split_header(line: &str) -> Option<(String, String)> {
    let position = line.find(':')?;
    let key = line[..position].to_string();
    let value = line[position + 1..].trim_start().to_string();
    Some((key, value))
}

#[cfg(test)]
mod helper_tests {
    use super::*;

    #[test]
    fn extract_address_handles_all_forms() {
        assert_eq!(
            extract_address("MAIL FROM: test@example.com", "MAIL FROM"),
            "test@example.com"
        );
        assert_eq!(
            extract_address("MAIL FROM:<test@example.com>", "MAIL FROM"),
            "test@example.com"
        );
        assert_eq!(
            extract_address("RCPT TO: <Name> <test@example.com> junk", "RCPT TO"),
            "test@example.com"
        );
        assert_eq!(extract_address("RCPT TO: ", "RCPT TO"), "");
        assert_eq!(extract_address("rcpt to:x@y.com", "RCPT TO"), "x@y.com");
    }

    #[test]
    fn decode64_is_lenient_like_ruby() {
        assert_eq!(decode64_lenient("aGVsbG8="), b"hello");
        assert_eq!(decode64_lenient("aGVs\nbG8=\n"), b"hello");
        assert_eq!(decode64_lenient("aGVsbG8"), b"hello");
        assert_eq!(decode64_lenient(""), b"");
    }

    #[test]
    fn split_header_matches_ruby_semantics() {
        assert_eq!(
            split_header("Subject: Test"),
            Some(("Subject".into(), "Test".into()))
        );
        assert_eq!(
            split_header("X-Thing:no-space"),
            Some(("X-Thing".into(), "no-space".into()))
        );
        assert_eq!(split_header("no colon here"), None);
    }
}
