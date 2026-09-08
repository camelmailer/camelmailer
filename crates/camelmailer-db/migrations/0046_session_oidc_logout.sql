-- RP-initiated OIDC logout (OpenID Connect RP-Initiated Logout 1.0).
--
-- Ending the local session leaves the identity provider's own session
-- standing, so the next sign-in silently succeeds and the user is not
-- actually logged out. Sending the browser to the provider's
-- end_session_endpoint fixes that, and needs two things the session did not
-- keep: the id_token to pass as id_token_hint, and where to send it.
--
-- Both are NULL for every session that did not come from OIDC (password,
-- 2FA, WebAuthn, SAML), and they go away with the row when the session is
-- deleted. The endpoint is resolved from discovery at login rather than at
-- logout, so signing out never waits on the provider being reachable.
--
-- The id_token is a JWT carrying the user's claims. It is stored only for
-- the life of the session, and the alternative is worse: providers such as
-- Keycloak refuse an end_session request that carries neither an
-- id_token_hint nor a client_id, so without it logout silently does nothing.

ALTER TABLE auth_sessions
    ADD COLUMN oidc_id_token TEXT,
    ADD COLUMN oidc_end_session_endpoint TEXT;
