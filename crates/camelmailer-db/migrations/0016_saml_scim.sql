-- SAML single sign-on state and SCIM provisioning support.
--
-- user_auth.disabled: administrative deactivation (SCIM `active: false`).
-- A disabled account cannot log in (password, OIDC or SAML) and cannot
-- complete a password reset.
ALTER TABLE user_auth ADD COLUMN disabled BOOLEAN NOT NULL DEFAULT FALSE;

-- In-flight SAML logins: the AuthnRequest id a response must reference
-- via InResponseTo, redeemed exactly once by the ACS endpoint.
CREATE TABLE saml_requests (
    request_id TEXT PRIMARY KEY,
    expires_at TIMESTAMPTZ NOT NULL
);

-- Replay cache of consumed SAML assertion ids, kept until the
-- assertion's own NotOnOrAfter.
CREATE TABLE saml_assertions (
    assertion_id TEXT PRIMARY KEY,
    expires_at TIMESTAMPTZ NOT NULL
);
