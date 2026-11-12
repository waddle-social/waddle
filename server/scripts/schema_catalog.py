#!/usr/bin/env python3
"""Canonical catalog dump and comparison for a server database.

`dump` reads the catalog of one PostgreSQL database or one SQLite file and
writes a JSON document that describes every schema object the server can
create: one entry per object, keyed by kind and name, holding the properties
that make two schemas behave differently. The document does not depend on
object creation order, OIDs, the server's collation or the connection's
settings, so two databases with the same schema produce the same bytes.

`diff` compares two such documents and prints every object that exists on one
side only and every property that differs. It exits 0 when they are equal and
1 when they are not.

`fingerprint` writes one digest per structural object of the application
schema. The server computes the same digests before it records the schema
baseline on an existing database, and refuses on any difference.

PostgreSQL is read with catalog queries through `psql` (the `--psql` option,
then `WADDLE_PSQL`, then `PATH`). SQLite is read through Python's `sqlite3`.

What the dump holds, and what it leaves out, is listed in
`docs/planning/2046-schema-baseline.md`.
"""

from __future__ import annotations

import argparse
import difflib
import json
import hashlib
import os
import re
import shutil
import sqlite3
import subprocess
import sys
import urllib.parse
from pathlib import Path
from typing import Any

FORMAT = 1

# Properties that record who owns or may use an object. `--structure-only`
# leaves them out so a schema one login owns can be compared with the same
# schema under separate owner and runtime roles.
PRIVILEGE_FIELDS = ("owner", "acl")
PRIVILEGE_KINDS = ("role", "default_acl", "database", "system_privilege")


class CatalogError(Exception):
    """The catalog could not be read or a document could not be used."""


# ---------------------------------------------------------------------------
# PostgreSQL
# ---------------------------------------------------------------------------

# Each ACL is exploded into (grantee, privilege, grantable, grantor) rows. A
# NULL ACL means the built-in default for the owner, so it is replaced by
# `acldefault` and compares equal to the same privileges granted explicitly.
_ACL = """(SELECT coalesce(jsonb_agg(jsonb_build_array(
            CASE WHEN x.grantee = 0 THEN 'PUBLIC' ELSE pg_get_userbyid(x.grantee) END,
            x.privilege_type, x.is_grantable, pg_get_userbyid(x.grantor))), '[]'::jsonb)
          FROM aclexplode(coalesce({acl}, acldefault('{kind}', {owner}))) x)"""

_COLUMN_ACL = """(SELECT coalesce(jsonb_agg(jsonb_build_array(
            CASE WHEN x.grantee = 0 THEN 'PUBLIC' ELSE pg_get_userbyid(x.grantee) END,
            x.privilege_type, x.is_grantable, pg_get_userbyid(x.grantor))), '[]'::jsonb)
          FROM aclexplode(a.attacl) x)"""


def _acl(acl: str, kind: str, owner: str) -> str:
    return _ACL.format(acl=acl, kind=kind, owner=owner)


# Objects that belong to an extension are described by the extension entry.
_NOT_EXTENSION_MEMBER = """NOT EXISTS (SELECT 1 FROM pg_depend e
            WHERE e.classid = '{catalog}'::regclass AND e.objid = {oid}
              AND e.objsubid = 0 AND e.deptype = 'e')"""


def _not_member(catalog: str, oid: str) -> str:
    return _NOT_EXTENSION_MEMBER.format(catalog=catalog, oid=oid)


_NS = "(n.nspname <> 'information_schema' AND n.nspname !~ '^pg_')"

