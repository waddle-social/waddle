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
-- It is one transaction. Every check runs before the first change, so a
-- refusal alters nothing. A second run changes nothing.
--
-- Roles. A role named below that does not exist is created without a
-- password. A role that already exists is never altered: it is accepted
-- only if it already has the shape this procedure creates, and refused,
-- naming the difference, otherwise. A pre-existing login may have a
-- password, so the three logins can be created with their credentials
-- beforehand (for example by the cluster operator) as
-- `LOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION
-- NOBYPASSRLS`, with no membership and no `ALTER ROLE ... SET` setting. A
-- role that is a superuser, has REPLICATION or BYPASSRLS, is the session's
-- own role, or carries a system name is always refused.
--
-- Parameters are session settings, set before this file runs. The seven
-- role names must differ from one another, and so must the two schemas:
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
    this_database   text := pg_catalog.current_database();
    owner_roles     text[] := ARRAY[database_owner, schema_owner, definer];
    login_roles     text[] := ARRAY[migrator, runtime, installer];
    role_name       text;
    existing        record;
    detail          text;
BEGIN
    ---------------------------------------------------------------------
    -- Checks. Nothing above the "Changes" line below alters anything.
    ---------------------------------------------------------------------
    IF NOT (SELECT rolsuper FROM pg_catalog.pg_roles WHERE rolname = current_user) THEN
        RAISE EXCEPTION 'restricted runtime provisioning must run as a superuser';
    END IF;
    IF (SELECT pg_catalog.count(DISTINCT name)
          FROM pg_catalog.unnest(ARRAY[old_login] || owner_roles || login_roles) AS name) <> 7 THEN
        RAISE EXCEPTION 'the seven role parameters must name seven different roles; nothing was changed';
    END IF;
    IF app_schema = attestor_schema THEN
        RAISE EXCEPTION 'the application schema and the attestor schema must differ; nothing was changed';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_catalog.pg_namespace WHERE nspname = app_schema) THEN
        RAISE EXCEPTION 'application schema % does not exist in database %', app_schema, this_database;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = old_login) THEN
        RAISE EXCEPTION 'old login % does not exist', old_login;
    END IF;

    -- The old login must be gone from this database: a live backend or a
    -- prepared transaction of it could still write as the owner it was.
    SELECT pg_catalog.string_agg(activity.pid::text, ', ') INTO detail
      FROM pg_catalog.pg_stat_activity activity
     WHERE activity.datname = this_database AND activity.usename = old_login
       AND activity.pid <> pg_catalog.pg_backend_pid();
    IF detail IS NOT NULL THEN
        RAISE EXCEPTION 'the old login % still has backends in this database (pid %); nothing was changed',
            old_login, detail;
    END IF;
    SELECT pg_catalog.string_agg(prepared.gid, ', ') INTO detail
      FROM pg_catalog.pg_prepared_xacts prepared
     WHERE prepared.database = this_database AND prepared.owner = old_login;
    IF detail IS NOT NULL THEN
        RAISE EXCEPTION 'the old login % still has prepared transactions in this database (%); nothing was changed',
            old_login, detail;
    END IF;

    -- Default privileges would hand future objects to other roles, and
    -- REASSIGN OWNED leaves the old login's default privileges in place.
    -- None is removed here: each needs a decision.
    SELECT pg_catalog.string_agg(
               pg_catalog.format('role %s schema %s type %s: %s',
                   pg_catalog.pg_get_userbyid(da.defaclrole),
                   COALESCE(n.nspname, '(all)'), da.defaclobjtype, da.defaclacl), '; ')
      INTO detail
      FROM pg_catalog.pg_default_acl da
      LEFT JOIN pg_catalog.pg_namespace n ON n.oid = da.defaclnamespace
     WHERE da.defaclnamespace = 0
        OR n.nspname IN (app_schema, attestor_schema)
        OR pg_catalog.pg_get_userbyid(da.defaclrole) = old_login;
    IF detail IS NOT NULL THEN
        RAISE EXCEPTION 'default privileges must be removed first (%); nothing was changed', detail;
    END IF;

    -- An attestor schema that already exists must already be the definer's.
    SELECT pg_catalog.pg_get_userbyid(nspowner) INTO detail
      FROM pg_catalog.pg_namespace WHERE nspname = attestor_schema;
    IF detail IS NOT NULL AND detail <> definer THEN
        RAISE EXCEPTION 'attestor schema % already exists and belongs to %, not to %; nothing was changed',
            attestor_schema, detail, definer;
    END IF;

    -- Roles this procedure would create. An existing one is never altered.
    FOREACH role_name IN ARRAY owner_roles || login_roles LOOP
        IF role_name LIKE 'pg\_%'
           OR role_name IN (current_user, session_user,
                            'postgres', 'streaming_replica', 'cnpg_pooler_pgbouncer') THEN
            RAISE EXCEPTION 'role % is a system role or this session''s own role and cannot be a restricted-runtime role; nothing was changed',
                role_name;
        END IF;
        SELECT * INTO existing FROM pg_catalog.pg_authid WHERE rolname = role_name;
        IF NOT FOUND THEN
            CONTINUE;
        END IF;
        IF existing.rolsuper OR existing.rolreplication OR existing.rolbypassrls THEN
            RAISE EXCEPTION 'role % already exists with SUPERUSER, REPLICATION or BYPASSRLS and will not be altered; nothing was changed',
                role_name;
        END IF;
        detail := pg_catalog.concat_ws(', ',
            CASE WHEN role_name = ANY (login_roles) AND NOT existing.rolcanlogin THEN 'it is NOLOGIN' END,
            CASE WHEN role_name = ANY (owner_roles) AND existing.rolcanlogin THEN 'it is LOGIN' END,
            CASE WHEN role_name = ANY (owner_roles) AND existing.rolpassword IS NOT NULL
                 THEN 'it has a password' END,
            CASE WHEN existing.rolinherit THEN 'it is INHERIT' END,
            CASE WHEN existing.rolcreatedb THEN 'it is CREATEDB' END,
            CASE WHEN existing.rolcreaterole THEN 'it is CREATEROLE' END,
            CASE WHEN EXISTS (SELECT 1 FROM pg_catalog.pg_db_role_setting s
                               WHERE s.setrole = existing.oid)
                 THEN 'it has ALTER ROLE ... SET settings' END,
            (SELECT pg_catalog.string_agg(
                        pg_catalog.format('%s is a member of %s',
                            pg_catalog.pg_get_userbyid(m.member),
                            pg_catalog.pg_get_userbyid(m.roleid)), ', ')
               FROM pg_catalog.pg_auth_members m
              WHERE existing.oid IN (m.roleid, m.member)
                AND NOT (m.set_option AND NOT m.inherit_option AND NOT m.admin_option
                         AND (pg_catalog.pg_get_userbyid(m.roleid),
                              pg_catalog.pg_get_userbyid(m.member))
                             IN ((schema_owner, migrator), (definer, installer)))));
        IF detail <> '' THEN
            RAISE EXCEPTION 'role % already exists and does not have the shape this procedure creates: %; nothing was changed',
                role_name, detail;
        END IF;
    END LOOP;

    ---------------------------------------------------------------------
    -- Changes.
    ---------------------------------------------------------------------

    -- 1. Roles. The three owners cannot log in and nothing inherits from
    --    them; the three logins hold no cluster-wide attribute.
    FOREACH role_name IN ARRAY owner_roles LOOP
        IF NOT EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = role_name) THEN
            EXECUTE pg_catalog.format(
                'CREATE ROLE %I NOLOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE '
                'NOREPLICATION NOBYPASSRLS', role_name);
        END IF;
    END LOOP;
    FOREACH role_name IN ARRAY login_roles LOOP
        IF NOT EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = role_name) THEN
            EXECUTE pg_catalog.format(
                'CREATE ROLE %I LOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE '
                'NOREPLICATION NOBYPASSRLS', role_name);
        END IF;
    END LOOP;

    -- 2. The only memberships: the migrator can act as the schema owner and
    --    the installer as the attestor definer, by SET ROLE alone. The
    --    runtime login is a member of nothing.
    IF NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members m
                    WHERE pg_catalog.pg_get_userbyid(m.roleid) = schema_owner
                      AND pg_catalog.pg_get_userbyid(m.member) = migrator) THEN
        EXECUTE pg_catalog.format(
            'GRANT %I TO %I WITH INHERIT FALSE, SET TRUE, ADMIN FALSE', schema_owner, migrator);
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members m
                    WHERE pg_catalog.pg_get_userbyid(m.roleid) = definer
                      AND pg_catalog.pg_get_userbyid(m.member) = installer) THEN
        EXECUTE pg_catalog.format(
            'GRANT %I TO %I WITH INHERIT FALSE, SET TRUE, ADMIN FALSE', definer, installer);
    END IF;

    -- 3. Ownership. Every object of the old login moves to the NOLOGIN
    --    schema owner. The database gets an owner no login can act as, and
    --    the application schema (`public` belongs to pg_database_owner, not
    --    to the old login) is given to the schema owner explicitly.
    EXECUTE pg_catalog.format('REASSIGN OWNED BY %I TO %I', old_login, schema_owner);
    EXECUTE pg_catalog.format('ALTER DATABASE %I OWNER TO %I', this_database, database_owner);
    EXECUTE pg_catalog.format('ALTER SCHEMA %I OWNER TO %I', app_schema, schema_owner);

    -- 4. Database access. PUBLIC loses CONNECT and TEMPORARY on this
    --    database, so the old login can no longer connect to it; the three
    --    logins connect and nothing else.
    EXECUTE pg_catalog.format('REVOKE ALL ON DATABASE %I FROM PUBLIC', this_database);
    EXECUTE pg_catalog.format('REVOKE ALL ON DATABASE %I FROM %I', this_database, old_login);
    EXECUTE pg_catalog.format(
        'GRANT CONNECT ON DATABASE %I TO %I, %I, %I', this_database, migrator, runtime, installer);

    -- 5. Schema access. The owner phase refuses an application schema on
    --    which PUBLIC holds anything, which the default `public` schema
    --    does. No other schema is touched.
    EXECUTE pg_catalog.format('REVOKE ALL ON SCHEMA %I FROM PUBLIC', app_schema);
    EXECUTE pg_catalog.format('REVOKE ALL ON SCHEMA %I FROM %I', app_schema, old_login);

    -- 6. The attestor's own schema, owned by the definer role.
    IF NOT EXISTS (SELECT 1 FROM pg_catalog.pg_namespace WHERE nspname = attestor_schema) THEN
        EXECUTE pg_catalog.format('CREATE SCHEMA %I AUTHORIZATION %I', attestor_schema, definer);
    END IF;

    -- 7. The physical identity is read through pg_control_system(). Only the
    --    schema owner (lineage) and the attestor definer may call it; the
    --    runtime and installer logins must not, so PUBLIC loses it. Function
    --    ACLs are per database: no other database is affected.
    REVOKE EXECUTE ON FUNCTION pg_catalog.pg_control_system() FROM PUBLIC;
    EXECUTE pg_catalog.format(
        'GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO %I, %I',
        schema_owner, definer);
END
$provision$;

COMMIT;
