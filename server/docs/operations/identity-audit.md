# Read-only identity audit

`waddle-identity-audit` produces local diagnostic evidence for B6 in
[the federation plan](../../../docs/planning/federation-2047.md). It does not
change accounts, run migrations, issue grants, contact SpiceDB, enable
federation, or enforce a runtime profile. A complete report cannot establish
that a deployment is safe to enroll.

## Run against an explicitly selected source

Build from `server/`:

```sh
nix develop --command cargo build --locked -p waddle-server --bin waddle-identity-audit
```

Provide the connection URL through `WADDLE_IDENTITY_AUDIT_DATABASE_URL` in the
process environment. Do not put credentials in command arguments or shell
history. For an existing local test database, a non-secret example is:

```sh
export WADDLE_IDENTITY_AUDIT_DATABASE_URL='sqlite:///absolute/path/accounts.sqlite'
target/debug/waddle-identity-audit \
  --driver sqlite \
  --oidc-home-domain home.example.test \
  --report /private/operator/directory/identity-audit.json
```

Use `--driver postgres` for a PostgreSQL connection URL supplied through the
same environment variable. Production Korora domains remain undecided; all
examples here use isolated test domains. The OIDC home domain is a required
**operator assertion**, because `users` has no authoritative domain column.
The report records that assertion and diagnoses unsupported domain spellings,
even for empty tables. It does not authenticate domain ownership.

The report path must be new and in an operator-controlled private directory.
The executable creates a temporary file with owner-only permissions, verifies
its regular-file type, permission bits and link count before writing identifiers,
serializes and syncs it, then publishes without replacing or following an
existing destination. Unsupported permission platforms fail. Failed reads,
limits and malformed corpora publish no final report; an existing destination
is left unchanged. A crash may leave a private temporary artifact, which must
not be interpreted as a completed report. Reports contain sensitive original
and prepared identifiers; do not paste them into general logs, issues or CI.

Stdout contains status and counts; stderr contains only static error codes.
SQLx statement logging is disabled. Exit codes are:

| Code | Meaning |
| --- | --- |
| 0 | Both sources were read completely, with no findings under these diagnostic rules. |
| 1 | Both sources were read completely, with findings. |
| 2 | Incomplete: configuration, connection, schema, read, limit, timeout, corpus or report failure. |

None of these statuses is an authorization, enrollment or migration decision.

## Source and snapshot coverage

The executable reads only these identity columns:

| Store | Original evidence | Existing prepared binding |
| --- | --- | --- |
| `users` | `jid`, `xmpp_localpart`, asserted OIDC domain | `localpart_key` |
| `native_users` | `id`, `username`, `domain` | `jid_key` |

PostgreSQL uses the fixed `public` schema, verifies ordinary non-inherited base
tables and supported column types, and reads both sources in one
`REPEATABLE READ, READ ONLY` transaction. `row_security=off` makes reads fail
when row-level security would filter the role's result; it does not grant a
bypass. Missing SELECT permissions or RLS filtering are incomplete results.
Partitioned tables and tables with inheritance parents or children are rejected;
otherwise a foreign descendant could escape the database snapshot.
Before catalog validation, bounded `ACCESS SHARE` locks pin both account
relations against rename/replacement. Reads use `ONLY` to exclude inheritance
descendants added after validation. Search path is restricted to `pg_catalog`;
queries qualify account tables. A lock or schema race is incomplete.

SQLite uses the fixed `main` schema, requires both supported base tables and
column types, checks actual value types, and opens an existing file with
read-only/create-disabled/query-only options. A single read transaction covers
schema and both account sources. URI query options are rejected: immutable
mode or a non-locking VFS could invalidate snapshot claims. It does not change
journal mode. Existing WAL/shared-memory coordination may still involve
filesystem metadata; this is account/schema read-only access, not a claim of
zero filesystem activity. Embedded NUL bytes remain visible in the original
evidence instead of being truncated by SQLite text slicing.

Absent tables/columns, views, foreign/virtual tables, unsupported types, NULL
required identity values, decode errors, interrupted reads and exceeded limits
are incomplete. Missing prepared keys are findings in a complete report. No
source is silently skipped or auto-created.