POSTGRES_SECTIONS: dict[str, str] = {
    "schema": f"""
        SELECT n.nspname AS name, pg_get_userbyid(n.nspowner) AS owner,
               {_acl('n.nspacl', 'n', 'n.nspowner')} AS acl,
               obj_description(n.oid, 'pg_namespace') AS comment
          FROM pg_namespace n
         WHERE {_NS} AND {_not_member('pg_namespace', 'n.oid')}""",
    "relation": f"""
        SELECT n.nspname AS schema, c.relname AS name, c.relkind::text AS relkind,
               pg_get_userbyid(c.relowner) AS owner,
               c.relpersistence::text AS persistence,
               c.reloptions AS options,
               (SELECT t.reloptions FROM pg_class t WHERE t.oid = c.reltoastrelid) AS toast_options,
               c.relrowsecurity AS row_security,
               c.relforcerowsecurity AS force_row_security,
               c.relreplident::text AS replica_identity,
               CASE WHEN c.relkind = 'p' THEN pg_get_partkeydef(c.oid) END AS partition_key,
               pg_get_expr(c.relpartbound, c.oid) AS partition_bound,
               (SELECT jsonb_agg(jsonb_build_array(pn.nspname, pc.relname) ORDER BY i.inhseqno)
                  FROM pg_inherits i
                  JOIN pg_class pc ON pc.oid = i.inhparent
                  JOIN pg_namespace pn ON pn.oid = pc.relnamespace
                 WHERE i.inhrelid = c.oid) AS inherits,
               (SELECT am.amname FROM pg_am am WHERE am.oid = c.relam) AS access_method,
               (SELECT s.spcname FROM pg_tablespace s WHERE s.oid = c.reltablespace) AS tablespace,
               CASE WHEN c.relkind IN ('v', 'm') THEN pg_get_viewdef(c.oid) END AS definition,
               CASE WHEN c.relkind = 'm' THEN c.relispopulated END AS populated,
               CASE WHEN c.reloftype <> 0 THEN format_type(c.reloftype, NULL) END AS of_type,
               (SELECT coalesce(jsonb_agg(DISTINCT jsonb_build_array(rn.nspname, rc.relname)), '[]'::jsonb)
                  FROM pg_rewrite w
                  JOIN pg_depend d ON d.classid = 'pg_rewrite'::regclass AND d.objid = w.oid
                                  AND d.refclassid = 'pg_class'::regclass AND d.refobjid <> c.oid
                  JOIN pg_class rc ON rc.oid = d.refobjid
                  JOIN pg_namespace rn ON rn.oid = rc.relnamespace
                 WHERE w.ev_class = c.oid AND c.relkind IN ('v', 'm')) AS depends_on,
               {_acl('c.relacl', 'r', 'c.relowner')} AS acl,
               obj_description(c.oid, 'pg_class') AS comment
          FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
         WHERE {_NS} AND c.relkind IN ('r', 'p', 'v', 'm', 'f', 'c')
           AND {_not_member('pg_class', 'c.oid')}""",
    "column": f"""
        SELECT n.nspname AS schema, c.relname AS relation, a.attname AS name,
               row_number() OVER (PARTITION BY a.attrelid ORDER BY a.attnum) AS position,
               format_type(a.atttypid, a.atttypmod) AS type,
               a.attnotnull AS not_null,
               pg_get_expr(d.adbin, d.adrelid) AS "default",
               a.attidentity::text AS identity,
               a.attgenerated::text AS generated,
               CASE WHEN a.attcollation <> t.typcollation THEN
                    (SELECT jsonb_build_array(cn.nspname, co.collname)
                       FROM pg_collation co JOIN pg_namespace cn ON cn.oid = co.collnamespace
                      WHERE co.oid = a.attcollation) END AS collation,
               CASE WHEN a.attstorage <> t.typstorage THEN a.attstorage::text END AS storage,
               nullif(a.attcompression::text, '') AS compression,
               nullif(a.attstattarget, -1) AS statistics_target,
               a.attoptions AS options,
               a.attislocal AS local,
               a.attinhcount AS inherited_from,
               {_COLUMN_ACL} AS acl,
               col_description(a.attrelid, a.attnum) AS comment
          FROM pg_attribute a
          JOIN pg_class c ON c.oid = a.attrelid
          JOIN pg_namespace n ON n.oid = c.relnamespace
          JOIN pg_type t ON t.oid = a.atttypid
          LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
         WHERE {_NS} AND a.attnum > 0 AND NOT a.attisdropped
           AND c.relkind IN ('r', 'p', 'v', 'm', 'f', 'c')
           AND {_not_member('pg_class', 'c.oid')}""",
    "constraint": f"""
        SELECT n.nspname AS schema,
               CASE WHEN con.conrelid <> 0 THEN 'table' ELSE 'domain' END AS parent_kind,
               coalesce(c.relname, t.typname) AS parent,
               con.conname AS name, con.contype::text AS type,
               pg_get_constraintdef(con.oid) AS definition,
               con.condeferrable AS deferrable, con.condeferred AS deferred,
               con.convalidated AS validated, con.connoinherit AS no_inherit,
               con.conislocal AS local, con.coninhcount AS inherited_from,
               con.conparentid <> 0 AS partition_child,
               (SELECT ic.relname FROM pg_class ic WHERE ic.oid = con.conindid
                   AND con.contype IN ('p', 'u', 'x')) AS index,
               obj_description(con.oid, 'pg_constraint') AS comment
          FROM pg_constraint con
          JOIN pg_namespace n ON n.oid = con.connamespace
          LEFT JOIN pg_class c ON c.oid = con.conrelid
          LEFT JOIN pg_type t ON t.oid = con.contypid
         WHERE {_NS}
           AND (c.oid IS NULL OR {_not_member('pg_class', 'c.oid')})
           AND (t.oid IS NULL OR {_not_member('pg_type', 't.oid')})""",
    "index": f"""
        SELECT n.nspname AS schema, ic.relname AS name, tc.relname AS relation,
               pg_get_userbyid(ic.relowner) AS owner,
               pg_get_indexdef(i.indexrelid) AS definition,
               (SELECT am.amname FROM pg_am am WHERE am.oid = ic.relam) AS access_method,
               i.indisunique AS "unique", i.indisprimary AS "primary",
               i.indisexclusion AS exclusion, i.indimmediate AS immediate,
               i.indnullsnotdistinct AS nulls_not_distinct,
               i.indisclustered AS clustered, i.indisvalid AS valid,
               i.indisready AS ready, i.indislive AS live,
               i.indisreplident AS replica_identity,
               pg_get_expr(i.indpred, i.indrelid) AS predicate,
               ic.reloptions AS options,
               (SELECT s.spcname FROM pg_tablespace s WHERE s.oid = ic.reltablespace) AS tablespace,
               (SELECT con.conname FROM pg_constraint con
                 WHERE con.conindid = i.indexrelid AND con.contype IN ('p', 'u', 'x')) AS "constraint",
               (SELECT pc.relname FROM pg_inherits h JOIN pg_class pc ON pc.oid = h.inhparent
                 WHERE h.inhrelid = i.indexrelid) AS partition_of,
               obj_description(ic.oid, 'pg_class') AS comment
          FROM pg_index i
          JOIN pg_class ic ON ic.oid = i.indexrelid
          JOIN pg_class tc ON tc.oid = i.indrelid
          JOIN pg_namespace n ON n.oid = ic.relnamespace
         WHERE {_NS} AND {_not_member('pg_class', 'tc.oid')}""",
    "sequence": f"""
        SELECT n.nspname AS schema, c.relname AS name,
               pg_get_userbyid(c.relowner) AS owner,
               c.relpersistence::text AS persistence,
               format_type(s.seqtypid, NULL) AS type,
               s.seqstart::text AS start, s.seqincrement::text AS increment,
               s.seqmin::text AS minimum, s.seqmax::text AS maximum,
               s.seqcache::text AS cache, s.seqcycle AS cycle,
               (SELECT jsonb_build_object('schema', tn.nspname, 'relation', tc.relname,
                                          'column', a.attname, 'identity', d.deptype = 'i')
                  FROM pg_depend d
                  JOIN pg_class tc ON tc.oid = d.refobjid
                  JOIN pg_namespace tn ON tn.oid = tc.relnamespace
                  JOIN pg_attribute a ON a.attrelid = d.refobjid AND a.attnum = d.refobjsubid
                 WHERE d.classid = 'pg_class'::regclass AND d.objid = c.oid
                   AND d.refclassid = 'pg_class'::regclass AND d.deptype IN ('a', 'i')) AS owned_by,
               {_acl('c.relacl', 's', 'c.relowner')} AS acl,
               obj_description(c.oid, 'pg_class') AS comment
          FROM pg_sequence s
          JOIN pg_class c ON c.oid = s.seqrelid
          JOIN pg_namespace n ON n.oid = c.relnamespace
         WHERE {_NS} AND {_not_member('pg_class', 'c.oid')}""",
    "trigger": f"""
        SELECT n.nspname AS schema, c.relname AS relation, t.tgname AS name,
               pg_get_triggerdef(t.oid) AS definition,
               t.tgenabled::text AS enabled,
               t.tgparentid <> 0 AS partition_child,
               obj_description(t.oid, 'pg_trigger') AS comment
          FROM pg_trigger t
          JOIN pg_class c ON c.oid = t.tgrelid
          JOIN pg_namespace n ON n.oid = c.relnamespace
         WHERE {_NS} AND NOT t.tgisinternal AND {_not_member('pg_class', 'c.oid')}""",
    "function": f"""
        SELECT n.nspname AS schema, p.proname AS name,
               pg_get_function_identity_arguments(p.oid) AS identity_arguments,
               pg_get_function_arguments(p.oid) AS arguments,
               pg_get_function_result(p.oid) AS result,
               p.prokind::text AS kind,
               l.lanname AS language,
               p.prosrc AS source,
               pg_get_function_sqlbody(p.oid) AS sql_body,
               p.probin AS binary,
               p.prosecdef AS security_definer,
               p.proleakproof AS leakproof,
               p.proisstrict AS strict,
               p.proretset AS returns_set,
               p.provolatile::text AS volatility,
               p.proparallel::text AS parallel,
               p.procost::text AS cost, p.prorows::text AS rows,
               CASE WHEN p.prosupport <> 0 THEN p.prosupport::regproc::text END AS support,
               p.proconfig AS config,
               CASE WHEN p.prokind <> 'a' THEN pg_get_functiondef(p.oid) END AS definition,
               pg_get_userbyid(p.proowner) AS owner,
               {_acl('p.proacl', 'f', 'p.proowner')} AS acl,
               obj_description(p.oid, 'pg_proc') AS comment
          FROM pg_proc p
          JOIN pg_namespace n ON n.oid = p.pronamespace
          JOIN pg_language l ON l.oid = p.prolang
         WHERE {_NS} AND {_not_member('pg_proc', 'p.oid')}""",
    "type": f"""
        SELECT n.nspname AS schema, t.typname AS name, t.typtype::text AS kind,
               pg_get_userbyid(t.typowner) AS owner,
               (SELECT jsonb_agg(e.enumlabel ORDER BY e.enumsortorder)
                  FROM pg_enum e WHERE e.enumtypid = t.oid) AS labels,
               CASE WHEN t.typtype = 'd' THEN format_type(t.typbasetype, t.typtypmod) END AS base_type,
               CASE WHEN t.typtype = 'd' THEN t.typnotnull END AS not_null,
               CASE WHEN t.typtype = 'd' THEN t.typdefault END AS "default",
               CASE WHEN t.typtype = 'd' AND t.typcollation <> b.typcollation THEN
                    (SELECT jsonb_build_array(cn.nspname, co.collname)
                       FROM pg_collation co JOIN pg_namespace cn ON cn.oid = co.collnamespace
                      WHERE co.oid = t.typcollation) END AS collation,
               (SELECT jsonb_build_object(
                           'subtype', format_type(r.rngsubtype, NULL),
                           'operator_class', (SELECT oc.opcname FROM pg_opclass oc WHERE oc.oid = r.rngsubopc),
                           'canonical', CASE WHEN r.rngcanonical <> 0 THEN r.rngcanonical::regproc::text END,
                           'subtype_diff', CASE WHEN r.rngsubdiff <> 0 THEN r.rngsubdiff::regproc::text END)
                  FROM pg_range r WHERE r.rngtypid = t.oid) AS range,
               {_acl('t.typacl', 'T', 't.typowner')} AS acl,
               obj_description(t.oid, 'pg_type') AS comment
          FROM pg_type t
          JOIN pg_namespace n ON n.oid = t.typnamespace
          LEFT JOIN pg_type b ON b.oid = t.typbasetype
         WHERE {_NS} AND {_not_member('pg_type', 't.oid')}
           AND t.typtype IN ('e', 'd', 'r', 'c')
           AND (t.typtype <> 'c' OR EXISTS (SELECT 1 FROM pg_class tc
                                             WHERE tc.oid = t.typrelid AND tc.relkind = 'c'))""",
    "policy": f"""
        SELECT n.nspname AS schema, c.relname AS relation, p.polname AS name,
               p.polcmd::text AS command, p.polpermissive AS permissive,
               (SELECT coalesce(jsonb_agg(CASE WHEN r = 0 THEN 'PUBLIC' ELSE pg_get_userbyid(r) END), '[]'::jsonb)
                  FROM unnest(p.polroles) r) AS roles,
               pg_get_expr(p.polqual, p.polrelid) AS using,
               pg_get_expr(p.polwithcheck, p.polrelid) AS with_check
          FROM pg_policy p
          JOIN pg_class c ON c.oid = p.polrelid
          JOIN pg_namespace n ON n.oid = c.relnamespace
         WHERE {_NS}""",
    "rule": f"""
        SELECT n.nspname AS schema, c.relname AS relation, w.rulename AS name,
               pg_get_ruledef(w.oid) AS definition, w.ev_enabled::text AS enabled,
               obj_description(w.oid, 'pg_rewrite') AS comment
          FROM pg_rewrite w
          JOIN pg_class c ON c.oid = w.ev_class
          JOIN pg_namespace n ON n.oid = c.relnamespace
         WHERE {_NS} AND w.rulename <> '_RETURN'""",
    "statistics": f"""
        SELECT n.nspname AS schema, s.stxname AS name,
               pg_get_userbyid(s.stxowner) AS owner,
               pg_get_statisticsobjdef(s.oid) AS definition,
               s.stxstattarget AS statistics_target,
               obj_description(s.oid, 'pg_statistic_ext') AS comment
          FROM pg_statistic_ext s JOIN pg_namespace n ON n.oid = s.stxnamespace
         WHERE {_NS}""",
    "default_acl": """
        SELECT pg_get_userbyid(d.defaclrole) AS role,
               (SELECT n.nspname FROM pg_namespace n WHERE n.oid = d.defaclnamespace) AS schema,
               d.defaclobjtype::text AS object_type,
               (SELECT coalesce(jsonb_agg(jsonb_build_array(
                    CASE WHEN x.grantee = 0 THEN 'PUBLIC' ELSE pg_get_userbyid(x.grantee) END,
                    x.privilege_type, x.is_grantable, pg_get_userbyid(x.grantor))), '[]'::jsonb)
                  FROM aclexplode(d.defaclacl) x) AS acl
          FROM pg_default_acl d""",
    "extension": """
        SELECT e.extname AS name, n.nspname AS schema, e.extversion AS version,
               pg_get_userbyid(e.extowner) AS owner
          FROM pg_extension e JOIN pg_namespace n ON n.oid = e.extnamespace
         WHERE e.extname <> 'plpgsql'""",
    "event_trigger": """
        SELECT t.evtname AS name, t.evtevent AS event, t.evtfoid::regproc::text AS function,
               t.evtenabled::text AS enabled, t.evttags AS tags,
               pg_get_userbyid(t.evtowner) AS owner
          FROM pg_event_trigger t""",
    "database": f"""
        SELECT pg_get_userbyid(d.datdba) AS owner,
               {_acl('d.datacl', 'd', 'd.datdba')} AS acl,
               (SELECT coalesce(jsonb_agg(jsonb_build_array(
                           CASE WHEN s.setrole = 0 THEN NULL ELSE pg_get_userbyid(s.setrole) END,
                           s.setconfig)), '[]'::jsonb)
                  FROM pg_db_role_setting s WHERE s.setdatabase = d.oid) AS settings,
               shobj_description(d.oid, 'pg_database') AS comment
          FROM pg_database d WHERE d.datname = current_database()""",
    # Privileges on `pg_catalog` objects that differ from the ones `initdb`
    # recorded, such as EXECUTE on `pg_control_system()`. `information_schema`
    # is left out: `initdb` records no initial privileges for its views.
    "system_privilege": f"""
        SELECT 'function' AS class,
               n.nspname || '.' || p.proname || '('
                   || pg_get_function_identity_arguments(p.oid) || ')' AS name,
               pg_get_userbyid(p.proowner) AS owner,
               {_acl('p.proacl', 'f', 'p.proowner')} AS acl
          FROM pg_proc p
          JOIN pg_namespace n ON n.oid = p.pronamespace
          LEFT JOIN pg_init_privs ip ON ip.objoid = p.oid
               AND ip.classoid = 'pg_proc'::regclass AND ip.objsubid = 0
         WHERE n.nspname = 'pg_catalog' AND p.proacl::text IS DISTINCT FROM ip.initprivs::text
        UNION ALL
        SELECT 'relation', n.nspname || '.' || c.relname,
               pg_get_userbyid(c.relowner),
               {_acl('c.relacl', 'r', 'c.relowner')}
          FROM pg_class c
          JOIN pg_namespace n ON n.oid = c.relnamespace
          LEFT JOIN pg_init_privs ip ON ip.objoid = c.oid
               AND ip.classoid = 'pg_class'::regclass AND ip.objsubid = 0
         WHERE n.nspname = 'pg_catalog'
           AND c.relacl::text IS DISTINCT FROM ip.initprivs::text""",
    # Object classes the dump does not describe. They are counted so that a
    # database which uses one cannot compare equal by omission.
    "undescribed": f"""
        SELECT kind, count FROM (
            SELECT 'operator' AS kind, count(*) AS count FROM pg_operator o
              JOIN pg_namespace n ON n.oid = o.oprnamespace WHERE {_NS}
            UNION ALL SELECT 'operator class', count(*) FROM pg_opclass o
              JOIN pg_namespace n ON n.oid = o.opcnamespace WHERE {_NS}
            UNION ALL SELECT 'operator family', count(*) FROM pg_opfamily o
              JOIN pg_namespace n ON n.oid = o.opfnamespace WHERE {_NS}
            UNION ALL SELECT 'collation', count(*) FROM pg_collation o
              JOIN pg_namespace n ON n.oid = o.collnamespace WHERE {_NS}
            UNION ALL SELECT 'conversion', count(*) FROM pg_conversion o
              JOIN pg_namespace n ON n.oid = o.connamespace WHERE {_NS}
            UNION ALL SELECT 'text search configuration', count(*) FROM pg_ts_config o
              JOIN pg_namespace n ON n.oid = o.cfgnamespace WHERE {_NS}
            UNION ALL SELECT 'text search dictionary', count(*) FROM pg_ts_dict o
              JOIN pg_namespace n ON n.oid = o.dictnamespace WHERE {_NS}
            UNION ALL SELECT 'text search parser', count(*) FROM pg_ts_parser o
              JOIN pg_namespace n ON n.oid = o.prsnamespace WHERE {_NS}
            UNION ALL SELECT 'text search template', count(*) FROM pg_ts_template o
              JOIN pg_namespace n ON n.oid = o.tmplnamespace WHERE {_NS}
            UNION ALL SELECT 'base or other type', count(*) FROM pg_type o
              JOIN pg_namespace n ON n.oid = o.typnamespace
             WHERE {_NS} AND o.typtype NOT IN ('e', 'd', 'r', 'c', 'm')
               AND NOT (o.typtype = 'b' AND o.typelem <> 0 AND o.typcategory = 'A')
            UNION ALL SELECT 'cast', count(*) FROM pg_cast o WHERE o.oid >= 16384
            UNION ALL SELECT 'foreign data wrapper', count(*) FROM pg_foreign_data_wrapper
            UNION ALL SELECT 'foreign server', count(*) FROM pg_foreign_server
            UNION ALL SELECT 'publication', count(*) FROM pg_publication
            UNION ALL SELECT 'subscription', count(*) FROM pg_subscription
             WHERE subdbid = (SELECT oid FROM pg_database WHERE datname = current_database())
            UNION ALL SELECT 'procedural language', count(*) FROM pg_language o WHERE o.oid >= 16384
            UNION ALL SELECT 'access method', count(*) FROM pg_am o WHERE o.oid >= 16384
            UNION ALL SELECT 'transform', count(*) FROM pg_transform
            UNION ALL SELECT 'security label', count(*) FROM pg_seclabel
        ) counted""",
    "environment": """
        SELECT current_setting('server_version_num') AS server_version_num,
               pg_encoding_to_char(d.encoding) AS encoding,
               d.datcollate AS collate, d.datctype AS ctype,
               d.datlocprovider::text AS locale_provider
          FROM pg_database d WHERE d.datname = current_database()""",
}

