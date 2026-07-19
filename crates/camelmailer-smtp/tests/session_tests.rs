//! Port of the Ruby SMTP client specs (`spec/lib/smtp_server/client/*.rb`).

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use camelmailer_core::testing::Fixtures;
use camelmailer_core::{CredentialType, MemorySink, MessageScope, RouteMode};
use camelmailer_smtp::{RecipientKind, Reply, Session, SessionConfig, State};
use chrono::{TimeZone, Utc};
use std::sync::Arc;

const SMTP_HOSTNAME: &str = "postal.example.com";
const RETURN_PATH_DOMAIN: &str = "rp.postal.example.com";
const ROUTE_DOMAIN: &str = "routes.postal.example.com";

fn config() -> SessionConfig {
    SessionConfig {
        smtp_hostname: SMTP_HOSTNAME.into(),
        tls_enabled: false,
        max_message_size: 14,
        return_path_domain: RETURN_PATH_DOMAIN.into(),
        custom_return_path_prefix: "psrp".into(),
        route_domain: ROUTE_DOMAIN.into(),
    }
}

struct TestSetup {
    fixtures: Fixtures,
    sink: Arc<MemorySink>,
    session: Session,
}

impl TestSetup {
    fn new() -> Self {
        Self::with_config_and_ip(config(), Some("1.2.3.4"))
    }

    fn with_ip(ip: Option<&str>) -> Self {
        Self::with_config_and_ip(config(), ip)
    }

    fn with_config(config: SessionConfig) -> Self {
        Self::with_config_and_ip(config, Some("1.2.3.4"))
    }

    fn with_config_and_ip(config: SessionConfig, ip: Option<&str>) -> Self {
        let fixtures = Fixtures::new();
        let sink = Arc::new(MemorySink::new());
        let mut session =
            Session::new(config, fixtures.store(), sink.clone(), ip.map(String::from));
        // Freeze time, mirroring Timecop.freeze in the Ruby specs.
        session.set_clock(Arc::new(|| {
            Utc.with_ymd_and_hms(2026, 7, 9, 12, 34, 56).unwrap()
        }));
        Self {
            fixtures,
            sink,
            session,
        }
    }
}

fn line(reply: &Reply) -> &str {
    match reply {
        Reply::Line(text) => text,
        other => panic!("expected single-line reply, got {other:?}"),
    }
}

/// `Credential#to_smtp_plain`
fn to_smtp_plain(key: &str) -> String {
    BASE64.encode(format!("\0XX\0{key}"))
}

fn frozen_rfc2822() -> &'static str {
    "Thu, 09 Jul 2026 12:34:56 +0000"
}

// ---------------------------------------------------------------- proxy_spec

#[test]
fn proxy_sets_the_ip_address_when_the_header_is_valid() {
    let mut setup = TestSetup::with_ip(None);
    let reply = setup.session.handle("PROXY TCP4 1.1.1.1 2.2.2.2 1111 2222");
    assert_eq!(
        line(&reply),
        format!(
            "220 {SMTP_HOSTNAME} ESMTP CamelMailer/{}",
            setup.session.trace_id()
        )
    );
    assert_eq!(setup.session.ip_address(), Some("1.1.1.1"));
}

#[test]
fn proxy_returns_an_error_when_the_header_is_invalid() {
    let mut setup = TestSetup::with_ip(None);
    let reply = setup.session.handle("PROXY TCP4");
    assert_eq!(line(&reply), "502 Proxy Error");
    assert!(setup.session.finished());
}

// ----------------------------------------------------------------- helo_spec

#[test]
fn helo_returns_the_hostname() {
    let mut setup = TestSetup::new();
    assert_eq!(setup.session.state(), State::Welcome);
    let reply = setup.session.handle("HELO: test.example.com");
    assert_eq!(line(&reply), format!("250 {SMTP_HOSTNAME}"));
    assert_eq!(setup.session.state(), State::Welcomed);
}

#[test]
fn ehlo_advertises_auth_plain_login_when_tls_is_disabled() {
    let mut setup = TestSetup::new();
    let reply = setup.session.handle("EHLO test.example.com");
    assert_eq!(
        reply,
        Reply::Lines(vec![
            "250-My capabilities are".into(),
            "250 AUTH PLAIN LOGIN".into(),
        ])
    );
}

#[test]
fn ehlo_offers_starttls_and_withholds_auth_before_tls_upgrade() {
    let mut tls_config = config();
    tls_config.tls_enabled = true;
    let mut setup = TestSetup::with_config(tls_config);
    let reply = setup.session.handle("EHLO test.example.com");
    assert_eq!(
        reply,
        Reply::Lines(vec![
            "250-My capabilities are".into(),
            "250 STARTTLS".into(),
        ])
    );
}

