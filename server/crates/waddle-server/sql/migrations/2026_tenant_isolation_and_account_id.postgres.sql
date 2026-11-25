-- Migration 2026: Structural tenant scoping (`tenant_id`), immutable `account_id`
-- UUIDs on `users` and `native_users`, tenant-scoped composite unique keys, and
-- `tenant_placement_leases` table.

ALTER TABLE users ADD COLUMN IF NOT EXISTS account_id TEXT;
ALTER TABLE native_users ADD COLUMN IF NOT EXISTS account_id TEXT;

UPDATE users SET account_id = gen_random_uuid()::text WHERE account_id IS NULL;
UPDATE native_users SET account_id = gen_random_uuid()::text WHERE account_id IS NULL;

CREATE UNIQUE INDEX IF NOT EXISTS idx_users_account_id
    ON users (account_id) WHERE account_id IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS idx_native_users_account_id
    ON native_users (account_id) WHERE account_id IS NOT NULL;

CREATE OR REPLACE FUNCTION reject_immutable_account_id_change() RETURNS TRIGGER AS $$
BEGIN
    IF OLD.account_id IS NOT NULL AND NEW.account_id IS DISTINCT FROM OLD.account_id THEN
        RAISE EXCEPTION 'account_id is immutable';
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trg_users_account_id_immutable ON users;
CREATE TRIGGER trg_users_account_id_immutable
    BEFORE UPDATE OF account_id ON users
    FOR EACH ROW EXECUTE FUNCTION reject_immutable_account_id_change();

DROP TRIGGER IF EXISTS trg_native_users_account_id_immutable ON native_users;
CREATE TRIGGER trg_native_users_account_id_immutable
    BEFORE UPDATE OF account_id ON native_users
    FOR EACH ROW EXECUTE FUNCTION reject_immutable_account_id_change();

ALTER TABLE users ADD COLUMN IF NOT EXISTS tenant_id TEXT NOT NULL DEFAULT 'default';
ALTER TABLE native_users ADD COLUMN IF NOT EXISTS tenant_id TEXT NOT NULL DEFAULT 'default';
ALTER TABLE auth_identities ADD COLUMN IF NOT EXISTS tenant_id TEXT NOT NULL DEFAULT 'default';
ALTER TABLE spaces_metadata ADD COLUMN IF NOT EXISTS tenant_id TEXT NOT NULL DEFAULT 'default';
ALTER TABLE extension_room_observers ADD COLUMN IF NOT EXISTS tenant_id TEXT NOT NULL DEFAULT 'default';

ALTER TABLE users DROP CONSTRAINT IF EXISTS users_username_key;
ALTER TABLE users DROP CONSTRAINT IF EXISTS users_xmpp_localpart_key;
CREATE UNIQUE INDEX IF NOT EXISTS idx_users_tenant_username
    ON users (tenant_id, username);
CREATE UNIQUE INDEX IF NOT EXISTS idx_users_tenant_xmpp_localpart
    ON users (tenant_id, xmpp_localpart);

ALTER TABLE auth_identities DROP CONSTRAINT IF EXISTS auth_identities_issuer_subject_key;
CREATE UNIQUE INDEX IF NOT EXISTS idx_auth_identities_tenant_issuer_subject
    ON auth_identities (tenant_id, issuer, subject);

CREATE UNIQUE INDEX IF NOT EXISTS idx_spaces_metadata_tenant_space_node
    ON spaces_metadata (tenant_id, space_node);

ALTER TABLE extension_room_observers DROP CONSTRAINT IF EXISTS extension_room_observers_pkey;
ALTER TABLE extension_room_observers
    ADD CONSTRAINT extension_room_observers_pkey PRIMARY KEY (tenant_id, plugin_id);

CREATE TABLE IF NOT EXISTS tenant_placement_leases (
    tenant_id                 TEXT PRIMARY KEY,
    cell_role                 TEXT NOT NULL,
    placement_mode            TEXT NOT NULL CHECK (placement_mode IN ('shared_pool', 'dedicated_cell')),
    holder_spiffe_id          TEXT NOT NULL,
    holder_incarnation_pubkey BYTEA NOT NULL CHECK (octet_length(holder_incarnation_pubkey) = 32),
    holder_tls_exporter_proof BYTEA NOT NULL CHECK (octet_length(holder_tls_exporter_proof) BETWEEN 1 AND 128),
    generation                BIGINT NOT NULL CHECK (generation >= 1),
    expires_at                TIMESTAMPTZ NOT NULL,
    transfer_state            TEXT NOT NULL CHECK (transfer_state IN ('active', 'transferring', 'draining', 'released')),
    transfer_target_json      JSONB,
    updated_at                TIMESTAMPTZ NOT NULL DEFAULT now()
);