# Every role an object names, and every role those are members of. A
# superuser's memberships are left out: they grant it nothing it lacks.
_ROLES_SQL = """
    WITH RECURSIVE named(name) AS (
        SELECT jsonb_array_elements_text({roles}::jsonb)
    ), closure(oid) AS (
        SELECT r.oid FROM pg_roles r JOIN named ON named.name = r.rolname
        UNION
        SELECT m.roleid FROM pg_auth_members m
          JOIN closure ON closure.oid = m.member
          JOIN pg_roles member ON member.oid = m.member
         WHERE NOT member.rolsuper
    )
    SELECT coalesce(jsonb_agg(to_jsonb(q)), '[]'::jsonb)::text FROM (
        SELECT r.rolname AS name, r.rolsuper AS superuser, r.rolinherit AS inherit,
               r.rolcreaterole AS create_role, r.rolcreatedb AS create_database,
               r.rolcanlogin AS login, r.rolreplication AS replication,
               r.rolbypassrls AS bypass_rls, r.rolconnlimit AS connection_limit,
               r.rolconfig AS config,
               (SELECT coalesce(jsonb_agg(jsonb_build_object(
                           'role', pg_get_userbyid(m.roleid),
                           'admin', m.admin_option, 'inherit', m.inherit_option,
                           'set', m.set_option, 'grantor', pg_get_userbyid(m.grantor))), '[]'::jsonb)
                  FROM pg_auth_members m
                 WHERE m.member = r.oid AND NOT r.rolsuper) AS member_of
          FROM pg_roles r JOIN closure ON closure.oid = r.oid
    ) q;
"""

