-- User accounts, login sessions, organization RBAC memberships,
-- invitations, password resets, OIDC login state and the auth audit log.
-- All of these are global configuration tables (not tenant data), so none
-- are RLS-protected.

-- Per-user authentication state. Kept in its own table so account secrets
-- never travel with the users row used by profile/admin queries.
CREATE TABLE user_auth (
    user_id BIGINT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    password_digest TEXT,
    totp_secret TEXT,
    totp_enabled BOOLEAN NOT NULL DEFAULT FALSE,
    failed_login_attempts INTEGER NOT NULL DEFAULT 0,
    locked_until TIMESTAMPTZ,
    last_login_at TIMESTAMPTZ,
    oidc_sub TEXT UNIQUE,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Login sessions. Only the SHA-256 of the bearer token is stored.
CREATE TABLE auth_sessions (
    id BIGSERIAL PRIMARY KEY,
    user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL,
    last_used_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    ip_address TEXT,
    user_agent TEXT
);
CREATE INDEX idx_auth_sessions_user ON auth_sessions (user_id);

-- Organization-level RBAC: viewer < member < admin < owner.
CREATE TABLE organization_memberships (
    id BIGSERIAL PRIMARY KEY,
    organization_id BIGINT NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role TEXT NOT NULL CHECK (role IN ('viewer', 'member', 'admin', 'owner')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (organization_id, user_id)
);
CREATE INDEX idx_memberships_user ON organization_memberships (user_id);

CREATE TABLE invitations (
    id BIGSERIAL PRIMARY KEY,
    uuid TEXT NOT NULL UNIQUE,
    organization_id BIGINT NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    email_address TEXT NOT NULL,
    role TEXT NOT NULL CHECK (role IN ('viewer', 'member', 'admin', 'owner')),
    token_hash TEXT NOT NULL UNIQUE,
    invited_by_user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    expires_at TIMESTAMPTZ NOT NULL,
    accepted_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_invitations_org ON invitations (organization_id);
-- one *pending* invitation per address per organization
CREATE UNIQUE INDEX idx_invitations_pending
    ON invitations (organization_id, lower(email_address))
    WHERE accepted_at IS NULL;

CREATE TABLE password_resets (
    id BIGSERIAL PRIMARY KEY,
    user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- In-flight OIDC logins: authorization-request state -> PKCE verifier +
-- nonce, redeemed exactly once by the callback.
CREATE TABLE oidc_states (
    state TEXT PRIMARY KEY,
    pkce_verifier TEXT NOT NULL,
    nonce TEXT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL
);

CREATE TABLE auth_events (
    id BIGSERIAL PRIMARY KEY,
    user_id BIGINT REFERENCES users(id) ON DELETE SET NULL,
    email_address TEXT,
    event TEXT NOT NULL,
    ip_address TEXT,
    user_agent TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_auth_events_created ON auth_events (created_at DESC);
