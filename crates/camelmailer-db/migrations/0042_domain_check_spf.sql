-- Per-domain SPF opt-out, mirroring check_dmarc (0041). When false, the
-- domain health check reports the SPF row as "ignored" and excludes it from
-- the overall grade — for domains whose SPF is managed externally (an
-- existing multi-provider record the owner will not extend with our include).
ALTER TABLE domains ADD COLUMN check_spf BOOLEAN NOT NULL DEFAULT TRUE;