_ROW_COUNT_SQL = """
    SELECT coalesce(jsonb_object_agg(q.name, q.rows), '{}'::jsonb)::text FROM (
        SELECT n.nspname || '.' || c.relname AS name,
               (xpath('/row/n/text()', query_to_xml(
                    format('SELECT count(*) AS n FROM %I.%I', n.nspname, c.relname),
                    false, true, '')))[1]::text::bigint AS rows
          FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
         WHERE (n.nspname <> 'information_schema' AND n.nspname !~ '^pg_')
           AND c.relkind IN ('r', 'p')
    ) q;
"""


# The structural sections a fingerprint covers, each with the SQL expression
# that names one of its rows within the application schema.
FINGERPRINT_SECTIONS: dict[str, str] = {
    "relation": "q.name",
    "column": "q.relation || '.' || q.name",
    "constraint": "q.parent || '.' || q.name",
    "index": "q.name",
    "sequence": "q.name",
    "trigger": "q.relation || '.' || q.name",
    "function": "q.name || '(' || q.identity_arguments || ')'",
    "type": "q.name",
    "policy": "q.relation || '.' || q.name",
    "rule": "q.relation || '.' || q.name",
    "statistics": "q.name",
}

SCHEMA_PLACEHOLDER = "__SCHEMA__"
_SIMPLE_NAME = re.compile(r"^[a-z_][a-z0-9_]*$")


def fingerprint_sql() -> str:
    """One query that returns `(key, digest)` for every structural object.

    It covers the schema that leads the search path and nothing else. Each
    digest is the SHA-256 of the object's dump row without its owner and
    ACL, with the schema's name replaced by a placeholder, so the same
    structure has the same fingerprint under any schema and any roles. The
    server runs this text to decide whether a database may be stamped, and
    the tooling runs it to record what the server should find; neither
    interprets the rows.

    The caller must have set `search_path` to the application schema alone,
    and the schema's name must be a plain lower-case identifier.
    """
    parts = []
    for section, key in FINGERPRINT_SECTIONS.items():
        parts.append(
            f"SELECT '{section} ' || {key} AS key,\n"
            "       encode(sha256(convert_to(regexp_replace(\n"
            "           (to_jsonb(q) - 'owner' - 'acl' - 'schema')::text,\n"
            f"           '\\m' || current_schema() || '\\M', '{SCHEMA_PLACEHOLDER}', 'g'), 'UTF8')), 'hex') AS digest\n"
            f"  FROM ({POSTGRES_SECTIONS[section]}) q\n"
            " WHERE q.schema = current_schema()"
        )
    parts.append(
        "SELECT 'undescribed ' || q.kind, q.count::text\n"
        f"  FROM ({POSTGRES_SECTIONS['undescribed']}) q"
    )
    return "\nUNION ALL\n".join(parts) + "\n"


def fingerprint_postgres(url: str, psql: str | None = None, schema: str = "public") -> dict[str, str]:
    """The structural fingerprint of `schema` in the database at `url`."""
    if not _SIMPLE_NAME.match(schema):
        raise CatalogError("a fingerprint needs a plain lower-case schema name")
    sql = (
        f"SET search_path = {quote_identifier(schema)};\n"
        "SELECT coalesce(jsonb_object_agg(f.key, f.digest), '{}'::jsonb)::text\n"
        f"  FROM ({fingerprint_sql()}) f;\n"
    )
    return json.loads(run_psql(find_psql(psql), url, sql))


def table_rows_postgres(url: str, psql: str, schema: str, tables: list[str]) -> dict[str, list[Any]]:
    """Every row of each named table, as JSON objects in canonical order."""
    rows = {}
    for table in tables:
        text = run_psql(
            psql, url,
            "SELECT coalesce(jsonb_agg(to_jsonb(t)), '[]'::jsonb)::text "
            f"FROM {quote_identifier(schema)}.{quote_identifier(table)} t",
        )
        rows[table] = _sorted_list(json.loads(text))
    return rows


def find_psql(explicit: str | None) -> str:
    candidate = explicit or os.environ.get("WADDLE_PSQL") or shutil.which("psql")
    if not candidate:
        raise CatalogError(
            "psql was not found: pass --psql, set WADDLE_PSQL or put psql on PATH"
        )
    return candidate


def run_psql(
    psql: str,
    url: str,
    sql: str,
    variables: dict[str, str] | None = None,
    role: str | None = None,
) -> str:
    """Run `sql` on `url` and return its unaligned, tuples-only output.

    The URL may hold a password, so it reaches the child through its
    environment, never on its command line. `PG*` variables of the caller
    are not inherited.
    """
    command = [psql, "-X", "-q", "-A", "-t", "-v", "ON_ERROR_STOP=1"]
    for key, value in (variables or {}).items():
        command += ["-v", f"{key}={value}"]
    command += ["-f", "-"]
    environment = {
        key: value for key, value in os.environ.items() if not key.startswith("PG")
    }
    environment.update(url_environment(url))
    environment["PGCLIENTENCODING"] = "UTF8"
    prelude = ""
    if role:
        prelude = f"SET ROLE {quote_identifier(role)};\n"
    completed = subprocess.run(
        command,
        input=prelude + sql,
        capture_output=True,
        text=True,
        encoding="utf-8",
        env=environment,
        check=False,
    )
    if completed.returncode != 0:
        raise CatalogError(f"psql failed:\n{completed.stderr.strip()}")
    return completed.stdout


