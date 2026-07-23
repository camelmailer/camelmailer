-- Per-domain DMARC opt-out. When false, the domain health check reports the
-- DMARC row as "ignored" and excludes it from the overall grade — for
-- domains whose DNS (and DMARC policy) is managed externally, where a
-- "missing" DMARC record is noise rather than a problem.
ALTER TABLE domains ADD COLUMN check_dmarc BOOLEAN NOT NULL DEFAULT TRUE;
