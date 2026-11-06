-- Disposable PostgreSQL test fixture only. This is not a startup migration.
-- The caller creates distinct, non-superuser claim/start roles and fences all
-- other writers. The trusted start role belongs only to the test process.
ALTER TABLE __SCHEMA__.extension_room_observation_work
    ADD COLUMN dark_snapshot JSONB NULL;

CREATE FUNCTION __SCHEMA__.observer_quarantine_fixture_guard()
RETURNS TRIGGER LANGUAGE plpgsql
SET search_path = pg_catalog, pg_temp
AS $fixture$
DECLARE
    snapshot JSONB;
BEGIN
    IF OLD.status = 'dark_started' THEN
        RAISE EXCEPTION 'fixture dark start is immutable';
    END IF;
    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    END IF;
    IF NEW.dark_snapshot IS DISTINCT FROM OLD.dark_snapshot OR NEW.status = 'dark_started' THEN
        RAISE EXCEPTION 'fixture marker can only be set by this guard';
    END IF;
    IF NEW.status = 'started' THEN
        IF current_user <> '__START_ROLE__' OR OLD.status <> 'leased'
           OR OLD.id <> '__WORK_ID__'
           OR (to_jsonb(NEW) - 'status') IS DISTINCT FROM (to_jsonb(OLD) - 'status') THEN
            RAISE EXCEPTION 'fixture start authority mismatch';
        END IF;
        SELECT jsonb_build_object(
            'version', 1,
            'work', to_jsonb(NEW) - 'dark_snapshot',
            'observer', to_jsonb(observer),
            'source', to_jsonb(source),
            'canonical', jsonb_build_object(
                'message_key', canonical.message_key::text,
                'digest_version', canonical.digest_version,
                'digest', encode(canonical.digest, 'hex')),
            'delivery', jsonb_build_object(
                'message_key', delivery.message_key::text,
                'delivery_key', delivery.delivery_key::text),
            'effect', jsonb_build_object(
                'kind', effect.kind,
                'identity_hash', encode(effect.semantic_identity_hash, 'hex'),
                'payload_version', effect.payload_version,
                'payload', encode(effect.payload, 'hex')))
        INTO snapshot
        FROM __SCHEMA__.extension_room_observers observer
        JOIN __SCHEMA__.extension_room_sources source ON source.source_key = NEW.source_key
        JOIN __SCHEMA__.ingress_messages canonical ON canonical.message_key = NEW.message_key::uuid
        JOIN __SCHEMA__.ingress_deliveries delivery ON delivery.message_key = canonical.message_key
          AND delivery.delivery_key = '__DELIVERY_KEY__'::uuid
        JOIN __SCHEMA__.ingress_effect_intents effect ON effect.message_key = canonical.message_key
          AND effect.kind = 28
          AND effect.semantic_identity_hash = sha256(convert_to(
              NEW.room_jid || '|' || NEW.plugin_id || '|' || NEW.generation::text || '|' || NEW.identity,
              'UTF8'))
        WHERE observer.plugin_id = NEW.plugin_id
          AND observer.generation = NEW.generation
          AND observer.identity = NEW.identity
          AND source.retracted = 0
          AND source.revision = NEW.revision
          AND source.source_json = NEW.source_json;
        IF snapshot IS NULL THEN
            RAISE EXCEPTION 'fixture start evidence missing';
        END IF;
        NEW.dark_snapshot := snapshot;
        NEW.status := 'dark_started';
        RETURN NEW;
    END IF;
    IF current_user = '__START_ROLE__' THEN
        RAISE EXCEPTION 'fixture start role cannot perform another work update';
    END IF;
    RETURN NEW;
END
$fixture$;
CREATE TRIGGER observer_quarantine_fixture_guard
BEFORE UPDATE OR DELETE ON __SCHEMA__.extension_room_observation_work
FOR EACH ROW EXECUTE FUNCTION __SCHEMA__.observer_quarantine_fixture_guard();

-- UPDATE column grants permit PostgreSQL row locks. These triggers turn them
-- into lock-only privileges for both fixture roles; no canonical or config
-- mutation is authorized by the pilot.
CREATE FUNCTION __SCHEMA__.observer_quarantine_fixture_lock_only()
RETURNS TRIGGER LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp
AS $fixture$
BEGIN
    IF current_user IN ('__START_ROLE__', '__CLAIM_ROLE__') THEN
        RAISE EXCEPTION 'fixture role has lock-only access';
    END IF;
    IF TG_OP = 'DELETE' THEN RETURN OLD; END IF;
    RETURN NEW;
END
$fixture$;
CREATE TRIGGER observer_fixture_lock_only BEFORE UPDATE OR DELETE
ON __SCHEMA__.ingress_protocol_epoch FOR EACH ROW EXECUTE FUNCTION __SCHEMA__.observer_quarantine_fixture_lock_only();
CREATE TRIGGER observer_fixture_lock_only BEFORE UPDATE OR DELETE
ON __SCHEMA__.ingress_messages FOR EACH ROW EXECUTE FUNCTION __SCHEMA__.observer_quarantine_fixture_lock_only();
CREATE TRIGGER observer_fixture_lock_only BEFORE UPDATE OR DELETE
ON __SCHEMA__.extension_room_observers FOR EACH ROW EXECUTE FUNCTION __SCHEMA__.observer_quarantine_fixture_lock_only();
CREATE TRIGGER observer_fixture_lock_only BEFORE UPDATE OR DELETE
ON __SCHEMA__.extension_room_sources FOR EACH ROW EXECUTE FUNCTION __SCHEMA__.observer_quarantine_fixture_lock_only();

-- Only the fixed work's real delivery binding can be inserted by the trusted
-- fixture starter. It cannot manufacture other canonical child identities.
CREATE FUNCTION __SCHEMA__.observer_quarantine_fixture_delivery_guard()
RETURNS TRIGGER LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp
AS $fixture$
BEGIN
    IF current_user <> '__START_ROLE__'
       OR NEW.message_key <> '__MESSAGE_KEY__'::uuid
       OR NEW.delivery_key <> '__DELIVERY_KEY__'::uuid THEN
        RAISE EXCEPTION 'fixture delivery binding mismatch';
    END IF;
    RETURN NEW;
END
$fixture$;
CREATE TRIGGER observer_fixture_delivery_guard BEFORE INSERT
ON __SCHEMA__.ingress_deliveries FOR EACH ROW EXECUTE FUNCTION __SCHEMA__.observer_quarantine_fixture_delivery_guard();