_URL_PARAMETERS = {
    "sslmode": "PGSSLMODE",
    "sslrootcert": "PGSSLROOTCERT",
    "sslcert": "PGSSLCERT",
    "sslkey": "PGSSLKEY",
    "options": "PGOPTIONS",
    "application_name": "PGAPPNAME",
    "connect_timeout": "PGCONNECT_TIMEOUT",
}


def url_environment(url: str) -> dict[str, str]:
    """The libpq environment variables that select the database `url` names."""
    parts = urllib.parse.urlsplit(url)
    if parts.scheme not in ("postgres", "postgresql"):
        raise CatalogError("the PostgreSQL URL must start with postgres:// or postgresql://")
    environment = {}
    if parts.hostname:
        environment["PGHOST"] = urllib.parse.unquote(parts.hostname)
    if parts.port:
        environment["PGPORT"] = str(parts.port)
    if parts.username:
        environment["PGUSER"] = urllib.parse.unquote(parts.username)
    if parts.password:
        environment["PGPASSWORD"] = urllib.parse.unquote(parts.password)
    database = urllib.parse.unquote(parts.path.lstrip("/"))
    if database:
        environment["PGDATABASE"] = database
    for name, value in urllib.parse.parse_qsl(parts.query, keep_blank_values=True):
        if name == "host":
            environment["PGHOST"] = value
        elif name in _URL_PARAMETERS:
            environment[_URL_PARAMETERS[name]] = value
        else:
            raise CatalogError(f"the PostgreSQL URL parameter {name!r} is not supported")
    return environment


def quote_literal(text: str) -> str:
    """A string literal that is safe whatever `standard_conforming_strings` is."""
    if "\\" in text:
        return "E'" + text.replace("\\", "\\\\").replace("'", "''") + "'"
    return "'" + text.replace("'", "''") + "'"


def quote_identifier(name: str) -> str:
    return '"' + name.replace('"', '""') + '"'


def _postgres_document_sql(schema: str) -> str:
    sections = ",\n".join(
        f"'{name}', (SELECT coalesce(jsonb_agg(to_jsonb(q)), '[]'::jsonb) FROM ({query}) q)"
        for name, query in POSTGRES_SECTIONS.items()
    )
    # The application schema leads the search path, so the definitions the
    # catalog functions print name its objects without a schema and do not
    # change with the schema's name. `pg_catalog` is searched first
    # implicitly.
    return (
        f"SET search_path = {quote_identifier(schema)};\n"
        f"SELECT jsonb_build_object(\n{sections}\n)::text;\n"
    )


def _sorted_list(value: Any) -> Any:
    """Sort a list of JSON values by their canonical encoding."""
    if value is None:
        return None
    return sorted(value, key=lambda item: json.dumps(item, sort_keys=True))


def _alias(aliases: dict[str, str], role: Any) -> Any:
    if isinstance(role, str):
        return aliases.get(role, role)
    return role


def _alias_acl(aliases: dict[str, str], acl: list[list[Any]]) -> list[list[Any]]:
    return _sorted_list(
        [
            [_alias(aliases, grantee), privilege, grantable, _alias(aliases, grantor)]
            for grantee, privilege, grantable, grantor in acl
        ]
    )


def _qualified(row: dict[str, Any], *parts: str) -> str:
    return ".".join(str(row[part]) for part in parts)


def dump_postgres(
    url: str,
    psql: str | None = None,
    schema: str = "public",
    role_aliases: dict[str, str] | None = None,
    row_counts: bool = False,
    rows: list[str] | None = None,
) -> dict[str, Any]:
    """Read the catalog of the database at `url` into a document.

    `rows` names tables of the application schema whose contents are
    recorded too; a named table the database lacks is skipped.
    """
    psql = find_psql(psql)
    aliases = role_aliases or {}
    raw = json.loads(run_psql(psql, url, _postgres_document_sql(schema)))
    objects: dict[str, dict[str, Any]] = {}
    roles: set[str] = set()

    def put(kind: str, identity: str, row: dict[str, Any]) -> None:
        key = f"{kind} {identity}"
        if key in objects:
            raise CatalogError(f"two catalog objects share the key {key!r}")
        if "owner" in row:
            roles.add(row["owner"])
            row["owner"] = _alias(aliases, row["owner"])
        if "acl" in row:
            for grantee, _, _, grantor in row["acl"]:
                roles.update(name for name in (grantee, grantor) if name != "PUBLIC")
            row["acl"] = _alias_acl(aliases, row["acl"])
        for field in ("options", "toast_options", "config"):
            if field in row:
                row[field] = _sorted_list(row[field])
        objects[key] = row

    for row in raw["schema"]:
        put("schema", row["name"], row)
    relation_kinds = {
        "r": "table",
        "p": "table",
        "v": "view",
        "m": "materialized_view",
        "f": "foreign_table",
        "c": "composite_type",
    }
    for row in raw["relation"]:
        row["depends_on"] = _sorted_list(row["depends_on"])
        put(relation_kinds[row["relkind"]], _qualified(row, "schema", "name"), row)
    for row in raw["column"]:
        put("column", _qualified(row, "schema", "relation", "name"), row)
    for row in raw["constraint"]:
        put("constraint", _qualified(row, "schema", "parent", "name"), row)
    for row in raw["index"]:
        put("index", _qualified(row, "schema", "name"), row)
    for row in raw["sequence"]:
        put("sequence", _qualified(row, "schema", "name"), row)
    for row in raw["trigger"]:
        put("trigger", _qualified(row, "schema", "relation", "name"), row)
    for row in raw["function"]:
        identity = f"{_qualified(row, 'schema', 'name')}({row['identity_arguments']})"
        put("function", identity, row)
    for row in raw["type"]:
        put("type", _qualified(row, "schema", "name"), row)
    for row in raw["policy"]:
        roles.update(name for name in row["roles"] if name != "PUBLIC")
        row["roles"] = sorted(_alias(aliases, name) for name in row["roles"])
        put("policy", _qualified(row, "schema", "relation", "name"), row)
    for row in raw["rule"]:
        put("rule", _qualified(row, "schema", "relation", "name"), row)
    for row in raw["statistics"]:
        put("statistics", _qualified(row, "schema", "name"), row)
    for row in raw["default_acl"]:
        roles.add(row["role"])
        row["role"] = _alias(aliases, row["role"])
        identity = f"{row['role']} {row['schema'] or '*'} {row['object_type']}"
        put("default_acl", identity, row)
    for row in raw["extension"]:
        put("extension", row["name"], row)
    for row in raw["event_trigger"]:
        row["tags"] = _sorted_list(row["tags"])
        put("event_trigger", row["name"], row)
    for row in raw["database"]:
        for setting in row["settings"]:
            if setting[0] is not None:
                roles.add(setting[0])
                setting[0] = _alias(aliases, setting[0])
            setting[1] = sorted(setting[1])
        row["settings"] = _sorted_list(row["settings"])
        put("database", "current", row)
    for row in raw["system_privilege"]:
        put("system_privilege", f"{row['class']} {row['name']}", row)
    undescribed = {row["kind"]: row["count"] for row in raw["undescribed"]}
    put("undescribed", "object counts", undescribed)

    role_rows = json.loads(
        run_psql(
            psql,
            url,
            _ROLES_SQL.replace("{roles}", quote_literal(json.dumps(sorted(roles)))),
        )
    )
    for row in role_rows:
        row["config"] = _sorted_list(row["config"])
        for membership in row["member_of"]:
            membership["role"] = _alias(aliases, membership["role"])
            membership["grantor"] = _alias(aliases, membership["grantor"])
        row["member_of"] = _sorted_list(row["member_of"])
        row["name"] = _alias(aliases, row["name"])
        put("role", row["name"], row)

    document: dict[str, Any] = {
        "format": FORMAT,
        "engine": "postgresql",
        "schema": schema,
        "environment": raw["environment"][0],
        "objects": objects,
    }
    if row_counts:
        document["row_counts"] = json.loads(run_psql(psql, url, _ROW_COUNT_SQL))
    if rows is not None:
        present = [name for name in rows if f"table {schema}.{name}" in objects]
        document["rows"] = table_rows_postgres(url, psql, schema, present)
    return document