#[test]
fn ehlo_advertises_auth_and_no_starttls_once_tls_protected() {
    let mut tls_config = config();
    tls_config.tls_enabled = true;
    let mut setup = TestSetup::with_config(tls_config);
    setup.session.set_tls(true);
    let reply = setup.session.handle("EHLO test.example.com");
    assert_eq!(
        reply,
        Reply::Lines(vec![
            "250-My capabilities are".into(),
            "250 AUTH PLAIN LOGIN".into(),
        ])
    );
}

#[test]
fn ehlo_reply_is_always_terminated_with_a_space_separated_line() {
    let scenarios = [(false, false), (true, false), (true, true)];
    for (tls_enabled, tls_upgraded) in scenarios {
        let mut scenario_config = config();
        scenario_config.tls_enabled = tls_enabled;
        let mut setup = TestSetup::with_config(scenario_config);
        if tls_upgraded {
            setup.session.set_tls(true);
        }
        let reply = setup.session.handle("EHLO test.example.com");
        let Reply::Lines(lines) = reply else {
            panic!("expected multiline reply");
        };
        assert!(
            lines.last().unwrap().starts_with("250 "),
            "final line must use '250 ' (tls_enabled={tls_enabled}, upgraded={tls_upgraded})"
        );
        for prefix_line in &lines[..lines.len() - 1] {
            assert!(prefix_line.starts_with("250-"));
        }
    }
}

// ------------------------------------------------------------ mail_from_spec

#[test]
fn mail_from_requires_helo_first() {
    let mut setup = TestSetup::new();
    let reply = setup.session.handle("MAIL FROM: test@example.com");
    assert_eq!(line(&reply), "503 EHLO/HELO first please");
    assert_eq!(setup.session.state(), State::Welcome);
}

#[test]
fn mail_from_resets_the_transaction_when_called_again() {
    let mut setup = TestSetup::new();
    setup.session.handle("HELO test.example.com");
    setup.session.handle("MAIL FROM: test@example.com");
    setup.session.handle("MAIL FROM: test2@example.com");
    assert_eq!(setup.session.mail_from(), Some("test2@example.com"));
}

#[test]
fn mail_from_sets_the_address() {
    let mut setup = TestSetup::new();
    setup.session.handle("HELO test.example.com");
    let reply = setup.session.handle("MAIL FROM: test@example.com");
    assert_eq!(line(&reply), "250 OK");
    assert_eq!(setup.session.state(), State::MailFromReceived);
    assert_eq!(setup.session.mail_from(), Some("test@example.com"));
}

#[test]
fn mail_from_discards_the_auth_parameter() {
    let mut setup = TestSetup::new();
    setup.session.handle("HELO test.example.com");
    setup.session.handle("MAIL FROM:<test@example.com> AUTH=<>");
    assert_eq!(setup.session.mail_from(), Some("test@example.com"));
}

// -------------------------------------------------------------- rcpt_to_spec

fn helo_and_mail_from(session: &mut Session) {
    session.handle("HELO test.example.com");
    session.handle("MAIL FROM: test@example.com");
}

#[test]
fn rcpt_to_requires_mail_from_first() {
    let mut setup = TestSetup::new();
    setup.session.handle("HELO test.example.com");
    let reply = setup.session.handle("RCPT TO: no-route-here@internal.com");
    assert_eq!(line(&reply), "503 EHLO/HELO and MAIL FROM first please");
    assert_eq!(setup.session.state(), State::Welcomed);
}

#[test]
fn rcpt_to_rejects_invalid_addresses() {
    let mut setup = TestSetup::new();
    helo_and_mail_from(&mut setup.session);
    let reply = setup.session.handle("RCPT TO: blah");
    assert_eq!(line(&reply), "501 Invalid RCPT TO");
}

#[test]
fn rcpt_to_rejects_empty_addresses() {
    let mut setup = TestSetup::new();
    helo_and_mail_from(&mut setup.session);
    let reply = setup.session.handle("RCPT TO: ");
    assert_eq!(line(&reply), "501 RCPT TO should not be empty");
}

#[test]
fn return_path_rcpt_with_unknown_server_token_is_rejected() {
    let mut setup = TestSetup::new();
    helo_and_mail_from(&mut setup.session);
    let reply = setup
        .session
        .handle(&format!("RCPT TO: nothing@{RETURN_PATH_DOMAIN}"));
    assert_eq!(line(&reply), "550 Invalid server token");
}

#[test]
fn return_path_rcpt_for_suspended_server_is_rejected() {
    let mut setup = TestSetup::new();
    setup.fixtures.suspend_server();
    let token = setup.fixtures.server().token.clone();
    helo_and_mail_from(&mut setup.session);
    let reply = setup
        .session
        .handle(&format!("RCPT TO: {token}@{RETURN_PATH_DOMAIN}"));
    assert_eq!(line(&reply), "535 Mail server has been suspended");
}

