-- Migration 2026: Structural tenant scoping (`tenant_id`), immutable `account_id`
-- UUIDs on `users` and `native_users`, and `tenant_placement_leases` table.

ALTER TABLE users ADD COLUMN account_id TEXT;
ALTER TABLE native_users ADD COLUMN account_id TEXT;

UPDATE users
SET account_id = lower(
    hex(randomblob(4)) || '-' ||
    hex(randomblob(2)) || '-4' ||
    substr(hex(randomblob(2)), 2) || '-' ||
    substr('89ab', abs(random()) % 4 + 1, 1) ||
    substr(hex(randomblob(2)), 2) || '-' ||
    hex(randomblob(6))
)
WHERE account_id IS NULL;

UPDATE native_users
SET account_id = lower(
    hex(randomblob(4)) || '-' ||
    hex(randomblob(2)) || '-4' ||
    substr(hex(randomblob(2)), 2) || '-' ||
    substr('89ab', abs(random()) % 4 + 1, 1) ||
    substr(hex(randomblob(2)), 2) || '-' ||
    hex(randomblob(6))
)
WHERE account_id IS NULL;

CREATE UNIQUE INDEX IF NOT EXISTS idx_users_account_id
    ON users (account_id) WHERE account_id IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS idx_native_users_account_id
    ON native_users (account_id) WHERE account_id IS NOT NULL;

CREATE TRIGGER IF NOT EXISTS trg_users_account_id_immutable
BEFORE UPDATE OF account_id ON users
WHEN OLD.account_id IS NOT NULL AND NEW.account_id IS NOT OLD.account_id
BEGIN
    SELECT RAISE(ABORT, 'users.account_id is immutable');
END;

CREATE TRIGGER IF NOT EXISTS trg_native_users_account_id_immutable
BEFORE UPDATE OF account_id ON native_users
WHEN OLD.account_id IS NOT NULL AND NEW.account_id IS NOT OLD.account_id
BEGIN
    SELECT RAISE(ABORT, 'native_users.account_id is immutable');
END;

ALTER TABLE users ADD COLUMN tenant_id TEXT NOT NULL DEFAULT 'default';
ALTER TABLE native_users ADD COLUMN tenant_id TEXT NOT NULL DEFAULT 'default';
ALTER TABLE auth_identities ADD COLUMN tenant_id TEXT NOT NULL DEFAULT 'default';
ALTER TABLE spaces_metadata ADD COLUMN tenant_id TEXT NOT NULL DEFAULT 'default';
ALTER TABLE extension_room_observers ADD COLUMN tenant_id TEXT NOT NULL DEFAULT 'default';

CREATE INDEX IF NOT EXISTS idx_users_tenant_localpart
    ON users (tenant_id, xmpp_localpart);
CREATE INDEX IF NOT EXISTS idx_auth_identities_tenant_issuer_subject
    ON auth_identities (tenant_id, issuer, subject);
CREATE INDEX IF NOT EXISTS idx_spaces_metadata_tenant_node
    ON spaces_metadata (tenant_id, space_node);
CREATE INDEX IF NOT EXISTS idx_extension_room_observers_tenant_plugin
    ON extension_room_observers (tenant_id, plugin_id);

CREATE TABLE IF NOT EXISTS tenant_placement_leases (
    tenant_id                 TEXT PRIMARY KEY,
    cell_role                 TEXT NOT NULL,
    placement_mode            TEXT NOT NULL CHECK (placement_mode IN ('shared_pool', 'dedicated_cell')),
    holder_spiffe_id          TEXT NOT NULL,
    holder_incarnation_pubkey BLOB NOT NULL CHECK (length(holder_incarnation_pubkey) = 32),
    holder_tls_exporter_proof BLOB NOT NULL CHECK (length(holder_tls_exporter_proof) BETWEEN 1 AND 128),
    generation                INTEGER NOT NULL CHECK (generation >= 1),
    expires_at_ms             INTEGER NOT NULL,
    transfer_state            TEXT NOT NULL CHECK (transfer_state IN ('active', 'transferring', 'draining', 'released')),
    transfer_target_json      TEXT,
    updated_at_ms             INTEGER NOT NULL
);