# ---------------------------------------------------------------------------
# SQLite
# ---------------------------------------------------------------------------


def normalise_sqlite_sql(sql: str | None) -> str | None:
    """Reduce stored SQLite DDL to a form that ignores layout and comments.

    SQLite keeps the text of each `CREATE` statement, and `ALTER TABLE ... ADD
    COLUMN` splices new text into it, so the same table reached two ways can
    differ in whitespace. Runs of whitespace outside quoted text collapse to
    one space, whitespace next to parentheses and commas is dropped, and
    comments are removed. Quoted strings and identifiers are kept verbatim.
    """
    if sql is None:
        return None
    out: list[str] = []
    index = 0
    length = len(sql)
    while index < length:
        char = sql[index]
        if char in "'\"`":
            end = index + 1
            while end < length:
                if sql[end] == char:
                    if end + 1 < length and sql[end + 1] == char:
                        end += 2
                        continue
                    break
                end += 1
            out.append(sql[index : end + 1])
            index = end + 1
        elif char == "[":
            end = sql.find("]", index)
            end = length - 1 if end < 0 else end
            out.append(sql[index : end + 1])
            index = end + 1
        elif sql.startswith("--", index):
            end = sql.find("\n", index)
            index = length if end < 0 else end
            out.append(" ")
        elif sql.startswith("/*", index):
            end = sql.find("*/", index + 2)
            index = length if end < 0 else end + 2
            out.append(" ")
        elif char.isspace():
            out.append(" ")
            index += 1
        else:
            out.append(char)
            index += 1
    # Collapse spaces between the tokens gathered above. Quoted text was
    # appended whole, so its inner whitespace is not touched.
    tokens: list[str] = []
    for piece in out:
        if piece == " ":
            if tokens and tokens[-1] not in (" ", "("):
                tokens.append(" ")
            continue
        if piece in "),":
            while tokens and tokens[-1] == " ":
                tokens.pop()
        tokens.append(piece)
    return "".join(tokens).strip()


def _sqlite_rows(connection: sqlite3.Connection, sql: str) -> list[sqlite3.Row]:
    return connection.execute(sql).fetchall()


def _sqlite_literal(name: str) -> str:
    return "'" + name.replace("'", "''") + "'"


def fingerprint_sqlite(path: str) -> dict[str, str]:
    """The structural fingerprint of the SQLite database file at `path`.

    One entry per stored `CREATE` statement: the SHA-256 of its text with
    layout and comments removed. The server computes the same digests.
    """
    document = dump_sqlite(path)
    return {
        key: hashlib.sha256(normalise_sqlite_sql(sql).encode("utf-8")).hexdigest()
        for key, sql in sorted(document["sources"].items())
    }


def dump_sqlite(
    path: str, row_counts: bool = False, rows: list[str] | None = None
) -> dict[str, Any]:
    """Read the schema of the SQLite database file at `path` into a document.

    `rows` names tables whose contents are recorded too, each value as the
    SQL literal `quote()` gives; a named table the database lacks is skipped.
    """
    if not Path(path).is_file():
        raise CatalogError(f"{path} is not a SQLite database file")
    connection = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
    connection.row_factory = sqlite3.Row
    try:
        document = _dump_sqlite(connection, row_counts)
        if rows is not None:
            document["rows"] = {
                name: _sqlite_table_rows(connection, name)
                for name in rows
                if f"table {name}" in document["objects"]
            }
        return document
    except sqlite3.DatabaseError as error:
        raise CatalogError(f"could not read {path}: {error}") from error
    finally:
        connection.close()


def _dump_sqlite(connection: sqlite3.Connection, row_counts: bool) -> dict[str, Any]:
    objects: dict[str, dict[str, Any]] = {}
    sources: dict[str, str] = {}
    counts: dict[str, int] = {}
    master = _sqlite_rows(
        connection,
        "SELECT type, name, tbl_name, sql FROM sqlite_master "
        "WHERE name NOT LIKE 'sqlite\\_%' ESCAPE '\\'",
    )
    table_flags = {
        row["name"]: row
        for row in _sqlite_rows(
            connection, "SELECT name, type, wr, strict FROM pragma_table_list"
        )
    }
    for entry in master:
        kind, name, sql = entry["type"], entry["name"], entry["sql"]
        literal = _sqlite_literal(name)
        if kind in ("table", "view"):
            flags = table_flags.get(name)
            key = f"{kind} {name}"
            row: dict[str, Any] = {"name": name, "sql": normalise_sqlite_sql(sql)}
            if kind == "table":
                row["without_rowid"] = bool(flags["wr"]) if flags else None
                row["strict"] = bool(flags["strict"]) if flags else None
                row["virtual"] = bool(flags and flags["type"] == "virtual")
                row["foreign_keys"] = _sqlite_foreign_keys(connection, literal)
                if row_counts and not row["virtual"]:
                    counts[name] = connection.execute(
                        f"SELECT count(*) FROM {quote_identifier(name)}"
                    ).fetchone()[0]
            objects[key] = row
            sources[key] = sql
            for column in _sqlite_rows(
                connection, f"SELECT * FROM pragma_table_xinfo({literal})"
            ):
                objects[f"column {name}.{column['name']}"] = {
                    "relation": name,
                    "name": column["name"],
                    "position": column["cid"] + 1,
                    "type": column["type"],
                    "not_null": bool(column["notnull"]),
                    "default": column["dflt_value"],
                    "primary_key_position": column["pk"],
                    # 0 ordinary, 1 hidden in a virtual table, 2 virtual
                    # generated, 3 stored generated.
                    "hidden": column["hidden"],
                }
            if kind == "table":
                for index in _sqlite_rows(
                    connection, f"SELECT * FROM pragma_index_list({literal})"
                ):
                    # Indexes that back a PRIMARY KEY or UNIQUE constraint
                    # have no row of their own in `sqlite_master`.
                    if index["origin"] == "c":
                        continue
                    objects[f"index {index['name']}"] = _sqlite_index(
                        connection, name, index, None
                    )
        elif kind == "index":
            listed = [
                index
                for index in _sqlite_rows(
                    connection,
                    f"SELECT * FROM pragma_index_list({_sqlite_literal(entry['tbl_name'])})",
                )
                if index["name"] == name
            ]
            if not listed:
                raise CatalogError(f"index {name} is not listed for its table")
            key = f"index {name}"
            objects[key] = _sqlite_index(
                connection, entry["tbl_name"], listed[0], normalise_sqlite_sql(sql)
            )
            sources[key] = sql
        elif kind == "trigger":
            key = f"trigger {name}"
            objects[key] = {
                "name": name,
                "relation": entry["tbl_name"],
                "sql": normalise_sqlite_sql(sql),
            }
            sources[key] = sql
        else:
            raise CatalogError(f"unknown sqlite_master type {kind!r} for {name}")
    objects["pragma schema"] = {
        "user_version": connection.execute("PRAGMA user_version").fetchone()[0],
        "application_id": connection.execute("PRAGMA application_id").fetchone()[0],
    }
    document: dict[str, Any] = {
        "format": FORMAT,
        "engine": "sqlite",
        "environment": {
            "sqlite_version": sqlite3.sqlite_version,
            "encoding": connection.execute("PRAGMA encoding").fetchone()[0],
            "page_size": connection.execute("PRAGMA page_size").fetchone()[0],
            "auto_vacuum": connection.execute("PRAGMA auto_vacuum").fetchone()[0],
            "journal_mode": connection.execute("PRAGMA journal_mode").fetchone()[0],
        },
        "objects": objects,
        "sources": sources,
    }
    if row_counts:
        document["row_counts"] = counts
    return document


