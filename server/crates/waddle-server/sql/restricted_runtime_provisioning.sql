-- One-time role and ownership provisioning for the restricted runtime.
--
-- This turns a database that one login role owns outright (the shape the
-- owner-as-runtime server creates: every relation in the application schema
-- owned by the application login, default PUBLIC grants on the database and
-- on the `public` schema) into the shape `waddle-server owner-phase`
-- accepts. It precedes `owner-phase`; it does not replace it.
--
-- Run it as a superuser, connected to the application database, after the
-- server pods are stopped and the old login's backends are gone. A
-- superuser is required: only one can create roles, reassign another role's
-- objects, change the database owner and change the ACL of
-- `pg_catalog.pg_control_system()`.
--
-- The procedure sets no password and creates no credential. Give the three
-- login roles their credentials separately.
--
-- It is one transaction and can be run again: a second run changes nothing.
--
-- Parameters are session settings, set before this file runs:
--
--   SET waddle.schema                   = 'public';
--   SET waddle.old_login                = 'waddle';
--   SET waddle.database_owner           = 'waddle_database_owner';
--   SET waddle.schema_owner             = 'waddle_owner';
--   SET waddle.migrator_login           = 'waddle_migrator';
--   SET waddle.runtime_login            = 'waddle_runtime';
--   SET waddle.attestor_installer_login = 'waddle_attestor_installer';
--   SET waddle.attestor_definer_role    = 'waddle_attestor';
--   SET waddle.attestor_schema          = 'waddle_attestation';
--
-- `waddle.old_login` is the login that owns the application's relations
-- today. After this procedure it owns nothing in this database and cannot
-- connect to it; it is the `old_runtime` the handoff's cutover record names.
-- `REASSIGN OWNED` moves every object that login owns in this database, in
-- any schema, and every database it owns, to the schema owner.

BEGIN;

DO $provision$
DECLARE
    app_schema      text := pg_catalog.current_setting('waddle.schema');
    old_login       text := pg_catalog.current_setting('waddle.old_login');
    database_owner  text := pg_catalog.current_setting('waddle.database_owner');
    schema_owner    text := pg_catalog.current_setting('waddle.schema_owner');
    migrator        text := pg_catalog.current_setting('waddle.migrator_login');
    runtime         text := pg_catalog.current_setting('waddle.runtime_login');
    installer       text := pg_catalog.current_setting('waddle.attestor_installer_login');
    definer         text := pg_catalog.current_setting('waddle.attestor_definer_role');
    attestor_schema text := pg_catalog.current_setting('waddle.attestor_schema');
    database        text := pg_catalog.current_database();
    role_name       text;
    default_acl     text;
