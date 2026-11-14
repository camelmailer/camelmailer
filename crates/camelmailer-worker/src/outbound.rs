//! Which address families and local addresses a message is sent from.
//!
//! | The message's pool | Families tried | Local address |
//! |---|---|---|
//! | none | `smtp_client.address_family` | the operating system's choice |
//! | a pool without `address_family` | IPv4 only | the pool address's `ipv4` |
//! | a pool with `address_family` | the pool's `address_family` | the pool address's `ipv4` for IPv4 remotes, its `ipv6` for IPv6 remotes |
//!
//! A pool address without an `ipv6` value never sends over IPv6, whatever
//! the family: the bound address pins the family.

use crate::sender::SmtpSender;
use crate::smtp_client::{AddressFamily, OutboundSource};
use camelmailer_core::{Id, ServerStore};
use std::net::{IpAddr, Ipv6Addr};

/// The [`OutboundSource`] of a message on `stream_id` of `server_id`.
/// `source_ip` is the pool address the worker chose (its `ipv4` value), or
/// `None` when the message has no pool.
pub async fn source_for<S: ServerStore + ?Sized>(
    store: &S,
    sender: &SmtpSender,
    server_id: Id,
    stream_id: Option<Id>,
    source_ip: Option<IpAddr>,
) -> OutboundSource {
    let Some(source_ip) = source_ip else {
        return OutboundSource::system(sender.address_family());
    };
    let IpAddr::V4(ipv4) = source_ip else {
        return OutboundSource::bound(source_ip);
    };
    let pool = match store
        .pool_source_for(server_id, stream_id, &ipv4.to_string())
        .await
    {
        Ok(pool) => pool,
        Err(error) => {
            tracing::warn!(%error, %ipv4, "could not read the IP pool's address family; sending from its IPv4 address");
            None
        }
    };
    pool_source(ipv4, pool)
}

/// The source for the pool address `ipv4`, given what the store knows about
/// its pool (see the module documentation).
pub fn pool_source(
    ipv4: std::net::Ipv4Addr,
    pool: Option<camelmailer_core::PoolSource>,
) -> OutboundSource {
    let Some(pool) = pool else {
        return OutboundSource::bound(IpAddr::V4(ipv4));
    };
    let family = match pool
        .address_family
        .as_deref()
        .map(str::parse::<AddressFamily>)
    {
        None => return OutboundSource::bound(IpAddr::V4(ipv4)),
        Some(Ok(family)) => family,
        Some(Err(error)) => {
            tracing::warn!(%error, %ipv4, "IP pool has an unknown address family; sending from its IPv4 address");
            return OutboundSource::bound(IpAddr::V4(ipv4));
        }
    };
    let ipv6 = pool
        .ipv6
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|value| match value.parse::<Ipv6Addr>() {
            Ok(ipv6) => Some(ipv6),
            Err(_) => {
                tracing::warn!(%ipv4, value, "the ipv6 value of an IP pool address is not an IPv6 address; it is not bound");
                None
            }
        });
    OutboundSource::pool(family, Some(ipv4), ipv6)
}

#[cfg(test)]
mod tests {
    use super::*;
    use camelmailer_core::PoolSource;

    const V4: std::net::Ipv4Addr = std::net::Ipv4Addr::new(203, 0, 113, 10);

    fn pool(family: Option<&str>, ipv6: Option<&str>) -> Option<PoolSource> {
        Some(PoolSource {
            address_family: family.map(String::from),
            ipv6: ipv6.map(String::from),
        })
    }

    #[test]
    fn a_pool_without_a_family_binds_its_ipv4_only() {
        // as every pool before address families: the ipv6 value is not used
        let source = pool_source(V4, pool(None, Some("2001:db8::10")));
        assert_eq!(source, OutboundSource::bound(IpAddr::V4(V4)));
        assert_eq!(source.family, AddressFamily::Ipv4Only);
        assert_eq!(source.ipv6, None);
        // an address the store does not find is bound the same way
        assert_eq!(pool_source(V4, None), OutboundSource::bound(IpAddr::V4(V4)));
    }

    #[test]
    fn a_pool_with_a_family_binds_both_values() {
        for family in AddressFamily::ALL {
            let source = pool_source(V4, pool(Some(family.as_str()), Some("2001:db8::10")));
            assert_eq!(source.family, family);
            assert_eq!(source.ipv4, Some(V4));
            assert_eq!(source.ipv6, Some("2001:db8::10".parse().unwrap()));
            assert!(source.pinned);
        }
    }

    #[test]
    fn an_unusable_ipv6_value_or_family_falls_back_safely() {
        let source = pool_source(V4, pool(Some("dual_stack"), Some("not-an-ip")));
        assert_eq!(source.family, AddressFamily::DualStack);
        assert_eq!(source.ipv6, None, "never bound, so IPv4 only in effect");
        let source = pool_source(V4, pool(Some("ipv4_first"), Some("2001:db8::10")));
        assert_eq!(source, OutboundSource::bound(IpAddr::V4(V4)));
    }

    #[tokio::test]
    async fn without_a_pool_the_installation_family_applies() {
        let store = camelmailer_core::MemoryStore::new();
        let mut config = camelmailer_config::Config::default();
        config.smtp_client.address_family = AddressFamily::PreferIpv4;
        let sender = SmtpSender::new(&config);
        let source = source_for(&store, &sender, 1, None, None).await;
        assert_eq!(source, OutboundSource::system(AddressFamily::PreferIpv4));
    }
}
