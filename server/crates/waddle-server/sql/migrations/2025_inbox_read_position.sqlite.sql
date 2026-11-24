-- Chat-marker read position of an inbox conversation (XEP-0430 "Unread
-- Messages", XEP-0333 "Business Rules").
--
-- A displayed or acknowledged marker reads the marked message and every
-- message before it, never later ones, and markers only move forward. The
-- column holds the archive position (`archive_seq`) of the latest message a
-- marker has read through, so an older or repeated marker is ignored. NULL
-- means no marker has advanced the conversation yet.
ALTER TABLE inbox_entries ADD COLUMN read_ordinal INTEGER;
