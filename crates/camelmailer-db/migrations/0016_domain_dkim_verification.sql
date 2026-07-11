-- Per-domain DKIM keys and DNS-based domain verification.
--
-- dkim_private_key: the domain's own RSA signing key (PEM). NULL means
-- the domain signs with the installation key (camelmailer.signing_key_path);
-- that fallback stays valid forever, so existing domains keep working.
--
-- verification_token: a stable random token published as a TXT record at
-- `_camelmailer-challenge.<domain>` to prove ownership. Backfilled for
-- existing rows so the column can be NOT NULL.

ALTER TABLE domains ADD COLUMN dkim_private_key TEXT;
ALTER TABLE domains ADD COLUMN verification_token TEXT;

UPDATE domains
   SET verification_token = md5(random()::text || clock_timestamp()::text || id::text);

ALTER TABLE domains ALTER COLUMN verification_token SET NOT NULL;