BEGIN
    IF NOT (SELECT rolsuper FROM pg_catalog.pg_roles WHERE rolname = current_user) THEN
        RAISE EXCEPTION 'restricted runtime provisioning must run as a superuser';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_catalog.pg_namespace WHERE nspname = app_schema) THEN
        RAISE EXCEPTION 'application schema % does not exist in database %', app_schema, database;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = old_login) THEN
        RAISE EXCEPTION 'old login % does not exist', old_login;
    END IF;
    IF old_login IN (database_owner, schema_owner, migrator, runtime, installer, definer) THEN
        RAISE EXCEPTION 'the old login % cannot be reused as a restricted-runtime role', old_login;
    END IF;

    -- 1. Roles. The three owners cannot log in and nothing inherits from
    --    them; the three logins hold no cluster-wide attribute.
    FOREACH role_name IN ARRAY ARRAY[database_owner, schema_owner, definer] LOOP
        IF NOT EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = role_name) THEN
            EXECUTE pg_catalog.format('CREATE ROLE %I NOLOGIN', role_name);
        END IF;
        EXECUTE pg_catalog.format(
            'ALTER ROLE %I NOLOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE '
            'NOREPLICATION NOBYPASSRLS', role_name);
    END LOOP;
    FOREACH role_name IN ARRAY ARRAY[migrator, runtime, installer] LOOP
        IF NOT EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = role_name) THEN
            EXECUTE pg_catalog.format('CREATE ROLE %I LOGIN', role_name);
        END IF;
        EXECUTE pg_catalog.format(
            'ALTER ROLE %I LOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE '
            'NOREPLICATION NOBYPASSRLS', role_name);
    END LOOP;

    -- 2. The only memberships: the migrator can act as the schema owner and
    --    the installer as the attestor definer, by SET ROLE alone. The
    --    runtime login is a member of nothing.
    EXECUTE pg_catalog.format(
        'GRANT %I TO %I WITH INHERIT FALSE, SET TRUE, ADMIN FALSE', schema_owner, migrator);
    EXECUTE pg_catalog.format(
        'GRANT %I TO %I WITH INHERIT FALSE, SET TRUE, ADMIN FALSE', definer, installer);

    -- 3. Ownership. Every object of the old login moves to the NOLOGIN
    --    schema owner. The database gets an owner no login can act as, and
    --    the application schema (`public` belongs to pg_database_owner, not
    --    to the old login) is given to the schema owner explicitly.
    EXECUTE pg_catalog.format('REASSIGN OWNED BY %I TO %I', old_login, schema_owner);
    EXECUTE pg_catalog.format('ALTER DATABASE %I OWNER TO %I', database, database_owner);
    EXECUTE pg_catalog.format('ALTER SCHEMA %I OWNER TO %I', app_schema, schema_owner);

    -- 4. Database access. PUBLIC loses CONNECT and TEMPORARY, so the old
    --    login can no longer connect; the three logins connect and nothing
    --    else.
    EXECUTE pg_catalog.format('REVOKE ALL ON DATABASE %I FROM PUBLIC', database);
    EXECUTE pg_catalog.format('REVOKE ALL ON DATABASE %I FROM %I', database, old_login);
    EXECUTE pg_catalog.format(
        'GRANT CONNECT ON DATABASE %I TO %I, %I, %I', database, migrator, runtime, installer);

    -- 5. Schema access. The owner phase refuses an application schema on
    --    which PUBLIC holds anything, which the default `public` schema does.
    EXECUTE pg_catalog.format('REVOKE ALL ON SCHEMA %I FROM PUBLIC', app_schema);
    EXECUTE pg_catalog.format('REVOKE ALL ON SCHEMA %I FROM %I', app_schema, old_login);
    IF EXISTS (SELECT 1 FROM pg_catalog.pg_namespace WHERE nspname = 'public') THEN
        REVOKE ALL ON SCHEMA public FROM PUBLIC;
    END IF;

    -- 6. The attestor's own schema, owned by the definer role.
    IF NOT EXISTS (SELECT 1 FROM pg_catalog.pg_namespace WHERE nspname = attestor_schema) THEN
        EXECUTE pg_catalog.format('CREATE SCHEMA %I AUTHORIZATION %I', attestor_schema, definer);
    END IF;
    EXECUTE pg_catalog.format('ALTER SCHEMA %I OWNER TO %I', attestor_schema, definer);

    -- 7. The physical identity is read through pg_control_system(). Only the
    --    schema owner (lineage) and the attestor definer may call it; the
    --    runtime and installer logins must not, so PUBLIC loses it.
    REVOKE EXECUTE ON FUNCTION pg_catalog.pg_control_system() FROM PUBLIC;
    EXECUTE pg_catalog.format(
        'GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO %I, %I',
        schema_owner, definer);

    -- 8. Default privileges would hand the runtime future objects. None is
    --    removed here: each needs a decision, so the procedure stops.
    SELECT pg_catalog.string_agg(
               pg_catalog.format('role %s schema %s type %s: %s',
                   da.defaclrole::pg_catalog.regrole,
                   COALESCE(n.nspname, '(all)'), da.defaclobjtype, da.defaclacl), '; ')
      INTO default_acl
      FROM pg_catalog.pg_default_acl da
      LEFT JOIN pg_catalog.pg_namespace n ON n.oid = da.defaclnamespace
     WHERE da.defaclnamespace = 0 OR n.nspname = app_schema;
    IF default_acl IS NOT NULL THEN
        RAISE EXCEPTION 'default privileges must be removed before the owner phase: %', default_acl;
    END IF;
END
$provision$;

COMMIT;
