-- Whether accounts at other servers may reach a channel's room over
-- server-to-server streams (ADR-0020 D10): discover it, enter it and read
-- its archive, each still subject to the room's own admission.
--
-- The column is the durable copy of the room configuration's `federated`
-- setting, read when a room actor is rebuilt from the catalog. Every row
-- that exists when this runs is not federated: an owner turns the setting
-- on per room, and a deployment may create new rooms with it on.
ALTER TABLE channels ADD COLUMN federated integer DEFAULT 0 NOT NULL;
