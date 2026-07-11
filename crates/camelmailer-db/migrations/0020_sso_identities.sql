-- Social SSO account links (auth.sso_providers). Each account may link at
-- most one identity per provider; a (provider, subject) pair belongs to at
-- most one account. Independent of user_auth.oidc_sub, which stays the
-- link for the single enterprise `oidc` group. Global configuration data,
-- not tenant data — no RLS.
CREATE TABLE sso_identities (
    provider TEXT NOT NULL,
    subject TEXT NOT NULL,
    user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (provider, subject),
    CONSTRAINT sso_identities_provider_user_key UNIQUE (provider, user_id)
);