#[test]
fn return_path_rcpt_adds_a_bounce_recipient() {
    let mut setup = TestSetup::new();
    let token = setup.fixtures.server().token.clone();
    helo_and_mail_from(&mut setup.session);
    let address = format!("{token}@{RETURN_PATH_DOMAIN}");
    let reply = setup.session.handle(&format!("RCPT TO: {address}"));
    assert_eq!(line(&reply), "250 OK");
    assert_eq!(setup.session.recipients().len(), 1);
    let recipient = &setup.session.recipients()[0];
    assert_eq!(recipient.kind, RecipientKind::Bounce);
    assert_eq!(recipient.rcpt_to, address);
    assert_eq!(recipient.server.id, setup.fixtures.server_id());
    assert_eq!(setup.session.state(), State::RcptToReceived);
}

#[test]
fn custom_return_path_prefix_behaves_like_the_return_path_domain() {
    let mut setup = TestSetup::new();
    let token = setup.fixtures.server().token.clone();
    helo_and_mail_from(&mut setup.session);

    let reply = setup.session.handle("RCPT TO: nothing@psrp.example.com");
    assert_eq!(line(&reply), "550 Invalid server token");

    let address = format!("{token}@psrp.example.com");
    let reply = setup.session.handle(&format!("RCPT TO: {address}"));
    assert_eq!(line(&reply), "250 OK");
    let recipient = &setup.session.recipients()[0];
    assert_eq!(recipient.kind, RecipientKind::Bounce);
    assert_eq!(recipient.rcpt_to, address);
}

#[test]
fn route_domain_rcpt_with_invalid_token_is_rejected() {
    let mut setup = TestSetup::new();
    helo_and_mail_from(&mut setup.session);
    let reply = setup
        .session
        .handle(&format!("RCPT TO: nothing@{ROUTE_DOMAIN}"));
    assert_eq!(line(&reply), "550 Invalid route token");
}

#[test]
fn route_domain_rcpt_for_suspended_server_is_rejected() {
    let mut setup = TestSetup::new();
    setup.fixtures.suspend_server();
    let domain = setup.fixtures.verified_server_domain("example.com");
    let route = setup
        .fixtures
        .route("info", Some(domain.id), RouteMode::Endpoint);
    helo_and_mail_from(&mut setup.session);
    let reply = setup
        .session
        .handle(&format!("RCPT TO: {}@{ROUTE_DOMAIN}", route.token));
    assert_eq!(line(&reply), "535 Mail server has been suspended");
}

#[test]
fn route_domain_rcpt_for_reject_route_is_rejected() {
    let mut setup = TestSetup::new();
    let domain = setup.fixtures.verified_server_domain("example.com");
    let route = setup
        .fixtures
        .route("info", Some(domain.id), RouteMode::Reject);
    helo_and_mail_from(&mut setup.session);
    let reply = setup
        .session
        .handle(&format!("RCPT TO: {}@{ROUTE_DOMAIN}", route.token));
    assert_eq!(line(&reply), "550 Route does not accept incoming messages");
}

#[test]
fn route_domain_rcpt_resolves_to_the_routes_real_address_with_tag() {
    let mut setup = TestSetup::new();
    let domain = setup.fixtures.verified_server_domain("example.com");
    let route = setup
        .fixtures
        .route("info", Some(domain.id), RouteMode::Endpoint);
    helo_and_mail_from(&mut setup.session);
    let reply = setup
        .session
        .handle(&format!("RCPT TO: {}+tag1@{ROUTE_DOMAIN}", route.token));
    assert_eq!(line(&reply), "250 OK");
    let recipient = &setup.session.recipients()[0];
    assert_eq!(recipient.kind, RecipientKind::Route);
    assert_eq!(recipient.rcpt_to, "info+tag1@example.com");
    assert_eq!(recipient.route.as_ref().unwrap().route.id, route.id);
    assert_eq!(setup.session.state(), State::RcptToReceived);
}

#[test]
fn authenticated_rcpt_for_suspended_server_is_rejected() {
    let mut setup = TestSetup::new();
    setup.fixtures.suspend_server();
    let credential = setup.fixtures.credential(CredentialType::Smtp, "key123");
    helo_and_mail_from(&mut setup.session);
    let reply = setup
        .session
        .handle(&format!("AUTH PLAIN {}", to_smtp_plain(&credential.key)));
    assert!(line(&reply).starts_with("235 Granted for "));
    let reply = setup.session.handle("RCPT TO: outgoing@example.com");
    assert_eq!(line(&reply), "535 Mail server has been suspended");
}

#[test]
fn authenticated_rcpt_adds_a_credential_recipient() {
    let mut setup = TestSetup::new();
    let credential = setup.fixtures.credential(CredentialType::Smtp, "key123");
    helo_and_mail_from(&mut setup.session);
    let reply = setup
        .session
        .handle(&format!("AUTH PLAIN {}", to_smtp_plain(&credential.key)));
    assert!(line(&reply).starts_with("235 Granted for "));
    let reply = setup.session.handle("RCPT TO: outgoing@example.com");
    assert_eq!(line(&reply), "250 OK");
    let recipient = &setup.session.recipients()[0];
    assert_eq!(recipient.kind, RecipientKind::Credential);
    assert_eq!(recipient.rcpt_to, "outgoing@example.com");
    assert_eq!(setup.session.state(), State::RcptToReceived);
}