def _sqlite_table_rows(connection: sqlite3.Connection, table: str) -> list[Any]:
    columns = [
        row["name"]
        for row in _sqlite_rows(
            connection, f"SELECT name FROM pragma_table_info({_sqlite_literal(table)})"
        )
    ]
    quoted = ", ".join(f"quote({quote_identifier(name)})" for name in columns)
    return _sorted_list(
        [
            dict(zip(columns, row))
            for row in connection.execute(f"SELECT {quoted} FROM {quote_identifier(table)}")
        ]
    )


def _sqlite_foreign_keys(
    connection: sqlite3.Connection, literal: str
) -> list[dict[str, Any]]:
    grouped: dict[int, dict[str, Any]] = {}
    for row in _sqlite_rows(
        connection, f"SELECT * FROM pragma_foreign_key_list({literal}) ORDER BY id, seq"
    ):
        key = grouped.setdefault(
            row["id"],
            {
                "references": row["table"],
                "columns": [],
                "referenced_columns": [],
                "on_update": row["on_update"],
                "on_delete": row["on_delete"],
                "match": row["match"],
            },
        )
        key["columns"].append(row["from"])
        key["referenced_columns"].append(row["to"])
    # The id is the reverse declaration order, which is not schema.
    return _sorted_list(list(grouped.values()))


def _sqlite_index(
    connection: sqlite3.Connection,
    table: str,
    index: sqlite3.Row,
    sql: str | None,
) -> dict[str, Any]:
    columns = [
        {
            "name": column["name"],
            "descending": bool(column["desc"]),
            "collation": column["coll"],
            "key": bool(column["key"]),
        }
        for column in _sqlite_rows(
            connection,
            f"SELECT * FROM pragma_index_xinfo({_sqlite_literal(index['name'])}) ORDER BY seqno",
        )
    ]
    return {
        "name": index["name"],
        "relation": table,
        "unique": bool(index["unique"]),
        # `c` CREATE INDEX, `u` UNIQUE constraint, `pk` PRIMARY KEY.
        "origin": index["origin"],
        "partial": bool(index["partial"]),
        "columns": columns,
        "sql": sql,
    }


# ---------------------------------------------------------------------------
# Documents
# ---------------------------------------------------------------------------


def render(document: dict[str, Any]) -> str:
    """The canonical bytes of a document: sorted keys, one property per line."""
    return json.dumps(document, indent=1, sort_keys=True, ensure_ascii=False) + "\n"