The snapshot proves neither future uniqueness nor exclusion of concurrent or
mixed writers. It covers these two stores in one database only: separate
independent deployments, sessions, roster/archive references, external identity
providers and SpiceDB are outside the inventory. A clean snapshot cannot
establish historical identity ownership or provide a migration binding.

## Observed identity paths and candidate grammar

Each account row retains original evidence and separately reports:

- `current_account_prepared_jid`: `NodePart`/`DomainPart` plus `BareJid::from_parts`,
  matching the current account directory/backfill preparation path.
- `current_stanza_parsed_jid`: whole `BareJid` parsing, matching the JID parsing
  path exercised by typed stanzas.
- The stored lookup key, compared with the expected account-preparation result.

Collision groups name their basis: `account_preparation`, `stanza_parsing` or
`recorded_binding`. These bases are not treated as equivalent. Recorded OIDC
localpart keys are paired with the asserted domain; native keys are retained
as recorded. Groups reference report-local account indexes and never merge
rows or choose an oldest winner.

Pinned `jid` 0.12.2 demonstrates why the separation matters: sharp-s can become
`ss`; account component preparation removes a trailing domain dot, while a
whole JID parse can preserve it on its borrowed fast path. U-label and A-label
spellings can remain different. The report flags preparation-path disagreement
and retains all original values. It does not invent an equivalence rule.

`candidate-ascii-account-v1` is a diagnostic candidate only:

- Localpart: 1–64 bytes, lowercase ASCII letters, digits, `.`, `_` and `-`.
- Domain: at most 253 bytes with at least one dot; labels are 1–63 lowercase
  ASCII letters/digits/hyphens, with no leading/trailing hyphen or empty label.
- No trailing dot, Unicode, uppercase transformation or `xn--` A-label.

This does not implement full RFC 7622/PRECIS/IDNA conformance. It does not
validate remote ownership or grant eligibility. Full Unicode policy,
normalization and audited migration remain blocked.

## Optional raw-boundary corpus

`--frames /private/operator/directory/frames.json` accepts a JSON array of XML
stanza strings in a regular file. Each IQ/message/presence is decoded into an
Element; the audit captures top-level XML-decoded `from`/`to` attributes before
the same Element-to-typed-stanza conversion used by the live parser. Reports
separate `original_xml_decoded` from `current_parser_jid`, so numeric character
references cannot hide non-ASCII input. Absent attributes are explicit nulls.
The live parser's wire behavior and runtime stanza metadata are unchanged.

Malformed XML, rejected typed stanzas, non-stanza roots or any invalid corpus
entry make the operation incomplete. Service/resource addresses are diagnosed
as outside this account-profile candidate. The corpus does not cover SASL,
nested payload identities, relay/resumption or a future S2S admission boundary.

Limits are enforced before retaining an unbounded account inventory:

| Input | Bound |
| --- | --- |
| Account rows across both sources | `--max-rows`, default 10,000; maximum 100,000; one excess row detects truncation |
| Each original identity field | 4,096 bytes; SQL projections are bounded before driver allocation |
| Retained original identity bytes | 16 MiB |
| Corpus | 4 MiB; 1,000 frames; live parser's 1 MiB per-frame limit |
| Database connection/snapshot deadline | `--timeout-seconds`, default 30; maximum 300 |

PostgreSQL also has a 30-second statement timeout and 5-second lock timeout.
The database deadline does not promise a hard deadline for bounded local
analysis or filesystem writes/fsync; use a responsive local report filesystem.

## Native verification

From `server/`, use the repository toolchain:

```sh
nix develop --command cargo test --locked -p waddle-xmpp --lib protocol::frame
nix develop --command cargo test --locked -p waddle-server --bin waddle-identity-audit
nix develop --command cargo test --locked -p waddle-server --test identity_audit_cli
```

The PostgreSQL test requires native `initdb` and `postgres` on PATH and is
explicitly selected because those binaries are not in the default development
shell:

```sh
nix develop --command cargo test --locked -p waddle-server \
  --bin waddle-identity-audit native_postgres_snapshot_read_only_and_non_bypass_rls \
  -- --ignored
```

It creates its own cluster and socket inside a TempDir, disables TCP listening,
and never reads an external database URL. It proves stable snapshot visibility,
read-only write rejection, and failure under a non-bypass RLS role whose normal
SELECT would silently hide account rows. Cleanup targets only the spawned
process and its owned directory. A skipped native test is not a passing RLS
verification.