#[test]
fn unauthenticated_rcpt_matching_a_route_adds_a_route_recipient() {
    let mut setup = TestSetup::new();
    let domain = setup.fixtures.verified_server_domain("example.com");
    let route = setup
        .fixtures
        .route("info", Some(domain.id), RouteMode::Endpoint);
    helo_and_mail_from(&mut setup.session);
    let reply = setup.session.handle("RCPT TO: info@example.com");
    assert_eq!(line(&reply), "250 OK");
    let recipient = &setup.session.recipients()[0];
    assert_eq!(recipient.kind, RecipientKind::Route);
    assert_eq!(recipient.rcpt_to, "info@example.com");
    assert_eq!(recipient.route.as_ref().unwrap().route.id, route.id);
}

#[test]
fn unauthenticated_rcpt_matching_a_reject_route_is_rejected() {
    let mut setup = TestSetup::new();
    let domain = setup.fixtures.verified_server_domain("example.com");
    setup
        .fixtures
        .route("info", Some(domain.id), RouteMode::Reject);
    helo_and_mail_from(&mut setup.session);
    let reply = setup.session.handle("RCPT TO: info@example.com");
    assert_eq!(line(&reply), "550 Route does not accept incoming messages");
}

#[test]
fn unauthenticated_rcpt_without_route_requires_authentication() {
    let mut setup = TestSetup::new();
    helo_and_mail_from(&mut setup.session);
    let reply = setup.session.handle("RCPT TO: nothing@nothing.com");
    assert_eq!(line(&reply), "530 Authentication required");
}

#[test]
fn unauthenticated_rcpt_falls_back_to_ip_credentials() {
    let mut setup = TestSetup::new();
    setup
        .fixtures
        .credential(CredentialType::SmtpIp, "1.0.0.0/8");
    helo_and_mail_from(&mut setup.session);
    let reply = setup.session.handle("RCPT TO: test@example.com");
    assert_eq!(line(&reply), "250 OK");
    let recipient = &setup.session.recipients()[0];
    assert_eq!(recipient.kind, RecipientKind::Credential);
    assert_eq!(recipient.rcpt_to, "test@example.com");
    assert_eq!(setup.session.state(), State::RcptToReceived);
}

// ----------------------------------------------------------------- auth_spec

#[test]
fn auth_plain_without_initial_data_returns_334_and_accepts_credentials_next() {
    let mut setup = TestSetup::new();
    setup.session.handle("HELO test.example.com");
    let reply = setup.session.handle("AUTH PLAIN");
    assert_eq!(line(&reply), "334");

    let credential = setup.fixtures.credential(CredentialType::Smtp, "key123");
    let reply = setup.session.handle(&to_smtp_plain(&credential.key));
    assert!(line(&reply).starts_with("235 Granted for"));
}

#[test]
fn auth_plain_with_inline_credentials_authenticates() {
    let mut setup = TestSetup::new();
    setup.session.handle("HELO test.example.com");
    let credential = setup.fixtures.credential(CredentialType::Smtp, "key123");
    let reply = setup
        .session
        .handle(&format!("AUTH PLAIN {}", to_smtp_plain(&credential.key)));
    assert!(line(&reply).starts_with("235 Granted for"));
    assert_eq!(setup.session.credential().unwrap().id, credential.id);
}

#[test]
fn auth_plain_with_invalid_credentials_is_rejected() {
    let mut setup = TestSetup::new();
    setup.session.handle("HELO test.example.com");
    let encoded = BASE64.encode("user\0pass");
    let reply = setup.session.handle(&format!("AUTH PLAIN {encoded}"));
    assert_eq!(line(&reply), "535 Invalid credential");
    assert_eq!(setup.session.state(), State::Welcomed);
}

#[test]
fn auth_plain_with_missing_username_or_password_is_a_protocol_error() {
    let mut setup = TestSetup::new();
    setup.session.handle("HELO test.example.com");
    let encoded = BASE64.encode("pass");
    let reply = setup.session.handle(&format!("AUTH PLAIN {encoded}"));
    assert_eq!(line(&reply), "535 Authenticated failed - protocol error");
    assert_eq!(setup.session.state(), State::Welcomed);
}

#[test]
fn auth_login_requests_username_then_password() {
    let mut setup = TestSetup::new();
    setup.session.handle("HELO test.example.com");
    let reply = setup.session.handle("AUTH LOGIN");
    assert_eq!(line(&reply), "334 VXNlcm5hbWU6");
    let reply = setup.session.handle("xx");
    assert_eq!(line(&reply), "334 UGFzc3dvcmQ6");

    let credential = setup.fixtures.credential(CredentialType::Smtp, "key123");
    let password = BASE64.encode(&credential.key);
    let reply = setup.session.handle(&password);
    assert!(line(&reply).starts_with("235 Granted for"));
}

