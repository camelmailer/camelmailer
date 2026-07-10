-- Inbound message management: a `bypassed` flag records that an incoming
-- message was re-queued with block rules bypassed (Postmark's "bypass rules"
-- inbound action). Retry re-queues without setting it.

ALTER TABLE messages ADD COLUMN bypassed BOOLEAN NOT NULL DEFAULT FALSE;
