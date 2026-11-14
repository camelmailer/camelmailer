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
//!
//! A message whose pool yields no usable address (the pool is empty, its
//! address is not an IPv4 address, or the lookup failed) is sent as before
//! address families: from the operating system's address, every IPv4
//! remote first. It never falls back to `dual_stack`, which would send from
//! the host's IPv6 address, the one most likely to lack reverse DNS.

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
        return match store.pool_source_for(server_id, stream_id, None).await {
            Ok(None) => OutboundSource::system(sender.address_family()),
            Ok(Some(_)) => {
                tracing::warn!(
                    server_id,
                    "the IP pool has no usable address; sending from the host's address, IPv4 first"
                );
                OutboundSource::system(pool_fallback_family(sender.address_family()))
            }
            Err(error) => {
                tracing::warn!(%error, server_id, "could not read the IP pool; sending from the host's address, IPv4 first");
                OutboundSource::system(pool_fallback_family(sender.address_family()))
            }
        };
    };
    let IpAddr::V4(ipv4) = source_ip else {
        return OutboundSource::bound(source_ip);
    };
    let pool = match store
        .pool_source_for(server_id, stream_id, Some(&ipv4.to_string()))
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

/// The family of a message that has (or may have) a pool but no usable pool
/// address: the order from before address families (`prefer_ipv4`) instead
/// of `dual_stack`; an installation restricted to one family stays on it.
fn pool_fallback_family(installation: AddressFamily) -> AddressFamily {
    match installation {
        AddressFamily::DualStack => AddressFamily::PreferIpv4,
        other => other,
    }
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
                if first_warning(value) {
                    tracing::warn!(%ipv4, value, "the ipv6 value of an IP pool address is not an IPv6 address; it is not bound");
                }
                None
            }
        });
    OutboundSource::pool(family, Some(ipv4), ipv6)
}

/// Whether `value` is warned about for the first time in this process, so
/// an unusable pool value is logged once and not on every delivery (the
/// reverse-DNS check repeats it at each run).
fn first_warning(value: &str) -> bool {
    static WARNED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    WARNED
        .get_or_init(Default::default)
        .lock()
        .map(|mut warned| warned.insert(value.to_string()))
        .unwrap_or(true)
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

    #[test]
    fn an_unusable_value_is_warned_about_once() {
        assert!(first_warning("not-an-ipv6-once"));
        assert!(!first_warning("not-an-ipv6-once"));
        assert!(first_warning("another-bad-value"));
    }

    #[tokio::test]
    async fn a_pool_without_a_usable_address_never_falls_back_to_dual_stack() {
        use camelmailer_core::{AdminStore, NewIpAddress};

        let fixtures = camelmailer_core::testing::Fixtures::new();
        let store = fixtures.store();
        let server_id = fixtures.server_id();
        let sender = SmtpSender::new(&camelmailer_config::Config::default());
        assert_eq!(sender.address_family(), AddressFamily::DualStack);

        // An empty pool: the worker finds no address (source_ip None).
        let pool = store.create_ip_pool("empty", false).await.unwrap();
        store
            .set_server_ip_pool(server_id, Some(pool.id))
            .await
            .unwrap();
        let source = source_for(&*store, &sender, server_id, None, None).await;
        assert_eq!(source, OutboundSource::system(AddressFamily::PreferIpv4));

        // A pool address that is not an IPv4 address never parses into
        // source_ip either; the same fallback applies.
        store
            .create_ip_address(NewIpAddress {
                ip_pool_id: pool.id,
                ipv4: "not-an-ip".into(),
                ipv6: Some("2001:db8::10".into()),
                hostname: "out.example".into(),
                priority: 10,
            })
            .await
            .unwrap();
        let source = source_for(&*store, &sender, server_id, None, None).await;
        assert_eq!(source, OutboundSource::system(AddressFamily::PreferIpv4));

        // An installation restricted to one family stays on it.
        let mut config = camelmailer_config::Config::default();
        config.smtp_client.address_family = AddressFamily::Ipv4Only;
        let sender = SmtpSender::new(&config);
        let source = source_for(&*store, &sender, server_id, None, None).await;
        assert_eq!(source, OutboundSource::system(AddressFamily::Ipv4Only));
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