#[test]
fn auth_login_rejects_invalid_credentials() {
    let mut setup = TestSetup::new();
    setup.session.handle("HELO test.example.com");
    setup.session.handle("AUTH LOGIN");
    setup.session.handle("xx");
    let password = BASE64.encode("xx");
    let reply = setup.session.handle(&password);
    assert_eq!(line(&reply), "535 Invalid credential");
}

#[test]
fn auth_login_with_inline_username_requests_password_immediately() {
    let mut setup = TestSetup::new();
    setup.session.handle("HELO test.example.com");
    let credential = setup.fixtures.credential(CredentialType::Smtp, "key123");
    let username = BASE64.encode("xx");
    let reply = setup.session.handle(&format!("AUTH LOGIN {username}"));
    assert_eq!(line(&reply), "334 UGFzc3dvcmQ6");
    let password = BASE64.encode(&credential.key);
    let reply = setup.session.handle(&password);
    assert!(line(&reply).starts_with("235 Granted for"));
    assert_eq!(setup.session.credential().unwrap().id, credential.id);
}

#[test]
fn auth_cram_md5_authenticates_with_a_valid_digest() {
    use hmac::{Hmac, Mac};

    let mut setup = TestSetup::new();
    setup.session.handle("HELO test.example.com");
    let credential = setup.fixtures.credential(CredentialType::Smtp, "key123");

    let reply = setup.session.handle("AUTH CRAM-MD5");
    let text = line(&reply).to_string();
    assert!(text.starts_with("334 "));
    let challenge = BASE64.decode(text.split(' ').nth(1).unwrap()).unwrap();

    let mut mac = Hmac::<md5::Md5>::new_from_slice(credential.key.as_bytes()).unwrap();
    mac.update(&challenge);
    let digest: String = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();

    let organization = setup.fixtures.organization().permalink.clone();
    let server = setup.fixtures.server().permalink.clone();
    let response = BASE64.encode(format!("{organization}/{server} {digest}"));
    let reply = setup.session.handle(&response);
    assert!(line(&reply).starts_with("235 Granted for"));
    assert_eq!(setup.session.credential().unwrap().id, credential.id);
}

#[test]
fn auth_cram_md5_rejects_unknown_org_server() {
    let mut setup = TestSetup::new();
    setup.session.handle("HELO test.example.com");
    setup.session.handle("AUTH CRAM-MD5");
    let response = BASE64.encode("org/server password");
    let reply = setup.session.handle(&response);
    assert_eq!(line(&reply), "535 Denied");
}

#[test]
fn auth_cram_md5_rejects_invalid_digests() {
    let mut setup = TestSetup::new();
    setup.session.handle("HELO test.example.com");
    setup.fixtures.credential(CredentialType::Smtp, "key123");
    setup.session.handle("AUTH CRAM-MD5");
    let organization = setup.fixtures.organization().permalink.clone();
    let server = setup.fixtures.server().permalink.clone();
    let response = BASE64.encode(format!("{organization}/{server} invalid-password"));
    let reply = setup.session.handle(&response);
    assert_eq!(line(&reply), "535 Denied");
}

// ----------------------------------------------------------------- data_spec

#[test]
fn data_requires_the_full_transaction_first() {
    let mut setup = TestSetup::new();
    let expected = "503 HELO/EHLO, MAIL FROM and RCPT TO before sending data";

    let reply = setup.session.handle("DATA");
    assert_eq!(line(&reply), expected);

    setup.session.handle("HELO test.example.com");
    let reply = setup.session.handle("DATA");
    assert_eq!(line(&reply), expected);

    setup.session.handle("MAIL FROM: test@example.com");
    let reply = setup.session.handle("DATA");
    assert_eq!(line(&reply), expected);
}

fn setup_with_route() -> TestSetup {
    let setup = TestSetup::new();
    let domain = setup.fixtures.verified_server_domain("example.com");
    setup
        .fixtures
        .route("info", Some(domain.id), RouteMode::Endpoint);
    setup
}

fn begin_data_transaction(session: &mut Session) {
    session.handle("HELO test.example.com");
    session.handle("MAIL FROM: test@test.com");
    session.handle("RCPT TO: info@example.com");
}

#[test]
fn data_returns_go_ahead() {
    let mut setup = setup_with_route();
    begin_data_transaction(&mut setup.session);
    let reply = setup.session.handle("DATA");
    assert_eq!(line(&reply), "354 Go ahead");
}

#[test]
fn data_adds_a_received_header_for_itself() {
    let mut setup = setup_with_route();
    begin_data_transaction(&mut setup.session);
    setup.session.handle("DATA");
    let headers = setup.session.headers().unwrap();
    assert!(headers["received"].contains(&format!(
        "from test.example.com (1.2.3.4 [1.2.3.4]) by {SMTP_HOSTNAME} with SMTP; {}",
        frozen_rfc2822()
    )));
}

