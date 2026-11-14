-- Outbound address family per IP pool, and the reverse-DNS check of the
-- worker's source addresses.
--
-- ip_pools.address_family overrides smtp_client.address_family for the
-- servers and streams that send from the pool. NULL keeps the behaviour of
-- every pool so far: the pool's IPv4 address is bound and only IPv4 remote
-- addresses are tried. Set, it also binds the address's ipv6 value, which
-- until now was stored for reference only.
--
-- source_address_checks holds the latest result per address: the workers
-- check, at start and every smtp_client.reverse_dns_check_interval seconds,
-- that each address they may send from has a PTR record whose name
-- resolves back to it (forward-confirmed reverse DNS). origin is 'pool' for
-- an IP pool address and 'host' for the address a worker host sends from
-- when a server has no pool. Rows are written by the workers and read by
-- GET /api/v2/admin/source_addresses.

ALTER TABLE ip_pools ADD COLUMN address_family TEXT
    CHECK (address_family IN ('dual_stack', 'prefer_ipv4', 'ipv4_only', 'ipv6_only'));

CREATE TABLE source_address_checks (
    address TEXT PRIMARY KEY,
    family TEXT NOT NULL CHECK (family IN ('ipv4', 'ipv6')),
    origin TEXT NOT NULL CHECK (origin IN ('host', 'pool')),
    status TEXT NOT NULL
        CHECK (status IN ('pass', 'no_ptr', 'mismatch', 'lookup_failed', 'not_public')),
    ptr TEXT[] NOT NULL DEFAULT '{}',
    detail TEXT NOT NULL DEFAULT '',
    checked_by TEXT NOT NULL,
    checked_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
