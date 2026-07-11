-- Org-wide two-factor enforcement (Postmark-style): while the flag is on,
-- users without an active second factor (TOTP or a registered passkey)
-- may not access the organization's resources via a user session. Admin
-- API keys are unaffected; global admins are enforced like everyone else.
ALTER TABLE organizations
    ADD COLUMN require_two_factor BOOLEAN NOT NULL DEFAULT FALSE;