#[test]
fn data_prepends_the_received_spf_header_when_the_server_set_one() {
    let mut setup = setup_with_route();
    begin_data_transaction(&mut setup.session);
    // The async server layer evaluates SPF and hands the value in before DATA.
    setup.session.set_received_spf(
        "pass (postal.example.com: domain of test@test.com designates 1.2.3.4 as permitted sender) client-ip=1.2.3.4; envelope-from=<test@test.com>;".into(),
    );
    setup.session.handle("DATA");
    let headers = setup.session.headers().unwrap();
    assert!(headers.contains_key("received-spf"));
    assert!(headers["received-spf"][0].starts_with("pass ("));
    // The header lands in the stored raw, above the trace headers.
    let raw = String::from_utf8_lossy(setup.session.raw_data().unwrap());
    assert!(raw.contains("Received-SPF: pass ("));
    let spf_at = raw.find("Received-SPF:").unwrap();
    let received_at = raw.find("Received:").unwrap();
    assert!(spf_at < received_at, "Received-SPF should precede Received");
}

#[test]
fn data_has_no_received_spf_header_by_default() {
    let mut setup = setup_with_route();
    begin_data_transaction(&mut setup.session);
    setup.session.handle("DATA");
    assert!(!setup
        .session
        .headers()
        .unwrap()
        .contains_key("received-spf"));
    let raw = String::from_utf8_lossy(setup.session.raw_data().unwrap());
    assert!(!raw.contains("Received-SPF:"));
}

#[test]
fn data_logs_headers_including_multiline_continuations() {
    let mut setup = setup_with_route();
    begin_data_transaction(&mut setup.session);
    setup.session.handle("DATA");
    setup.session.handle("Subject: Test");
    setup.session.handle("From: test@test.com");
    setup.session.handle("To: test1@example.com");
    setup.session.handle("To: test2@example.com");
    setup.session.handle("X-Something: abcdef1234");
    setup.session.handle("X-Multiline: 1234");
    setup.session.handle("             4567");
    let headers = setup.session.headers().unwrap();
    assert_eq!(headers["subject"], vec!["Test"]);
    assert_eq!(headers["from"], vec!["test@test.com"]);
    assert_eq!(
        headers["to"],
        vec!["test1@example.com", "test2@example.com"]
    );
    assert_eq!(headers["x-something"], vec!["abcdef1234"]);
    assert_eq!(headers["x-multiline"], vec!["1234             4567"]);
}

#[test]
fn data_accumulates_raw_content_with_crlf_line_endings() {
    let mut setup = setup_with_route();
    begin_data_transaction(&mut setup.session);
    setup.session.handle("DATA");
    setup.session.handle("Subject: Test");
    setup.session.handle("");
    setup
        .session
        .handle("This is some content for the message.");
    setup.session.handle("It will keep going.");
    let expected = format!(
        "X-Envelope-From: <test@test.com>\r\n\
         Received: from test.example.com (1.2.3.4 [1.2.3.4]) by {SMTP_HOSTNAME} with SMTP; {}\r\n\
         Subject: Test\r\n\
         \r\n\
         This is some content for the message.\r\n\
         It will keep going.\r\n",
        frozen_rfc2822()
    );
    assert_eq!(setup.session.raw_data().unwrap(), expected.as_bytes());
}

#[test]
fn data_unstuffs_leading_double_dots() {
    let mut setup = setup_with_route();
    begin_data_transaction(&mut setup.session);
    setup.session.handle("DATA");
    setup.session.handle("Subject: Test");
    setup.session.handle("");
    setup.session.handle("..leading dot");
    let raw = String::from_utf8(setup.session.raw_data().unwrap().to_vec()).unwrap();
    assert!(raw.ends_with(".leading dot\r\n"));
}

// ------------------------------------------------------------- finished_spec

fn authenticated_setup(mail_from: &str, rcpt_to: &str) -> TestSetup {
    let mut setup = TestSetup::new();
    let credential = setup.fixtures.credential(CredentialType::Smtp, "key123");
    setup.session.handle("HELO test.example.com");
    let reply = setup
        .session
        .handle(&format!("AUTH PLAIN {}", to_smtp_plain(&credential.key)));
    assert!(line(&reply).starts_with("235 Granted for"));
    setup.session.handle(&format!("MAIL FROM: {mail_from}"));
    let reply = setup.session.handle(&format!("RCPT TO: {rcpt_to}"));
    assert_eq!(line(&reply), "250 OK");
    setup
}

#[test]
fn finish_dot_without_cr_does_nothing() {
    let mut setup = authenticated_setup("test@example.com", "test@example.com");
    setup.session.handle("DATA");
    setup.session.handle("Subject: Hello");
    setup.session.handle("\r");
    assert_eq!(setup.session.handle("."), Reply::None);
}

#[test]
fn finish_dot_after_a_bare_lf_line_does_nothing() {
    let mut setup = authenticated_setup("test@example.com", "test@example.com");
    setup.session.handle("DATA");
    setup.session.handle("Subject: Hello");
    assert_eq!(setup.session.handle(".\r"), Reply::None);
}

