-- WebAuthn / passkeys: registered credentials plus the short-lived
-- server-side ceremony state (mirroring how oidc_states holds in-flight
-- OIDC logins). Global configuration tables — not tenant data, no RLS.

-- A registered passkey. credential_id is the unpadded base64url of the
-- raw credential id (the lookup key at login); credential holds the
-- serialized webauthn-rs Passkey (public key, signature counter, backup
-- flags) — public material only, no secrets.
CREATE TABLE webauthn_credentials (
    id BIGSERIAL PRIMARY KEY,
    user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    credential_id TEXT NOT NULL UNIQUE,
    credential TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at TIMESTAMPTZ
);
CREATE INDEX idx_webauthn_credentials_user ON webauthn_credentials (user_id);

-- In-flight WebAuthn ceremonies: challenge key -> serialized
-- registration/authentication state, redeemed exactly once. Re-using a
-- key replaces the previous state (upsert).
CREATE TABLE webauthn_states (
    key TEXT PRIMARY KEY,
    user_id BIGINT REFERENCES users(id) ON DELETE CASCADE,
    state TEXT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL
);
