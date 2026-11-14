//! PROXY protocol v1 (HAProxy, "human-readable header format").
//!
//! One parser for every listener: a plain `smtp` listener and an implicit-TLS
//! `smtps` listener read the header the same way (same length cap, same
//! timeout, see [`crate::server`]) and hand the line to [`parse_v1`] through
//! the session.
//!
//! Spec: <https://www.haproxy.org/download/2.9/doc/proxy-protocol.txt>,
//! section 2.1.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// The longest PROXY protocol v1 header, `\r\n` included (spec section 2.1).
pub const V1_MAX_LENGTH: usize = 107;

/// What a valid v1 header says about the connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyHeader {
    /// `TCP4`/`TCP6`: the original client address.
    Inet { client: IpAddr },
    /// `UNKNOWN`: the proxy could not tell (a health check, or a protocol it
    /// does not relay). The spec requires the receiver to accept it and use
    /// the socket's own address.
    Unknown,
}

/// Why a header was refused. Only used for logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyError {
    /// Not of the form `PROXY <proto> ...`.
    Malformed,
    /// A protocol other than `TCP4`, `TCP6` or `UNKNOWN`.
    UnsupportedProtocol,
    /// An address that is not a literal of the announced family.
    InvalidAddress,
    /// A port that is not a decimal number in 0..=65535.
    InvalidPort,
}

impl std::fmt::Display for ProxyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Malformed => "malformed PROXY header",
            Self::UnsupportedProtocol => "unsupported PROXY protocol",
            Self::InvalidAddress => "invalid address in PROXY header",
            Self::InvalidPort => "invalid port in PROXY header",
        })
    }
}

/// Parse one PROXY v1 header line, without its `\r\n`.
///
/// `TCP4`/`TCP6` need exactly four further fields: two address literals of
/// the announced family and two ports. Anything after `UNKNOWN` is ignored,
/// as the spec requires.
pub fn parse_v1(line: &str) -> Result<ProxyHeader, ProxyError> {
    let mut parts = line.split(' ');
    if parts.next() != Some("PROXY") {
        return Err(ProxyError::Malformed);
    }
    let protocol = parts.next().ok_or(ProxyError::Malformed)?;
    if protocol == "UNKNOWN" {
        return Ok(ProxyHeader::Unknown);
    }
    let fields: Vec<&str> = parts.collect();
    let [client, proxy, client_port, proxy_port] = fields[..] else {
        return Err(ProxyError::Malformed);
    };
    // The proxy's own address is validated but not used.
    let (client, _proxy): (IpAddr, IpAddr) = match protocol {
        "TCP4" => (
            client
                .parse::<Ipv4Addr>()
                .map_err(|_| ProxyError::InvalidAddress)?
                .into(),
            proxy
                .parse::<Ipv4Addr>()
                .map_err(|_| ProxyError::InvalidAddress)?
                .into(),
        ),
        "TCP6" => (
            client
                .parse::<Ipv6Addr>()
                .map_err(|_| ProxyError::InvalidAddress)?
                .into(),
            proxy
                .parse::<Ipv6Addr>()
                .map_err(|_| ProxyError::InvalidAddress)?
                .into(),
        ),
        _ => return Err(ProxyError::UnsupportedProtocol),
    };
    for port in [client_port, proxy_port] {
        // u16::from_str accepts a leading '+'; the spec allows digits only.
        if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
            return Err(ProxyError::InvalidPort);
        }
        port.parse::<u16>().map_err(|_| ProxyError::InvalidPort)?;
    }
    Ok(ProxyHeader::Inet { client })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_tcp4_and_tcp6() {
        assert_eq!(
            parse_v1("PROXY TCP4 203.0.113.7 10.0.70.5 40000 465"),
            Ok(ProxyHeader::Inet {
                client: "203.0.113.7".parse().unwrap()
            })
        );
        assert_eq!(
            parse_v1("PROXY TCP6 2001:db8::7 2001:db8::1 40000 25"),
            Ok(ProxyHeader::Inet {
                client: "2001:db8::7".parse().unwrap()
            })
        );
    }

    #[test]
    fn accepts_unknown_with_or_without_trailing_fields() {
        assert_eq!(parse_v1("PROXY UNKNOWN"), Ok(ProxyHeader::Unknown));
        assert_eq!(
            parse_v1("PROXY UNKNOWN ffff:f...f:ffff ffff:f...f:ffff 65535 65535"),
            Ok(ProxyHeader::Unknown)
        );
    }

    #[test]
    fn refuses_a_client_field_that_is_not_an_address() {
        assert_eq!(
            parse_v1("PROXY TCP4 not-an-ip 10.0.70.5 40000 465"),
            Err(ProxyError::InvalidAddress)
        );
        assert_eq!(
            parse_v1("PROXY TCP4 203.0.113.7 garbage 40000 465"),
            Err(ProxyError::InvalidAddress)
        );
        // The family must match the protocol.
        assert_eq!(
            parse_v1("PROXY TCP4 2001:db8::7 10.0.70.5 40000 465"),
            Err(ProxyError::InvalidAddress)
        );
        assert_eq!(
            parse_v1("PROXY TCP6 203.0.113.7 2001:db8::1 40000 465"),
            Err(ProxyError::InvalidAddress)
        );
    }

    #[test]
    fn refuses_bad_ports_protocols_and_shapes() {
        assert_eq!(
            parse_v1("PROXY TCP4 203.0.113.7 10.0.70.5 70000 465"),
            Err(ProxyError::InvalidPort)
        );
        assert_eq!(
            parse_v1("PROXY TCP4 203.0.113.7 10.0.70.5 +1 465"),
            Err(ProxyError::InvalidPort)
        );
        assert_eq!(
            parse_v1("PROXY UDP4 203.0.113.7 10.0.70.5 1 465"),
            Err(ProxyError::UnsupportedProtocol)
        );
        assert_eq!(parse_v1("PROXY TCP4"), Err(ProxyError::Malformed));
        assert_eq!(
            parse_v1("PROXY TCP4 203.0.113.7  10.0.70.5 1 465"),
            Err(ProxyError::Malformed)
        );
        assert_eq!(parse_v1("EHLO client.example"), Err(ProxyError::Malformed));
        assert_eq!(parse_v1(""), Err(ProxyError::Malformed));
    }
}