def load(path: str) -> dict[str, Any]:
    try:
        document = json.loads(Path(path).read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        raise CatalogError(f"could not read the catalog document {path}: {error}") from error
    if not isinstance(document, dict) or document.get("format") != FORMAT:
        raise CatalogError(f"{path} is not a format {FORMAT} catalog document")
    return document


def structure_only(document: dict[str, Any]) -> dict[str, Any]:
    """The document without ownership, privileges and roles."""
    objects = {}
    for key, row in document["objects"].items():
        if key.split(" ", 1)[0] in PRIVILEGE_KINDS:
            continue
        objects[key] = {
            field: value for field, value in row.items() if field not in PRIVILEGE_FIELDS
        }
    return {**document, "objects": objects}


# Kinds that belong to the database or the cluster, not to one schema.
_GLOBAL_KINDS = (
    "database",
    "default_acl",
    "event_trigger",
    "extension",
    "role",
    "system_privilege",
    "undescribed",
    "pragma",
)


def _role_names(row: dict[str, Any]) -> set[str]:
    names = set()
    if isinstance(row.get("owner"), str):
        names.add(row["owner"])
    for entry in row.get("acl") or []:
        names.update((entry[0], entry[3]))
    for name in row.get("roles") or []:
        names.add(name)
    if isinstance(row.get("role"), str):
        names.add(row["role"])
    for setting in row.get("settings") or []:
        if setting[0] is not None:
            names.add(setting[0])
    return names


def only_schema(document: dict[str, Any], schema: str) -> dict[str, Any]:
    """The document restricted to one PostgreSQL schema.

    Objects of other schemas are dropped. Database-wide entries stay, and a
    role stays only while a remaining object names it or a remaining role is
    a member of it.
    """
    objects = {}
    for key, row in document["objects"].items():
        kind = key.split(" ", 1)[0]
        if kind == "role":
            continue
        if kind == "schema":
            if row["name"] == schema:
                objects[key] = row
        elif kind in _GLOBAL_KINDS or row.get("schema") == schema:
            objects[key] = row
    wanted: set[str] = set()
    for row in objects.values():
        wanted |= _role_names(row)
    roles = {
        row["name"]: row
        for key, row in document["objects"].items()
        if key.startswith("role ")
    }
    pending = list(wanted)
    while pending:
        name = pending.pop()
        for membership in roles.get(name, {}).get("member_of", []):
            if membership["role"] not in wanted:
                wanted.add(membership["role"])
                pending.append(membership["role"])
    for name in sorted(wanted):
        if name in roles:
            objects[f"role {name}"] = roles[name]
    counts = document.get("row_counts")
    restricted = {**document, "objects": objects}
    if counts is not None:
        restricted["row_counts"] = {
            name: count for name, count in counts.items() if name.startswith(schema + ".")
        }
    return restricted


def _show(value: Any) -> str:
    if isinstance(value, str):
        return value
    return json.dumps(value, sort_keys=True, ensure_ascii=False)


def _property_difference(field: str, left: Any, right: Any) -> list[str]:
    left_text, right_text = _show(left), _show(right)
    if "\n" in left_text or "\n" in right_text:
        lines = [f"    {field}:"]
        lines += [
            f"      {line}"
            for line in difflib.unified_diff(
                left_text.splitlines(), right_text.splitlines(), "A", "B", lineterm=""
            )
        ]
        return lines
    if isinstance(left, list) and isinstance(right, list):
        encode = lambda item: json.dumps(item, sort_keys=True, ensure_ascii=False)
        left_items = {encode(item) for item in left}
        right_items = {encode(item) for item in right}
        if left_items != right_items:
            lines = [f"    {field}:"]
            lines += [f"      A only: {item}" for item in sorted(left_items - right_items)]
            lines += [f"      B only: {item}" for item in sorted(right_items - left_items)]
            return lines
    return [f"    {field}:", f"      A: {left_text}", f"      B: {right_text}"]


def compare(
    left: dict[str, Any],
    right: dict[str, Any],
    include_environment: bool = False,
    include_row_counts: bool = False,
    include_rows: bool = False,
) -> list[str]:
    """Every difference between two documents, as printable lines.

    An empty list means the two schemas are identical. The order of the
    lines depends only on the documents.
    """
    lines: list[str] = []
    if left.get("engine") != right.get("engine"):
        return [f"! engine: A is {left.get('engine')}, B is {right.get('engine')}"]
    left_objects, right_objects = left["objects"], right["objects"]
    for key in sorted(set(left_objects) | set(right_objects)):
        if key not in right_objects:
            lines.append(f"- {key}   (only in A)")
            continue
        if key not in left_objects:
            lines.append(f"+ {key}   (only in B)")
            continue
        left_row, right_row = left_objects[key], right_objects[key]
        if left_row == right_row:
            continue
        lines.append(f"~ {key}")
        for field in sorted(set(left_row) | set(right_row)):
            left_value, right_value = left_row.get(field), right_row.get(field)
            if left_value != right_value:
                lines += _property_difference(field, left_value, right_value)
    if include_environment and left.get("environment") != right.get("environment"):
        lines.append("~ environment")
        for field in sorted(set(left["environment"]) | set(right["environment"])):
            left_value = left["environment"].get(field)
            right_value = right["environment"].get(field)
            if left_value != right_value:
                lines += _property_difference(field, left_value, right_value)
    if include_row_counts:
        left_counts = left.get("row_counts")
        right_counts = right.get("row_counts")
        if left_counts is None or right_counts is None:
            lines.append("! row counts: a document was dumped without --row-counts")
        else:
            for name in sorted(set(left_counts) | set(right_counts)):
                if left_counts.get(name) != right_counts.get(name):
                    lines.append(
                        f"~ rows {name}: A has {left_counts.get(name)}, "
                        f"B has {right_counts.get(name)}"
                    )
    if include_rows:
        left_rows, right_rows = left.get("rows") or {}, right.get("rows") or {}
        for name in sorted(set(left_rows) | set(right_rows)):
            if left_rows.get(name) != right_rows.get(name):
                lines.append(f"~ contents {name}")
                lines += _property_difference(
                    "rows", left_rows.get(name, []), right_rows.get(name, []))
    return lines


def summarise(lines: list[str]) -> str:
    only_a = sum(1 for line in lines if line.startswith("- "))
    only_b = sum(1 for line in lines if line.startswith("+ "))
    changed = sum(1 for line in lines if line.startswith("~ "))
    if not lines:
        return "identical"
    return f"{only_a} only in A, {only_b} only in B, {changed} differ"


def parse_aliases(values: list[str]) -> dict[str, str]:
    aliases = {}
    for value in values:
        name, separator, alias = value.partition("=")
        if not separator or not name or not alias:
            raise CatalogError(f"--role-alias takes ROLE=ALIAS, not {value!r}")
        aliases[name] = alias
    return aliases


# ---------------------------------------------------------------------------
# Command line
# ---------------------------------------------------------------------------


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    commands = parser.add_subparsers(dest="command", required=True)

    dump = commands.add_parser("dump", help="write the catalog document of one database")
    source = dump.add_mutually_exclusive_group(required=True)
    source.add_argument(
        "--postgres-url-env",
        metavar="VARIABLE",
        help="environment variable that holds the PostgreSQL URL of the database",
    )
    source.add_argument("--sqlite", metavar="FILE", help="SQLite database file")
    dump.add_argument("--psql", help="psql executable (default: WADDLE_PSQL, then PATH)")
    dump.add_argument(
        "--schema",
        default="public",
        help="PostgreSQL application schema; definitions name its objects unqualified",
    )
    dump.add_argument(
        "--role-alias",
        action="append",
        default=[],
        metavar="ROLE=ALIAS",
        help="record ROLE as ALIAS wherever it owns, grants or is granted (repeatable)",
    )
    dump.add_argument(
        "--row-counts",
        action="store_true",
        help="also record how many rows each table holds",
    )
    dump.add_argument(
        "--rows",
        action="append",
        metavar="TABLE",
        help="also record the contents of TABLE (repeatable)",
    )
    dump.add_argument("--output", "-o", help="file to write (default: standard output)")

    fingerprint = commands.add_parser(
        "fingerprint", help="write the structural fingerprint the server's stamp checks")
    source = fingerprint.add_mutually_exclusive_group(required=True)
    source.add_argument("--postgres-url-env", metavar="VARIABLE",
                        help="environment variable that holds the PostgreSQL URL")
    source.add_argument("--sqlite", metavar="FILE", help="SQLite database file")
    fingerprint.add_argument("--psql", help="psql executable (default: WADDLE_PSQL, then PATH)")
    fingerprint.add_argument("--schema", default="public", help="PostgreSQL application schema")
    fingerprint.add_argument(
        "--expected", metavar="FILE",
        help="compare with this fingerprint and exit 1 on any difference")
    fingerprint.add_argument("--output", "-o", help="file to write (default: standard output)")

    diff = commands.add_parser("diff", help="compare two catalog documents")
    diff.add_argument("a", metavar="A")
    diff.add_argument("b", metavar="B")
    diff.add_argument(
        "--structure-only",
        action="store_true",
        help="ignore owners, privileges, default privileges, roles and the database entry",
    )
    diff.add_argument(
        "--only-schema",
        metavar="SCHEMA",
        help="compare one PostgreSQL schema and the database-wide entries only",
    )
    diff.add_argument(
        "--environment",
        action="store_true",
        help="also compare server version, encoding and locale",
    )
    diff.add_argument(
        "--row-counts", action="store_true", help="also compare the recorded row counts"
    )
    diff.add_argument(
        "--rows", action="store_true", help="also compare the recorded table contents"
    )
    return parser


def fingerprint_differences(expected: dict[str, str], found: dict[str, str]) -> list[str]:
    lines = []
    for key in sorted(set(expected) | set(found)):
        if key not in found:
            lines.append(f"- {key}   (missing)")
        elif key not in expected:
            lines.append(f"+ {key}   (not expected)")
        elif expected[key] != found[key]:
            lines.append(f"~ {key}")
    return lines


def render_fingerprint(fingerprint: dict[str, str]) -> str:
    return json.dumps(fingerprint, indent=1, sort_keys=True, ensure_ascii=False) + "\n"


def _fingerprint_command(arguments: argparse.Namespace) -> int:
    if arguments.sqlite:
        found = fingerprint_sqlite(arguments.sqlite)
    else:
        url = os.environ.get(arguments.postgres_url_env)
        if not url:
            raise CatalogError(f"{arguments.postgres_url_env} is not set")
        found = fingerprint_postgres(url, arguments.psql, arguments.schema)
    if arguments.output:
        Path(arguments.output).write_text(render_fingerprint(found), encoding="utf-8")
    elif not arguments.expected:
        sys.stdout.write(render_fingerprint(found))
    if not arguments.expected:
        return 0
    try:
        expected = json.loads(Path(arguments.expected).read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        raise CatalogError(f"could not read {arguments.expected}: {error}") from error
    lines = fingerprint_differences(expected, found)
    for line in lines:
        print(line)
    print(f"fingerprint: {len(lines)} difference(s)", file=sys.stderr)
    return 1 if lines else 0


def main(argv: list[str] | None = None) -> int:
    arguments = _parser().parse_args(argv)
    try:
        if arguments.command == "dump":
            if arguments.sqlite:
                document = dump_sqlite(arguments.sqlite, arguments.row_counts, arguments.rows)
            else:
                url = os.environ.get(arguments.postgres_url_env)
                if not url:
                    raise CatalogError(f"{arguments.postgres_url_env} is not set")
                document = dump_postgres(
                    url,
                    arguments.psql,
                    arguments.schema,
                    parse_aliases(arguments.role_alias),
                    arguments.row_counts,
                    arguments.rows,
                )
            text = render(document)
            if arguments.output:
                Path(arguments.output).write_text(text, encoding="utf-8")
            else:
                sys.stdout.write(text)
            return 0
        if arguments.command == "fingerprint":
            return _fingerprint_command(arguments)
        left, right = load(arguments.a), load(arguments.b)
        if arguments.only_schema:
            left = only_schema(left, arguments.only_schema)
            right = only_schema(right, arguments.only_schema)
        if arguments.structure_only:
            left, right = structure_only(left), structure_only(right)
        lines = compare(
            left, right, arguments.environment, arguments.row_counts, arguments.rows)
        for line in lines:
            print(line)
        print(f"result: {summarise(lines)}", file=sys.stderr)
        return 1 if lines else 0
    except CatalogError as error:
        print(f"error: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