#[test]
fn finish_rejects_messages_over_the_maximum_size() {
    let mut small_config = config();
    small_config.max_message_size = 1;
    let mut setup = TestSetup::with_config(small_config);
    let credential = setup.fixtures.credential(CredentialType::Smtp, "key123");
    setup.session.handle("HELO test.example.com");
    setup
        .session
        .handle(&format!("AUTH PLAIN {}", to_smtp_plain(&credential.key)));
    setup.session.handle("MAIL FROM: test@example.com");
    setup.session.handle("RCPT TO: test@example.com");
    setup.session.handle("DATA");
    let big_line = "a".repeat(10 * 1024 * 1024);
    setup.session.handle(&big_line);
    setup.session.handle("\r");
    let reply = setup.session.handle(".\r");
    assert_eq!(line(&reply), "552 Message too large (maximum size 1MB)");
}

#[test]
fn finish_detects_mail_loops() {
    let mut setup = authenticated_setup("test@example.com", "test@example.com");
    setup.session.handle("DATA");
    for host in [
        "example1.com",
        "example2.com",
        "example1.com",
        "example2.com",
    ] {
        setup
            .session
            .handle(&format!("Received: from {host} by {SMTP_HOSTNAME}"));
    }
    setup.session.handle("Subject: Test");
    setup.session.handle("From: test@example.com");
    setup.session.handle("To: test@example.com");
    setup.session.handle("");
    setup.session.handle("This is a test message");
    setup.session.handle("\r");
    let reply = setup.session.handle(".\r");
    assert_eq!(line(&reply), "550 Loop detected");
}

#[test]
fn finish_rejects_unauthenticated_from_domains() {
    let mut setup = authenticated_setup("test@example.com", "test@example.com");
    // no verified domain exists for krystal.uk
    setup.session.handle("DATA");
    setup.session.handle("Subject: Test");
    setup.session.handle("From: invalid@krystal.uk");
    setup.session.handle("To: test@example.com");
    setup.session.handle("");
    setup.session.handle("This is a test message");
    setup.session.handle("\r");
    let reply = setup.session.handle(".\r");
    assert_eq!(line(&reply), "530 From/Sender name is not valid");
}

#[test]
fn finish_accepts_a_confirmed_sender_address_without_a_verified_domain() {
    let mut setup = authenticated_setup("test@example.com", "test@example.com");
    // no verified domain for solo.example, but the exact address is a
    // confirmed sender address of the server
    setup.fixtures.sender_address("sender@solo.example", true);
    setup.session.handle("DATA");
    setup.session.handle("Subject: Test");
    setup.session.handle("From: sender@solo.example");
    setup.session.handle("To: test@example.com");
    setup.session.handle("");
    setup.session.handle("This is a test message");
    setup.session.handle("\r");
    let reply = setup.session.handle(".\r");
    assert_eq!(line(&reply), "250 OK");
    // stored without an authenticated DKIM domain
    let queued = setup.sink.messages();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].domain_id, None);
}

#[test]
fn finish_rejects_a_pending_sender_address() {
    let mut setup = authenticated_setup("test@example.com", "test@example.com");
    setup.fixtures.sender_address("sender@solo.example", false);
    setup.session.handle("DATA");
    setup.session.handle("Subject: Test");
    setup.session.handle("From: sender@solo.example");
    setup.session.handle("To: test@example.com");
    setup.session.handle("");
    setup.session.handle("This is a test message");
    setup.session.handle("\r");
    let reply = setup.session.handle(".\r");
    assert_eq!(line(&reply), "530 From/Sender name is not valid");
}

#[test]
fn finish_stores_an_outgoing_message_and_resets_state() {
    let mut setup = TestSetup::new();
    setup.fixtures.verified_server_domain("example.com");
    let credential = setup.fixtures.credential(CredentialType::Smtp, "key123");
    setup.session.handle("HELO test.example.com");
    setup
        .session
        .handle(&format!("AUTH PLAIN {}", to_smtp_plain(&credential.key)));
    setup.session.handle("MAIL FROM: test@example.com");
    setup.session.handle("RCPT TO: recipient@elsewhere.com");
    setup.session.handle("DATA");
    setup.session.handle("Subject: Test");
    setup.session.handle("From: test@example.com");
    setup.session.handle("To: recipient@elsewhere.com");
    setup.session.handle("");
    setup.session.handle("This is a test message");
    setup.session.handle("\r");
    let reply = setup.session.handle(".\r");
    assert_eq!(line(&reply), "250 OK");
    assert_eq!(setup.session.state(), State::Welcomed);

    let messages = setup.sink.messages();
    assert_eq!(messages.len(), 1);
    let message = &messages[0];
    assert_eq!(message.server_id, setup.fixtures.server_id());
    assert_eq!(message.mail_from, "test@example.com");
    assert_eq!(message.rcpt_to, "recipient@elsewhere.com");
    assert_eq!(message.scope, MessageScope::Outgoing);
    assert!(!message.bounce);
    assert_eq!(message.credential_id, Some(credential.id));
    assert!(message.domain_id.is_some());
    assert_eq!(message.route_id, None);
    assert!(!message.raw_message.is_empty());
}

