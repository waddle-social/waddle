-- The offline store's recipients by their lower-case spelling, so that the
-- retirement of an account (ADR-0020 D8) finds the rows kept under another
-- spelling of its JID by an index range rather than by reading every
-- recipient of the store (`auth/retirement.rs`, `delete_offline_spellings`).
--
-- SQLite compares the range's text bytewise, as the index orders it.
CREATE INDEX idx_pending_delivery_recipient_lower ON pending_delivery (lower(recipient_jid));
