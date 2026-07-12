-- Per-organization SSO (tenant-based OIDC / SAML / social sign-in),
-- configured through the dashboard. Organization configuration data, not
-- tenant message data, so no row-level security. Supplements the
-- instance-wide `oidc`, `saml` and `auth.sso_providers` config groups.

-- Email domains an organization has claimed for login routing. A domain
-- routes logins only once verified. The partial unique index below lets
-- several organizations *claim* a domain (unverified) but allows exactly
-- one to *verify* it, so no tenant can capture another's sign-ins.
CREATE TABLE organization_email_domains (
    id BIGSERIAL PRIMARY KEY,
    organization_id BIGINT NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    domain TEXT NOT NULL,
    verified BOOLEAN NOT NULL DEFAULT false,
    verification_token TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT organization_email_domains_org_domain_key UNIQUE (organization_id, domain)
);
CREATE UNIQUE INDEX idx_org_email_domains_verified_domain
    ON organization_email_domains (domain)
    WHERE verified;

-- SSO connections (OIDC / SAML / social) scoped to a single organization.
-- The protocol-specific config (issuer, client id/secret, IdP url plus
-- certificate) lives in JSONB so its shape can vary by `kind`; secrets are
-- redacted by the API layer on read.
CREATE TABLE organization_sso_connections (
    id BIGSERIAL PRIMARY KEY,
    organization_id BIGINT NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    name TEXT NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT true,
    config JSONB NOT NULL DEFAULT '{}'::jsonb,
    default_role TEXT NOT NULL DEFAULT 'member',
    auto_provision BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_org_sso_connections_org ON organization_sso_connections (organization_id);