#[test]
fn finish_stores_a_bounce_through_the_return_path_route_when_present() {
    let mut setup = TestSetup::new();
    let domain = setup.fixtures.verified_server_domain("example.com");
    let rp_route = setup
        .fixtures
        .route("__returnpath__", Some(domain.id), RouteMode::Endpoint);
    let token = setup.fixtures.server().token.clone();
    setup.session.handle("HELO test.example.com");
    setup.session.handle("MAIL FROM: test@example.com");
    let rcpt_to = format!("{token}@{RETURN_PATH_DOMAIN}");
    setup.session.handle(&format!("RCPT TO: {rcpt_to}"));
    setup.session.handle("DATA");
    setup.session.handle("Subject: Bounce: Test");
    setup.session.handle("");
    setup.session.handle("This is a test message");
    setup.session.handle("\r");
    let reply = setup.session.handle(".\r");
    assert_eq!(line(&reply), "250 OK");

    let messages = setup.sink.messages();
    assert_eq!(messages.len(), 1);
    let message = &messages[0];
    assert_eq!(message.scope, MessageScope::Incoming);
    assert!(message.bounce);
    assert_eq!(message.route_id, Some(rp_route.id));
    assert_eq!(message.domain_id, Some(domain.id));
    assert_eq!(message.credential_id, None);
    assert_eq!(message.rcpt_to, rcpt_to);
}

#[test]
fn finish_stores_a_bounce_directly_when_no_return_path_route_exists() {
    let mut setup = TestSetup::new();
    let token = setup.fixtures.server().token.clone();
    setup.session.handle("HELO test.example.com");
    setup.session.handle("MAIL FROM: test@example.com");
    setup
        .session
        .handle(&format!("RCPT TO: {token}@{RETURN_PATH_DOMAIN}"));
    setup.session.handle("DATA");
    setup.session.handle("Subject: Bounce: Test");
    setup.session.handle("");
    setup.session.handle("Body");
    setup.session.handle("\r");
    let reply = setup.session.handle(".\r");
    assert_eq!(line(&reply), "250 OK");

    let messages = setup.sink.messages();
    assert_eq!(messages.len(), 1);
    let message = &messages[0];
    assert_eq!(message.scope, MessageScope::Incoming);
    assert!(message.bounce);
    assert_eq!(message.route_id, None);
    assert_eq!(message.domain_id, None);
}

#[test]
fn finish_stores_an_incoming_message_for_a_route() {
    let mut setup = setup_with_route();
    begin_data_transaction(&mut setup.session);
    setup.session.handle("DATA");
    setup.session.handle("Subject: Test");
    setup.session.handle("");
    setup.session.handle("Body");
    setup.session.handle("\r");
    let reply = setup.session.handle(".\r");
    assert_eq!(line(&reply), "250 OK");

    let messages = setup.sink.messages();
    assert_eq!(messages.len(), 1);
    let message = &messages[0];
    assert_eq!(message.scope, MessageScope::Incoming);
    assert!(!message.bounce);
    assert!(message.route_id.is_some());
    assert_eq!(message.rcpt_to, "info@example.com");
}

// -------------------------------------------------------------------- misc

#[test]
fn quit_closes_the_connection() {
    let mut setup = TestSetup::new();
    let reply = setup.session.handle("QUIT");
    assert_eq!(line(&reply), "221 Closing Connection");
    assert!(setup.session.finished());
}

#[test]
fn rset_resets_the_transaction() {
    let mut setup = TestSetup::new();
    setup.session.handle("HELO test.example.com");
    setup.session.handle("MAIL FROM: test@example.com");
    let reply = setup.session.handle("RSET");
    assert_eq!(line(&reply), "250 OK");
    assert_eq!(setup.session.state(), State::Welcomed);
    assert_eq!(setup.session.mail_from(), None);
}

#[test]
fn noop_returns_ok() {
    let mut setup = TestSetup::new();
    let reply = setup.session.handle("NOOP");
    assert_eq!(line(&reply), "250 OK");
}

#[test]
fn starttls_is_unavailable_when_tls_is_disabled() {
    let mut setup = TestSetup::new();
    let reply = setup.session.handle("STARTTLS");
    assert_eq!(line(&reply), "502 TLS not available");
}

#[test]
fn starttls_is_accepted_when_tls_is_enabled() {
    let mut tls_config = config();
    tls_config.tls_enabled = true;
    let mut setup = TestSetup::with_config(tls_config);
    let reply = setup.session.handle("STARTTLS");
    assert_eq!(line(&reply), "220 Ready to start TLS");
    assert!(setup.session.take_start_tls());
}

#[test]
fn unknown_commands_are_rejected() {
    let mut setup = TestSetup::new();
    let reply = setup.session.handle("WIBBLE");
    assert_eq!(line(&reply), "502 Invalid/unsupported command");
}
