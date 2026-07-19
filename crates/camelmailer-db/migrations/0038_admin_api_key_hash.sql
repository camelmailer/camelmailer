-- Admin API keys are now stored hashed (SHA-256 hex), mirroring session and
-- invitation tokens: a database leak no longer yields working full-access
-- keys. The plaintext is shown exactly once at creation and never persisted.
--
-- There are no real admin API keys to preserve yet, so this is a clean break:
-- any existing rows are dropped rather than migrated (they cannot be hashed
-- retroactively without the plaintext). Operators regenerate keys via
-- `make-admin-api-key` or the dashboard.
TRUNCATE admin_api_keys;

ALTER TABLE admin_api_keys DROP COLUMN key;
-- SHA-256 hex of the key; the only form ever stored.
ALTER TABLE admin_api_keys ADD COLUMN key_hash TEXT NOT NULL UNIQUE;
-- First few characters of the plaintext, kept for dashboard display only.
ALTER TABLE admin_api_keys ADD COLUMN key_prefix TEXT NOT NULL;
